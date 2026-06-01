//! Bounded iteration driver (ADR-0005 / SP3 seed).
//!
//! v1 owns only the round count and cap — the smallest shape team-mode rounds
//! need. SP3 generalizes it (pluggable stop policy, a per-iteration body, and
//! budget/event integration) into the loop shared with refine-skill's
//! judge→act; see `solo://proj/15/scratchpad/9`.

pub struct LoopDriver {
    pub max_iterations: u32,
}

impl LoopDriver {
    pub fn new(max_iterations: u32) -> Self {
        Self {
            max_iterations: max_iterations.max(1),
        }
    }

    /// True on the final iteration — no between-iteration hook runs after it.
    pub fn is_final_round(&self, round: u32) -> bool {
        round >= self.max_iterations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_at_least_one_and_marks_final() {
        let d = LoopDriver::new(0);
        assert_eq!(d.max_iterations, 1);
        assert!(d.is_final_round(1));

        let d = LoopDriver::new(2);
        assert!(!d.is_final_round(1));
        assert!(d.is_final_round(2));
    }
}
