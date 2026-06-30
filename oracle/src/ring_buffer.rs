use std::collections::VecDeque;

/// Default capacity for the failure history ring buffer.
pub const DEFAULT_CAPACITY: usize = 256;

/// Bounded ring buffer backed by a `VecDeque`.
///
/// When the buffer is full, `push` evicts the oldest item before inserting
/// the new one, keeping memory usage strictly bounded.
#[derive(Debug, Clone)]
pub struct RingBuffer<T> {
    capacity: usize,
    items: VecDeque<T>,
}

impl<T> RingBuffer<T> {
    /// Create a new ring buffer with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            items: VecDeque::with_capacity(capacity),
        }
    }

    /// Push an item. Evicts the oldest entry when at capacity.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    /// Iterate items oldest-first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<T> Default for RingBuffer<T> {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #338 — evicts oldest item when at capacity.
    #[test]
    fn ring_buffer_evicts_oldest_at_capacity() {
        let mut buf: RingBuffer<u32> = RingBuffer::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        // Now at capacity — next push should evict 1.
        buf.push(4);

        let items: Vec<u32> = buf.iter().copied().collect();
        assert_eq!(items, vec![2, 3, 4]);
    }

    /// #338 — default capacity is 256.
    #[test]
    fn ring_buffer_default_capacity_is_256() {
        let buf: RingBuffer<u8> = RingBuffer::default();
        assert_eq!(buf.capacity(), DEFAULT_CAPACITY);
    }

    /// #338 — iter() traverses items in insertion order.
    #[test]
    fn ring_buffer_iter_is_oldest_first() {
        let mut buf = RingBuffer::new(4);
        for i in 0u32..4 {
            buf.push(i);
        }
        let items: Vec<u32> = buf.iter().copied().collect();
        assert_eq!(items, vec![0, 1, 2, 3]);
    }

    /// #338 — len stays bounded at capacity.
    #[test]
    fn ring_buffer_len_never_exceeds_capacity() {
        let mut buf = RingBuffer::new(2);
        for i in 0u32..10 {
            buf.push(i);
            assert!(buf.len() <= 2);
        }
    }
}
