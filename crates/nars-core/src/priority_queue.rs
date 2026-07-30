//! Fixed-capacity double-ended priority queue with ONA-compatible eviction.

#[derive(Clone, Debug, PartialEq)]
pub struct PriorityItem<T> {
    pub priority: f64,
    pub value: T,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PushFeedback<T> {
    pub added: bool,
    pub evicted: Option<PriorityItem<T>>,
}

#[derive(Clone, Debug)]
pub struct PriorityQueue<T> {
    capacity: usize,
    items: Vec<PriorityItem<T>>,
}

impl<T> PriorityQueue<T> {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            items: Vec::with_capacity(capacity),
        }
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PriorityItem<T>> {
        self.items.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut PriorityItem<T>> {
        self.items.iter_mut()
    }

    fn min_index(&self) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| left.priority.total_cmp(&right.priority))
            .map(|(index, _)| index)
    }

    fn max_index(&self) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.priority.total_cmp(&right.priority))
            .map(|(index, _)| index)
    }

    pub fn push(&mut self, priority: f64, value: T) -> PushFeedback<T> {
        if self.capacity == 0 {
            return PushFeedback {
                added: false,
                evicted: None,
            };
        }

        let evicted = if self.items.len() == self.capacity {
            let Some(minimum) = self.min_index() else {
                return PushFeedback {
                    added: false,
                    evicted: None,
                };
            };
            if priority < self.items[minimum].priority {
                return PushFeedback {
                    added: false,
                    evicted: None,
                };
            }
            Some(self.items.swap_remove(minimum))
        } else {
            None
        };

        self.items.push(PriorityItem { priority, value });
        PushFeedback {
            added: true,
            evicted,
        }
    }

    pub fn pop_min(&mut self) -> Option<PriorityItem<T>> {
        self.min_index().map(|index| self.items.swap_remove(index))
    }

    pub fn pop_max(&mut self) -> Option<PriorityItem<T>> {
        self.max_index().map(|index| self.items.swap_remove(index))
    }

    pub fn pop_at(&mut self, index: usize) -> Option<PriorityItem<T>> {
        (index < self.items.len()).then(|| self.items.swap_remove(index))
    }

    /// Kept for API parity. The safe representation needs no explicit rebuild.
    pub const fn rebuild(&mut self) {}
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss)]

    use super::PriorityQueue;

    #[test]
    fn upstream_priority_queue_test() {
        let item_count = 10;
        let mut queue = PriorityQueue::new(item_count);
        let mut evictions = 0;

        for index in 0..(item_count * 2) {
            let priority = 1.0 / ((item_count * 2 - index) as f64);
            let feedback = queue.push(priority, index + 1);
            assert!(feedback.added);
            if let Some(evicted) = feedback.evicted {
                if evictions == 0 {
                    assert!((evicted.priority - 1.0 / 20.0).abs() < 0.00001);
                }
                assert!(queue.len() <= item_count);
                evictions += 1;
            }
        }

        assert_eq!(evictions, item_count);
        assert_eq!(queue.len(), item_count);
        assert!((queue.pop_min().unwrap().priority - 1.0 / 10.0).abs() < 1.0e-12);
        assert!((queue.pop_max().unwrap().priority - 1.0).abs() < 1.0e-12);
    }

    #[test]
    fn lower_priority_is_rejected_when_full() {
        let mut queue = PriorityQueue::new(2);
        assert!(queue.push(0.5, "middle").added);
        assert!(queue.push(1.0, "high").added);
        assert!(!queue.push(0.1, "low").added);
        assert_eq!(queue.len(), 2);
    }
}
