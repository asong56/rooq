//! Generation counter used to discard stale preview results.
//!
//! When the user switches preview targets quickly, a decode from the
//! previous file may finish after a newer request was already issued.
//! Each switch bumps the generation; a result is only applied if its
//! generation still matches current.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct RequestGenerator {
    current: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
pub struct RequestToken {
    generation_at_dispatch: u64,
    current: Arc<AtomicU64>,
}

impl RequestGenerator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self) -> RequestToken {
        let new_gen = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        RequestToken {
            generation_at_dispatch: new_gen,
            current: Arc::clone(&self.current),
        }
    }
}

impl RequestToken {
    /// False means a decode result should be discarded rather than applied.
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
        assert!(!old_token.is_still_current());
    }

    #[test]
    fn latest_token_remains_current_until_superseded() {
        let gen = RequestGenerator::new();
        let token = gen.advance();
        assert!(token.is_still_current());
        assert!(token.is_still_current());
    }
}
