//! 请求代次管理。
//!
//! 场景：用户在文件管理器里用方向键快速切换预览文件时，
//! 上一个文件的解码可能还没完成，新的预览请求已经发出。
//! 如果不做处理，可能出现"当前显示的其实是上一个文件的解码结果"这种错乱。
//!
//! 做法：每次用户切换预览目标时，代次号 +1；
//! 后台解码任务（当前所有 provider 都是同步阻塞调用，在专门的线程里跑）
//! 完成后，只有代次号仍然等于"当前代次"时，结果才会被采纳并送去渲染；
//! 否则直接丢弃。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 代次生成器。整个应用只需要一个实例，通常放在顶层 App 状态里。
#[derive(Debug, Default)]
pub struct RequestGenerator {
    current: Arc<AtomicU64>,
}

/// 某次请求发出时拿到的"代次快照"，解码完成后用它来判断结果是否仍然有效。
#[derive(Debug, Clone)]
pub struct RequestToken {
    generation_at_dispatch: u64,
    current: Arc<AtomicU64>,
}

impl RequestGenerator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用户切换了预览目标（打开新文件/关闭预览）时调用，
    /// 让所有仍在飞行中的旧请求在完成时自动失效。
    pub fn advance(&self) -> RequestToken {
        let new_gen = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        RequestToken {
            generation_at_dispatch: new_gen,
            current: Arc::clone(&self.current),
        }
    }
}

impl RequestToken {
    /// 解码任务完成后调用，判断这个结果是否仍然对应"当前正在预览的文件"。
    /// 返回 false 时，调用方应当直接丢弃解码结果，不更新 UI 状态。
    pub fn is_still_current(&self) -> bool {
        self.current.load(Ordering::SeqCst) == self.generation_at_dispatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_token_is_detected_after_advance() {
        let gen = RequestGenerator::new();
        let old_token = gen.advance();
        assert!(old_token.is_still_current());

        let _new_token = gen.advance();
        assert!(
            !old_token.is_still_current(),
            "旧token在generator前进后应当失效"
        );
    }

    #[test]
    fn latest_token_remains_current_until_superseded() {
        let gen = RequestGenerator::new();
        let token = gen.advance();
        assert!(token.is_still_current());
        // 没有新的 advance 调用，token 应该一直有效
        assert!(token.is_still_current());
    }
}
