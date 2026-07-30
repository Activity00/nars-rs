//! Safe fixed-capacity equivalents of ONA's pointer stack and chained table.

use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundedError {
    Overflow,
    Underflow,
    ZeroBuckets,
    MissingKey,
}

impl fmt::Display for BoundedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Overflow => "fixed-capacity structure is full",
            Self::Underflow => "fixed-capacity stack is empty",
            Self::ZeroBuckets => "hash table requires at least one bucket",
            Self::MissingKey => "hash table key was not found",
        })
    }
}

impl Error for BoundedError {}

#[derive(Clone, Debug)]
pub struct BoundedStack<T> {
    items: Vec<T>,
    capacity: usize,
}

impl<T> BoundedStack<T> {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            items: Vec::with_capacity(capacity),
            capacity,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn free_len(&self) -> usize {
        self.capacity - self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Pushes one item onto the stack.
    ///
    /// # Errors
    ///
    /// Returns [`BoundedError::Overflow`] when the fixed capacity is full.
    pub fn push(&mut self, item: T) -> Result<(), BoundedError> {
        if self.items.len() == self.capacity {
            return Err(BoundedError::Overflow);
        }
        self.items.push(item);
        Ok(())
    }

    /// Pops the most recently pushed item.
    ///
    /// # Errors
    ///
    /// Returns [`BoundedError::Underflow`] when the stack is empty.
    pub fn pop(&mut self) -> Result<T, BoundedError> {
        self.items.pop().ok_or(BoundedError::Underflow)
    }
}

#[derive(Clone, Debug)]
struct Entry<K, V> {
    key: K,
    value: V,
}

/// Fixed-capacity, explicitly chained hash table with deterministic buckets.
pub struct BoundedHashTable<K, V, H> {
    buckets: Vec<Vec<Entry<K, V>>>,
    capacity: usize,
    len: usize,
    hash: H,
}

impl<K, V, H> BoundedHashTable<K, V, H>
where
    K: Eq,
    H: Fn(&K) -> u64,
{
    /// Creates a table with fixed bucket and element counts.
    ///
    /// # Errors
    ///
    /// Returns [`BoundedError::ZeroBuckets`] when `buckets` is zero.
    pub fn new(buckets: usize, capacity: usize, hash: H) -> Result<Self, BoundedError> {
        if buckets == 0 {
            return Err(BoundedError::ZeroBuckets);
        }
        Ok(Self {
            buckets: std::iter::repeat_with(Vec::new).take(buckets).collect(),
            capacity,
            len: 0,
            hash,
        })
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub const fn free_len(&self) -> usize {
        self.capacity - self.len
    }

    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    #[must_use]
    pub fn get(&self, key: &K) -> Option<&V> {
        self.buckets[self.bucket_index(key)]
            .iter()
            .find(|entry| entry.key == *key)
            .map(|entry| &entry.value)
    }

    /// Inserts a key or updates its existing value.
    ///
    /// Returns the replaced value when the key already existed.
    ///
    /// # Errors
    ///
    /// Returns [`BoundedError::Overflow`] when inserting a new key into a full
    /// table. Updating an existing key never consumes capacity.
    pub fn set(&mut self, key: K, value: V) -> Result<Option<V>, BoundedError> {
        let bucket = self.bucket_index(&key);
        if let Some(entry) = self.buckets[bucket]
            .iter_mut()
            .find(|entry| entry.key == key)
        {
            return Ok(Some(std::mem::replace(&mut entry.value, value)));
        }
        if self.len == self.capacity {
            return Err(BoundedError::Overflow);
        }
        self.buckets[bucket].push(Entry { key, value });
        self.len += 1;
        Ok(None)
    }

    /// Removes one key and returns its value, recycling its capacity.
    ///
    /// # Errors
    ///
    /// Returns [`BoundedError::MissingKey`] when the key is absent.
    pub fn delete(&mut self, key: &K) -> Result<V, BoundedError> {
        let bucket = self.bucket_index(key);
        let index = self.buckets[bucket]
            .iter()
            .position(|entry| entry.key == *key)
            .ok_or(BoundedError::MissingKey)?;
        self.len -= 1;
        Ok(self.buckets[bucket].remove(index).value)
    }

    #[must_use]
    pub fn maximum_chain_length(&self) -> usize {
        self.buckets.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Iterates a bucket in insertion/chain order.
    ///
    /// # Errors
    ///
    /// Returns [`BoundedError::MissingKey`] for an invalid bucket index.
    pub fn bucket_entries(
        &self,
        bucket: usize,
    ) -> Result<impl Iterator<Item = (&K, &V)>, BoundedError> {
        let entries = self.buckets.get(bucket).ok_or(BoundedError::MissingKey)?;
        Ok(entries.iter().map(|entry| (&entry.key, &entry.value)))
    }

    fn bucket_index(&self, key: &K) -> usize {
        let bucket_count = u64::try_from(self.buckets.len()).unwrap_or(u64::MAX);
        usize::try_from((self.hash)(key) % bucket_count).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundedError, BoundedHashTable, BoundedStack};
    use crate::{ConceptId, Narsese};

    #[test]
    fn upstream_stack_test() {
        let mut stack = BoundedStack::new(5);
        let first = ConceptId(1);
        let second = ConceptId(2);
        stack.push(first).unwrap();
        assert_eq!(stack.len(), 1);
        assert!(!stack.is_empty());
        assert_eq!(stack.pop().unwrap(), first);
        assert!(stack.is_empty());
        stack.push(first).unwrap();
        stack.push(second).unwrap();
        assert_eq!(stack.len(), 2);
        assert_eq!(stack.pop().unwrap(), second);
        assert_eq!(stack.pop().unwrap(), first);
        assert!(stack.is_empty());
        assert_eq!(stack.pop(), Err(BoundedError::Underflow));
        let mut full = BoundedStack::new(1);
        full.push(first).unwrap();
        assert_eq!(full.push(second), Err(BoundedError::Overflow));
    }

    #[test]
    fn upstream_hash_table_collision_and_recycling_test() {
        let mut language = Narsese::new();
        let first = language.parse_term("<a --> b>").unwrap();
        let second = language.parse_term("<c --> d>").unwrap();
        let third = language.parse_term("<e --> f>").unwrap();
        let mut table = BoundedHashTable::new(10, 10, |_| 1).unwrap();
        assert_eq!(table.free_len(), 10);
        table.set(first, ConceptId(1)).unwrap();
        assert_eq!(table.free_len(), 9);
        assert_eq!(table.get(&first), Some(&ConceptId(1)));
        table.set(second, ConceptId(2)).unwrap();
        table.set(third, ConceptId(3)).unwrap();
        assert_eq!(table.maximum_chain_length(), 3);
        let keys = table
            .bucket_entries(1)
            .unwrap()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        assert_eq!(keys, [first, second, third]);
        assert_eq!(table.delete(&second).unwrap(), ConceptId(2));
        assert_eq!(
            table
                .bucket_entries(1)
                .unwrap()
                .map(|(key, _)| *key)
                .collect::<Vec<_>>(),
            [first, third]
        );
        table.delete(&third).unwrap();
        table.delete(&first).unwrap();
        assert_eq!(table.free_len(), 10);
        assert_eq!(table.maximum_chain_length(), 0);
    }

    #[test]
    fn hash_table_updates_and_enforces_capacity() {
        let mut table = BoundedHashTable::new(2, 1, |key: &&str| key.len() as u64).unwrap();
        assert_eq!(table.set("test", 42).unwrap(), None);
        assert_eq!(table.set("test", 43).unwrap(), Some(42));
        assert_eq!(table.get(&"test"), Some(&43));
        assert_eq!(table.set("full", 1), Err(BoundedError::Overflow));
        assert_eq!(table.delete(&"test").unwrap(), 43);
        assert_eq!(table.set("full", 1).unwrap(), None);
    }
}
