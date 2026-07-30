//! Concept usefulness combines frequency of use with recency.
#![allow(clippy::cast_precision_loss)]

use crate::config::ETERNAL_INPUT_USAGE_BOOST;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub use_count: u64,
    pub last_used: i64,
}

impl Usage {
    #[must_use]
    pub fn usefulness(self, current_time: i64) -> f64 {
        let recency = current_time.saturating_sub(self.last_used).max(0) as f64;
        let value = self.use_count as f64 / (recency + 1.0);
        value / (value + 1.0)
    }

    #[must_use]
    pub fn used(self, current_time: i64, eternal_input: bool) -> Self {
        Self {
            use_count: self.use_count.saturating_add(if eternal_input {
                ETERNAL_INPUT_USAGE_BOOST
            } else {
                1
            }),
            last_used: current_time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Usage;

    #[test]
    fn usefulness_decays_with_recency() {
        let usage = Usage {
            use_count: 10,
            last_used: 5,
        };
        assert!(usage.usefulness(5) > usage.usefulness(50));
    }
}
