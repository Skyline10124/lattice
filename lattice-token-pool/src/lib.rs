/// Trait for sharing a token budget across multiple agents.
pub trait TokenPool: Send + Sync {
    /// Try to acquire `amount` tokens. Returns false if not enough remain.
    fn acquire(&mut self, agent: &str, amount: u32) -> bool;

    /// Return unused tokens to the pool.
    fn release(&mut self, agent: &str, amount: u32);

    /// Tokens currently available.
    fn remaining(&self) -> u32;
}

/// Default implementation: no limit. acquire() always returns true.
pub struct UnlimitedPool;

impl TokenPool for UnlimitedPool {
    fn acquire(&mut self, _agent: &str, _amount: u32) -> bool {
        true
    }

    fn release(&mut self, _agent: &str, _amount: u32) {}

    fn remaining(&self) -> u32 {
        u32::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unlimited_pool_always_acquires() {
        let mut pool = UnlimitedPool;
        assert!(pool.acquire("agent-1", 1_000_000));
    }

    #[test]
    fn test_unlimited_pool_release_doesnt_panic() {
        let mut pool = UnlimitedPool;
        pool.release("agent-1", 100);
    }

    #[test]
    fn test_unlimited_pool_remaining_is_max() {
        let pool = UnlimitedPool;
        assert_eq!(pool.remaining(), u32::MAX);
    }
}
