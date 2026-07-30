//! Evidential stamps prevent evidence from being counted more than once.

use core::fmt;

use crate::config::STAMP_SIZE;

pub const STAMP_FREE: u64 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stamp {
    evidential_base: [u64; STAMP_SIZE],
}

impl Default for Stamp {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Stamp {
    pub const EMPTY: Self = Self {
        evidential_base: [STAMP_FREE; STAMP_SIZE],
    };

    #[must_use]
    pub fn from_evidence(evidence: impl IntoIterator<Item = u64>) -> Self {
        let mut stamp = Self::EMPTY;
        for (slot, value) in stamp
            .evidential_base
            .iter_mut()
            .zip(evidence.into_iter().filter(|value| *value != STAMP_FREE))
        {
            *slot = value;
        }
        stamp
    }

    #[must_use]
    pub const fn evidential_base(&self) -> &[u64; STAMP_SIZE] {
        &self.evidential_base
    }

    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        self.evidential_base
            .iter()
            .copied()
            .take_while(|value| *value != STAMP_FREE)
    }

    /// Alternates entries from both stamps, matching ONA's `Stamp_make`.
    #[must_use]
    pub fn zip(&self, other: &Self) -> Self {
        let mut output = [STAMP_FREE; STAMP_SIZE];
        let mut write = 0;
        let mut left = self.iter();
        let mut right = other.iter();

        loop {
            let mut progressed = false;
            if let Some(value) = left.next() {
                output[write] = value;
                write += 1;
                progressed = true;
                if write == STAMP_SIZE {
                    break;
                }
            }
            if let Some(value) = right.next() {
                output[write] = value;
                write += 1;
                progressed = true;
                if write == STAMP_SIZE {
                    break;
                }
            }
            if !progressed {
                break;
            }
        }
        Self {
            evidential_base: output,
        }
    }

    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.iter()
            .any(|left| other.iter().any(|right| left == right))
    }

    #[must_use]
    pub fn has_duplicate(&self) -> bool {
        self.iter()
            .enumerate()
            .any(|(index, value)| self.iter().skip(index + 1).any(|other| value == other))
    }

    /// ONA treats stamp equality as set equality rather than sequence equality.
    #[must_use]
    pub fn evidence_eq(&self, other: &Self) -> bool {
        self.iter().count() == other.iter().count()
            && self
                .iter()
                .all(|value| other.iter().any(|other| value == other))
    }
}

impl fmt::Display for Stamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Stamp=[")?;
        for (index, value) in self.iter().enumerate() {
            if index > 0 {
                write!(formatter, ",")?;
            }
            write!(formatter, "{value}")?;
        }
        write!(formatter, "]")
    }
}

#[cfg(test)]
mod tests {
    use super::Stamp;

    #[test]
    fn upstream_stamp_test() {
        let first = Stamp::from_evidence([1, 2]);
        let second = Stamp::from_evidence([2, 3, 4]);
        let zipped = first.zip(&second);

        assert_eq!(first.to_string(), "Stamp=[1,2]");
        assert_eq!(second.to_string(), "Stamp=[2,3,4]");
        assert_eq!(zipped.to_string(), "Stamp=[1,2,2,3,4]");
        assert!(first.overlaps(&second), "Stamp should overlap");
        assert!(zipped.has_duplicate());
    }

    #[test]
    fn zip_truncates_at_ona_capacity() {
        let first = Stamp::from_evidence(1..=10);
        let second = Stamp::from_evidence(11..=20);
        assert_eq!(
            first.zip(&second).iter().collect::<Vec<_>>(),
            [1, 11, 2, 12, 3, 13, 4, 14, 5, 15]
        );
    }

    #[test]
    fn evidence_equality_ignores_order() {
        let first = Stamp::from_evidence([1, 2, 3]);
        let second = Stamp::from_evidence([3, 1, 2]);
        assert!(first.evidence_eq(&second));
    }
}
