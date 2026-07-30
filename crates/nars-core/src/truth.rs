//! NAL truth values and truth functions, ported from ONA `Truth.c`.
#![allow(clippy::cast_precision_loss, clippy::float_cmp)]

use crate::config::{
    MAX_CONFIDENCE, OCCURRENCE_ETERNAL, RELIANCE, TRUTH_EVIDENTIAL_HORIZON, TRUTH_PROJECTION_DECAY,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Truth {
    pub frequency: f64,
    pub confidence: f64,
}

impl Truth {
    pub const STRUCTURAL: Self = Self::new(1.0, RELIANCE);

    #[must_use]
    pub const fn new(frequency: f64, confidence: f64) -> Self {
        Self {
            frequency,
            confidence,
        }
    }

    #[must_use]
    pub fn weight_to_confidence(weight: f64) -> f64 {
        weight / (weight + TRUTH_EVIDENTIAL_HORIZON)
    }

    #[must_use]
    pub fn confidence_to_weight(confidence: f64) -> f64 {
        TRUTH_EVIDENTIAL_HORIZON * confidence / (1.0 - confidence)
    }

    #[must_use]
    pub fn expectation(self) -> f64 {
        self.confidence.mul_add(self.frequency - 0.5, 0.5)
    }

    #[must_use]
    pub fn revision(self, other: Self) -> Self {
        if self.confidence == 0.0 {
            return other;
        }
        if other.confidence == 0.0 {
            return self;
        }
        if MAX_CONFIDENCE == 1.0 {
            if self.confidence == 1.0 {
                return self;
            }
            if other.confidence == 1.0 {
                return other;
            }
        }
        let first_weight = Self::confidence_to_weight(self.confidence);
        let second_weight = Self::confidence_to_weight(other.confidence);
        let weight = first_weight + second_weight;
        Self::new(
            ((first_weight * self.frequency + second_weight * other.frequency) / weight).min(1.0),
            Self::weight_to_confidence(weight)
                .max(self.confidence)
                .max(other.confidence)
                .min(MAX_CONFIDENCE),
        )
    }

    #[must_use]
    pub fn deduction(self, other: Self) -> Self {
        let frequency = self.frequency * other.frequency;
        Self::new(frequency, self.confidence * other.confidence * frequency)
    }

    #[must_use]
    pub fn abduction(self, other: Self) -> Self {
        Self::new(
            other.frequency,
            Self::weight_to_confidence(self.frequency * self.confidence * other.confidence),
        )
    }

    #[must_use]
    pub fn induction(self, other: Self) -> Self {
        other.abduction(self)
    }

    #[must_use]
    pub fn intersection(self, other: Self) -> Self {
        Self::new(
            self.frequency * other.frequency,
            self.confidence * other.confidence,
        )
    }

    #[must_use]
    pub fn eternalize(self) -> Self {
        Self::new(self.frequency, Self::weight_to_confidence(self.confidence))
    }

    #[must_use]
    pub fn project(self, original_time: i64, target_time: i64) -> Self {
        if original_time == OCCURRENCE_ETERNAL {
            self
        } else {
            let distance = target_time.abs_diff(original_time) as f64;
            Self::new(
                self.frequency,
                self.confidence * TRUTH_PROJECTION_DECAY.powf(distance),
            )
        }
    }

    #[must_use]
    pub fn exemplification(self, other: Self) -> Self {
        Self::new(
            1.0,
            Self::weight_to_confidence(
                self.frequency * other.frequency * self.confidence * other.confidence,
            ),
        )
    }

    fn or(first: f64, second: f64) -> f64 {
        1.0 - (1.0 - first) * (1.0 - second)
    }

    #[must_use]
    pub fn comparison(self, other: Self) -> Self {
        let union = Self::or(self.frequency, other.frequency);
        Self::new(
            if union == 0.0 {
                0.0
            } else {
                self.frequency * other.frequency / union
            },
            Self::weight_to_confidence(union * self.confidence * other.confidence),
        )
    }

    #[must_use]
    pub fn analogy(self, other: Self) -> Self {
        Self::new(
            self.frequency * other.frequency,
            self.confidence * other.confidence * other.frequency,
        )
    }

    #[must_use]
    pub fn resemblance(self, other: Self) -> Self {
        Self::new(
            self.frequency * other.frequency,
            self.confidence * other.confidence * Self::or(self.frequency, other.frequency),
        )
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self::new(
            Self::or(self.frequency, other.frequency),
            self.confidence * other.confidence,
        )
    }

    #[must_use]
    pub fn difference(self, other: Self) -> Self {
        Self::new(
            self.frequency * (1.0 - other.frequency),
            self.confidence * other.confidence,
        )
    }

    #[must_use]
    pub fn conversion(self) -> Self {
        Self::new(
            1.0,
            Self::weight_to_confidence(self.frequency * self.confidence),
        )
    }

    #[must_use]
    pub fn negation(self) -> Self {
        Self::new(1.0 - self.frequency, self.confidence)
    }

    #[must_use]
    pub fn structural_deduction(self) -> Self {
        self.deduction(Self::STRUCTURAL)
    }

    #[must_use]
    pub fn structural_deduction_negated(self) -> Self {
        self.structural_deduction().negation()
    }

    #[must_use]
    pub fn structural_intersection(self) -> Self {
        self.intersection(Self::STRUCTURAL)
    }

    #[must_use]
    pub fn decompose_pnn(self, other: Self) -> Self {
        let negative_frequency = self.frequency * (1.0 - other.frequency);
        Self::new(
            1.0 - negative_frequency,
            negative_frequency * self.confidence * other.confidence,
        )
    }

    #[must_use]
    pub fn decompose_npp(self, other: Self) -> Self {
        let frequency = (1.0 - self.frequency) * other.frequency;
        Self::new(frequency, frequency * self.confidence * other.confidence)
    }

    #[must_use]
    pub fn decompose_pnp(self, other: Self) -> Self {
        let frequency = self.frequency * (1.0 - other.frequency);
        Self::new(frequency, frequency * self.confidence * other.confidence)
    }

    #[must_use]
    pub fn decompose_ppp(self, other: Self) -> Self {
        self.negation().decompose_npp(other)
    }

    #[must_use]
    pub fn decompose_nnn(self, other: Self) -> Self {
        let negative_frequency = (1.0 - self.frequency) * (1.0 - other.frequency);
        Self::new(
            1.0 - negative_frequency,
            negative_frequency * self.confidence * other.confidence,
        )
    }

    #[must_use]
    pub fn anonymous_analogy(self, other: Self) -> Self {
        self.analogy(Self::new(
            1.0,
            Self::weight_to_confidence(other.frequency * other.confidence),
        ))
    }

    #[must_use]
    pub fn goal_deduction(self, other: Self) -> Self {
        let positive = self.deduction(other);
        let negative = self.negation().deduction(other).negation();
        if positive.confidence >= negative.confidence {
            positive
        } else {
            negative
        }
    }

    #[must_use]
    pub fn frequency_greater(self, other: Self) -> Self {
        if self.frequency > other.frequency {
            Self::new(1.0, self.confidence * other.confidence)
        } else {
            Self::new(0.0, 0.0)
        }
    }

    #[must_use]
    pub fn frequency_equal(self, other: Self) -> Self {
        if self.frequency == other.frequency {
            Self::new(1.0, self.confidence * other.confidence)
        } else {
            Self::new(0.0, 0.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Truth;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1.0e-12,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn evidential_weight_round_trip_matches_ona() {
        let confidence = 0.9;
        assert_close(
            Truth::weight_to_confidence(Truth::confidence_to_weight(confidence)),
            confidence,
        );
    }

    #[test]
    fn revision_combines_independent_evidence() {
        let revised = Truth::new(1.0, 0.9).revision(Truth::new(0.0, 0.9));
        assert_close(revised.frequency, 0.5);
        assert_close(revised.confidence, 18.0 / 19.0);
    }

    #[test]
    fn deduction_matches_ona_formula() {
        let result = Truth::new(0.8, 0.9).deduction(Truth::new(0.5, 0.8));
        assert_close(result.frequency, 0.4);
        assert_close(result.confidence, 0.288);
    }

    #[test]
    fn eternal_projection_is_unchanged() {
        let truth = Truth::new(0.7, 0.8);
        assert_eq!(truth.project(-1, 100), truth);
    }
}
