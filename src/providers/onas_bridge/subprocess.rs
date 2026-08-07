//! onas 子进程调用的底层实现：定位可执行文件、拼参数、跑进程、读管道、超时熔断。
//!
//! 接口细节已核实自 onas 源码（v0.2.0，cli.rs / image.rs / video.rs）：
//! - `onas image <input> <output>`：整文件格式转换。输出格式由 `<output>`
//!   的扩展名决定；onas 内部把输入完整解码成一份 RGBA8，再按目标格式编码写盘。
//!   没有"只解码不编码"的模式，也不支持输出到 stdout——每次调用都会
//!   实实在在往磁盘写一个完整的文件，这是 onas 当前 CLI 唯一支持的形态。
//! - `onas frame <input> <output> [--at SECONDS]`：从视频中解出单帧，编码成
//!   图片写盘（v0.2.0 新增子命令，之前版本没有）。不传 `--at` 时按 onas 侧
//!   实现取默认帧（起始位置），本文件固定不传，只要"随便一帧能当缩略图"，
//!   不需要指定具体时间点。
//! - 错误处理：onas 的 `main()` 返回 `anyhow::Result<()>`，任何失败都让进程
//!   以非零 exit code 退出（v0.2.0 起区分了具体错误类型，见 onas 的
//!   `exitcode` 模块），并把完整错误链（"Error: ...\n\nCaused by:\n  0: ..."）
//!   打到 stderr。这里目前仍只把 stderr 整体当人类可读文本展示给用户，
//!   不区分退出码——Rooq 只关心"成功还是失败"，具体是哪类失败对用户提示
//!   来说不需要区分，暂不解析 exit code。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OnasBridgeError {
    #[error(
        "找不到 onas 可执行文件（尝试过: {tried}）。\
         请确认 onas.exe 与 rooq.exe 放在同一目录，\
         或设置 ROOQ_ONAS 环境变量指向它。"
    )]
    BinaryNotFound { tried: String },
    #[error("启动 onas 子进程失败: {0}")]
    SpawnFailed(std::io::Error),
    #[error("等待 onas 子进程退出失败: {0}")]
    WaitFailed(std::io::Error),
    #[error("onas 处理超时（超过 {0:?}），已终止子进程")]
    Timeout(Duration),
    #[error("onas 报告失败:\n{stderr}")]
    OnasFailed { stderr: String },
}

/// 单次调用的超时上限。webp/avif 静态图转换正常应是毫秒到百毫秒级，
/// 10 秒不是期望耗时，只是给"onas 本身卡死/环境异常"这类意外情况一个
/// 兜底熔断点，避免 UI 线程被无限期阻塞（当前 window.rs 里所有 provider
/// 调用仍是同步的，见 core/window.rs 顶部注释）。
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// `onas frame` 的超时上限，比图片转换更宽松。取首帧本身很快，
/// 但视频解码器（尤其 H.265/VP9/AV1）的初始化开销比 webp/avif 静态图
/// 解码更重，且不排除用户预览的是体积异常大的 mkv/webm，给更充裕的
/// 熔断阈值避免"文件稍大就误判为卡死"的假阳性。
const FRAME_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// 定位 onas 可执行文件，按优先级依次尝试：
/// 1. `ROOQ_ONAS` 环境变量（显式覆盖，调试/自定义部署路径用）
/// 2. 与 rooq.exe 同目录下的 onas.exe（推荐部署方式：两个二进制放一起）
/// 3. PATH 环境变量里能找到的 `onas`（兜底，要求用户自己把 onas 加进 PATH）
fn locate_onas() -> Result<PathBuf, OnasBridgeError> {
    let mut tried = Vec::new();

    if let Ok(p) = std::env::var("ROOQ_ONAS") {
        let path = PathBuf::from(&p);
        let exists = path.is_file();
        tried.push(p);
        if exists {
            return Ok(path);
        }
    }

    let exe_name = if cfg!(windows) { "onas.exe" } else { "onas" };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(exe_name);
            tried.push(candidate.display().to_string());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(sep) {
            let candidate = Path::new(dir).join(exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    tried.push(format!("PATH 中的 {exe_name}"));

    Err(OnasBridgeError::BinaryNotFound {
        tried: tried.join(", "),
    })
}

/// 生成一个本次调用独占的临时输出文件路径（pid + 自增计数器，保证并发/连续
/// 调用不会互相覆盖）。这是"用后即删"的一次性中转文件，不是可复用缓存——
/// 调用方（onas_bridge::convert_image_to_png）读完 RGBA 数据后立即删除，
/// 磁盘上不留痕迹，和 providers/pdf.rs 里"纯内存缓存、不落盘"的既定原则
/// 尽量保持一致。这里的落盘本身无法避免：onas 的 CLI 只支持写文件、
/// 不支持 stdout，但落盘的生命周期被严格收紧到"这一次转换调用期间"。
pub(super) fn temp_output_path(target_ext: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mut path = std::env::temp_dir();
    path.push(format!("rooq_onas_{pid}_{n}.{target_ext}"));
    path
}

/// 调用 `onas image <input> <output>`，同步阻塞直到完成、超时、或失败。
/// 成功时 `output` 路径上已经写好了转换结果，调用方负责读取和清理。
pub(super) fn run_onas_image_convert(input: &Path, output: &Path) -> Result<(), OnasBridgeError> {
    run_onas(
        &["image", &input.display().to_string(), &output.display().to_string()],
        CALL_TIMEOUT,
    )
}

/// 调用 `onas frame <input> <output>`，同步阻塞直到完成、超时、或失败。
/// 成功时 `output` 路径上已经写好了提取出的单帧图片。不传 `--at`：
/// 缩略图场景只要"随便一帧"即可，不需要指定具体时间点，onas 侧默认取
/// 起始位置的帧就够用——省掉了"该取第几秒"这个本来就没有好答案的决定。
pub(super) fn run_onas_frame_extract(input: &Path, output: &Path) -> Result<(), OnasBridgeError> {
    run_onas(
        &["frame", &input.display().to_string(), &output.display().to_string()],
        FRAME_CALL_TIMEOUT,
    )
}

/// 共享的子进程调用逻辑：定位可执行文件、跑进程、排空管道、超时熔断。
/// `image` 和 `frame` 子命令的调用形态完全一致（`onas <subcmd> <in> <out>`，
/// 同样的错误处理约定），只是超时上限不同（见调用方注释），没有必要
/// 各写一份重复的进程管理代码。
fn run_onas(args: &[&str], timeout: Duration) -> Result<(), OnasBridgeError> {
    let onas = locate_onas()?;

    let mut child = Command::new(&onas)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(OnasBridgeError::SpawnFailed)?;

    // stdout/stderr 必须用独立线程边跑边读走：onas 成功时会往 stdout 打一行
    // 摘要信息，失败时会往 stderr 打完整错误链——如果只在主线程里
    // `try_wait()` 轮询而不消费管道，一旦子进程输出量超过 OS 管道缓冲区
    // （通常 64KB），子进程会阻塞在 write 上，我们这边永远等不到它退出，
    // 是经典的管道死锁场景。理论上单次转换的输出很小，实际不太可能触发，
    // 但正确处理管道排空不应该依赖"输出应该很小"这种假设。
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped at spawn");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped at spawn");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait().map_err(OnasBridgeError::WaitFailed)? {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    // kill() 关闭子进程的标准句柄，两个读线程的
                    // read_to_string 会随之自然返回（EOF），不会悬挂，
                    // 后面的 join() 可以放心调用、不会无限期阻塞。
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(OnasBridgeError::Timeout(timeout));
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    };

    let stderr = stderr_reader.join().unwrap_or_default();
    let _ = stdout_reader.join();

    if status.success() {
        Ok(())
    } else {
        Err(OnasBridgeError::OnasFailed { stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_output_paths_are_unique_across_calls() {
        let a = temp_output_path("png");
        let b = temp_output_path("png");
        assert_ne!(a, b, "连续两次调用必须产生不同的临时文件名，否则并发/连续转换会互相覆盖");
    }

    #[test]
    fn temp_output_path_uses_requested_extension() {
        let p = temp_output_path("png");
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
    }

    #[test]
    fn missing_onas_binary_reports_all_tried_locations() {
        // 清空 ROOQ_ONAS，且假设测试环境 PATH 里没有 onas，
        // 验证至少不会 panic，且错误信息里包含"尝试过"的线索。
        std::env::remove_var("ROOQ_ONAS");
        // 注意：这个测试在恰好安装了 onas 到 PATH 的机器上会失败于
        // "本该报错却成功了"，这是预期的——那种情况下测试环境本身
        // 就不满足"onas 不存在"这个前提，不是代码逻辑的问题。
        match locate_onas() {
            Err(OnasBridgeError::BinaryNotFound { tried }) => {
                assert!(!tried.is_empty());
            }
            other => {
                // 环境里真的有 onas 可执行文件时会走到这里，不算测试失败，
                // 只是这条用例在那种机器上验证不了目标行为。
                let _ = other;
            }
        }
    }
}
