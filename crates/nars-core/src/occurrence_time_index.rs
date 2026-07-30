//! Fixed-capacity newest-first ring index for temporally related concepts.

use crate::config::OCCURRENCE_TIME_INDEX_SIZE;

#[derive(Clone, Debug)]
pub struct OccurrenceTimeIndex<T: Copy, const N: usize = OCCURRENCE_TIME_INDEX_SIZE> {
    entries: Vec<Option<T>>,
    len: usize,
    current: usize,
}

impl<T: Copy, const N: usize> Default for OccurrenceTimeIndex<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy, const N: usize> OccurrenceTimeIndex<T, N> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: vec![None; N],
            len: 0,
            current: 0,
        }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn add(&mut self, value: T) {
        if N == 0 {
            return;
        }
        self.entries[self.current] = Some(value);
        self.current = (self.current + 1) % N;
        self.len = (self.len + 1).min(N);
    }

    #[must_use]
    pub fn kth_newest(&self, k: usize) -> Option<T> {
        if k >= self.len || N == 0 {
            return None;
        }
        let index = (self.current + N - 1 - k) % N;
        self.entries[index]
    }
}

#[cfg(test)]
mod tests {
    use super::OccurrenceTimeIndex;

    #[test]
    fn upstream_rollover_test() {
        let mut index = OccurrenceTimeIndex::<usize, 512>::new();
        for value in (1..=1024).rev() {
            index.add(value);
        }
        assert_eq!(index.len(), 512);
        assert_eq!(index.kth_newest(0), Some(1));
        assert_eq!(index.kth_newest(511), Some(512));
        assert_eq!(index.kth_newest(512), None);
    }
}
