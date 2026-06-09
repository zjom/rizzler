//! Fixed-capacity double-ended ring buffer backing the editor's bounded
//! history queues (jumplist, change list, etc.). Inserts on a full buffer
//! evict the opposite end and return the displaced value.

use std::fmt;
use std::mem::MaybeUninit;

/// A fixed-capacity, double-ended, overwriting ring buffer.
///
/// # Full-buffer behaviour
///
/// - `push_back` on a full buffer **evicts the front** (oldest) element.
/// - `push_front` on a full buffer **evicts the back** element.
///
///
/// # Invariants
///
/// Exactly the slots at physical indices
/// `(head + i) % N` for `i in 0..len` are initialised.
/// All other slots hold uninitialised bytes and must never be read.
///
pub struct RingBuffer<T, const N: usize> {
    data: [MaybeUninit<T>; N],
    head: usize, // physical index of the front element (valid when len > 0)
    len: usize,  // number of live elements; invariant: len <= N
}

impl<T, const N: usize> RingBuffer<T, N> {
    pub fn new() -> Self {
        assert!(N > 0, "capacity must be greater than 0");
        Self {
            // `array::from_fn` produces an array without requiring `T: Clone`,
            // unlike `[MaybeUninit::uninit(); N]`.
            data: std::array::from_fn(|_| MaybeUninit::uninit()),
            head: 0,
            len: 0,
        }
    }

    /// Return the live elements as up to two contiguous slices, front-to-back.
    /// The second slice is empty when the buffer does not wrap.
    pub fn as_slices(&self) -> (&[T], &[T]) {
        if self.len == 0 {
            return (&[], &[]);
        }
        let first_len = (N - self.head).min(self.len);
        let second_len = self.len - first_len;
        // SAFETY: slots `head..head+first_len` and `0..second_len` are
        // exactly the live range (by the type's invariant), so the pointed-to
        // `T`s are initialised and non-overlapping with any `&mut`.
        let first = unsafe { std::slice::from_raw_parts(self.data[self.head].as_ptr(), first_len) };
        let second = unsafe { std::slice::from_raw_parts(self.data[0].as_ptr(), second_len) };
        (first, second)
    }
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len == N
    }

    pub fn capacity(&self) -> usize {
        N
    }

    /// Physical slot where the next `push_back` writes (one past the current back).
    #[inline]
    fn back_slot(&self) -> usize {
        (self.head + self.len) % N
    }

    /// Physical slot where the next `push_front` writes (one before the current front).
    #[inline]
    fn front_slot(&self) -> usize {
        (self.head + N - 1) % N
    }

    /// Move ownership of the value out of `slot`.
    ///
    /// # Safety
    /// `slot` must be initialised. The caller is responsible for ensuring the
    /// slot is not read again without a subsequent `write`.
    #[inline]
    unsafe fn take_at(&mut self, slot: usize) -> T {
        unsafe { self.data[slot].assume_init_read() }
    }

    /// Run the destructor of the value at `slot` in place.
    ///
    /// # Safety
    /// `slot` must be initialised.
    #[inline]
    unsafe fn drop_at(&mut self, slot: usize) {
        unsafe {
            self.data[slot].assume_init_drop();
        }
    }

    /// Append `value` to the back.
    /// Returns `Some(evicted)` if the buffer was full (front element displaced),
    /// or `None` if there was room.
    pub fn push_back(&mut self, value: T) -> Option<T> {
        let evicted = if self.is_full() {
            // SAFETY: a full buffer guarantees the head slot is initialised.
            // After eviction the freed slot is exactly `back_slot()` (proof:
            //   back_slot_new = (head+1 + N-1) % N = head_old % N = head_old).
            let val = unsafe { self.take_at(self.head) };
            self.head = (self.head + 1) % N;
            self.len -= 1;
            Some(val)
        } else {
            None
        };
        self.data[self.back_slot()].write(value);
        self.len += 1;
        evicted
    }

    /// Prepend `value` to the front.
    /// Returns `Some(evicted)` if the buffer was full (back element displaced),
    /// or `None` if there was room.
    pub fn push_front(&mut self, value: T) -> Option<T> {
        let evicted = if self.is_full() {
            // SAFETY: a full buffer guarantees the back slot is initialised.
            // After eviction the freed slot is exactly `front_slot()` (proof:
            //   front_slot = (head + N-1) % N = back_slot when len == N).
            let back = (self.head + self.len - 1) % N;
            let val = unsafe { self.take_at(back) };
            self.len -= 1;
            Some(val)
        } else {
            None
        };
        let slot = self.front_slot();
        self.data[slot].write(value);
        self.head = slot;
        self.len += 1;
        evicted
    }

    /// Remove and return the back element, or `None` if empty.
    pub fn pop_back(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let slot = (self.head + self.len - 1) % N;
        self.len -= 1;
        // SAFETY: slot was in 0..old_len relative to head, so initialised.
        Some(unsafe { self.take_at(slot) })
    }

    /// Remove and return the front element, or `None` if empty.
    pub fn pop_front(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let slot = self.head;
        self.head = (self.head + 1) % N;
        self.len -= 1;
        // SAFETY: head slot is initialised whenever len > 0.
        Some(unsafe { self.take_at(slot) })
    }

    pub fn peek_back(&self) -> Option<&T> {
        if self.is_empty() {
            return None;
        }
        let slot = (self.head + self.len - 1) % N;
        // SAFETY: within the initialised range.
        Some(unsafe { self.data[slot].assume_init_ref() })
    }

    pub fn peek_front(&self) -> Option<&T> {
        if self.is_empty() {
            return None;
        }
        // SAFETY: head is initialised when len > 0.
        Some(unsafe { self.data[self.head].assume_init_ref() })
    }

    /// Iterate front-to-back without consuming the buffer.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        (0..self.len).map(move |i| {
            let slot = (self.head + i) % N;
            // SAFETY: slots 0..self.len relative to head are initialised.
            unsafe { self.data[slot].assume_init_ref() }
        })
    }
}

impl<T, const N: usize> Drop for RingBuffer<T, N> {
    fn drop(&mut self) {
        for i in 0..self.len {
            let slot = (self.head + i) % N;
            // SAFETY: slots 0..self.len relative to head are initialised.
            unsafe { self.drop_at(slot) };
        }
    }
}

impl<T: Clone, const N: usize> Clone for RingBuffer<T, N> {
    fn clone(&self) -> Self {
        let mut dst = Self::new();
        for i in 0..self.len {
            let slot = (self.head + i) % N;
            // SAFETY: within the live range.
            let val = unsafe { self.data[slot].assume_init_ref() }.clone();
            dst.data[(dst.head + dst.len) % N].write(val);
            dst.len += 1;
        }
        dst
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for RingBuffer<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = f.debug_list();
        for i in 0..self.len {
            let slot = (self.head + i) % N;
            // SAFETY: within the live range.
            list.entry(unsafe { self.data[slot].assume_init_ref() });
        }
        list.finish()
    }
}

impl<T, const N: usize> Default for RingBuffer<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Consuming iterator produced by `RingBuffer::into_iter`.
/// `next` yields front-to-back; `next_back` yields back-to-front.
pub struct IntoIter<T, const N: usize> {
    ring: RingBuffer<T, N>,
}

impl<T, const N: usize> IntoIterator for RingBuffer<T, N> {
    type Item = T;
    type IntoIter = IntoIter<T, N>;
    fn into_iter(self) -> Self::IntoIter {
        IntoIter { ring: self }
    }
}

impl<T, const N: usize> Iterator for IntoIter<T, N> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.ring.pop_front()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.ring.len();
        (n, Some(n))
    }
}

impl<T, const N: usize> DoubleEndedIterator for IntoIter<T, N> {
    fn next_back(&mut self) -> Option<T> {
        self.ring.pop_back()
    }
}

impl<T, const N: usize> ExactSizeIterator for IntoIter<T, N> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "capacity must be greater than 0")]
    fn test_zero_capacity_panics() {
        let _rb = RingBuffer::<i32, 0>::new();
    }

    #[test]
    fn test_push_and_len() {
        let mut rb = RingBuffer::<i32, 3>::new();
        rb.push_back(10);
        assert_eq!(rb.len(), 1);
        assert!(!rb.is_empty());
        rb.push_back(20);
        assert_eq!(rb.len(), 2);
        rb.push_back(30);
        assert_eq!(rb.len(), 3);
        assert!(rb.is_full());
        assert_eq!(rb.capacity(), 3);
    }

    #[test]
    fn test_push_back_wrap_evicts_front() {
        let mut rb = RingBuffer::<i32, 3>::new();
        assert_eq!(rb.push_back(1), None);
        assert_eq!(rb.push_back(2), None);
        assert_eq!(rb.push_back(3), None);
        assert_eq!(rb.push_back(4), Some(1)); // evicts 1
        assert_eq!(rb.len(), 3);
        assert_eq!(rb.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(rb.peek_front(), Some(&2));
        assert_eq!(rb.peek_back(), Some(&4));
    }

    #[test]
    fn test_push_front_wrap_evicts_back() {
        let mut rb = RingBuffer::<i32, 3>::new();
        assert_eq!(rb.push_front(1), None);
        assert_eq!(rb.push_front(2), None);
        assert_eq!(rb.push_front(3), None);
        assert_eq!(rb.push_front(4), Some(1)); // evicts 1 (the back)
        assert_eq!(rb.len(), 3);
        assert_eq!(rb.iter().copied().collect::<Vec<_>>(), vec![4, 3, 2]);
        assert_eq!(rb.peek_front(), Some(&4));
        assert_eq!(rb.peek_back(), Some(&2));
    }

    #[test]
    fn test_mixed_ends() {
        let mut rb = RingBuffer::<i32, 4>::new();
        rb.push_back(1);
        rb.push_front(0);
        rb.push_back(2);
        rb.push_front(-1);
        assert_eq!(rb.iter().copied().collect::<Vec<_>>(), vec![-1, 0, 1, 2]);
        assert_eq!(rb.pop_front(), Some(-1));
        assert_eq!(rb.pop_back(), Some(2));
        assert_eq!(rb.iter().copied().collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn test_pop_back() {
        let mut rb = RingBuffer::<i32, 3>::new();
        rb.push_back(10);
        rb.push_back(20);
        assert_eq!(rb.pop_back(), Some(20));
        assert_eq!(rb.len(), 1);
        assert_eq!(rb.pop_back(), Some(10));
        assert!(rb.is_empty());
        assert_eq!(rb.pop_back(), None);
    }

    #[test]
    fn test_pop_front() {
        let mut rb = RingBuffer::<i32, 3>::new();
        rb.push_back(10);
        rb.push_back(20);
        assert_eq!(rb.pop_front(), Some(10));
        assert_eq!(rb.len(), 1);
        assert_eq!(rb.pop_front(), Some(20));
        assert!(rb.is_empty());
        assert_eq!(rb.pop_front(), None);
    }

    #[test]
    fn test_peek_both_ends() {
        let mut rb = RingBuffer::<i32, 3>::new();
        assert_eq!(rb.peek_back(), None);
        assert_eq!(rb.peek_front(), None);
        rb.push_back(100);
        assert_eq!(rb.peek_back(), Some(&100));
        assert_eq!(rb.peek_front(), Some(&100));
        assert_eq!(rb.len(), 1); // peek is non-destructive
    }

    #[test]
    fn test_wrap_then_pop_returns_logical_order() {
        let mut rb = RingBuffer::<i32, 3>::new();
        for v in [1, 2, 3, 4, 5] {
            rb.push_back(v);
        }
        assert_eq!(rb.pop_back(), Some(5));
        assert_eq!(rb.pop_front(), Some(3));
        assert_eq!(rb.pop_back(), Some(4));
        assert_eq!(rb.pop_back(), None);
    }

    #[test]
    fn test_capacity_one() {
        let mut rb = RingBuffer::<i32, 1>::new();
        assert_eq!(rb.push_back(1), None);
        assert_eq!(rb.push_back(2), Some(1)); // evicts 1
        assert_eq!(rb.len(), 1);
        assert_eq!(rb.peek_front(), Some(&2));
        assert_eq!(rb.push_front(3), Some(2)); // evicts 2
        assert_eq!(rb.pop_front(), Some(3));
        assert!(rb.is_empty());
    }

    #[test]
    fn test_into_iter_front_to_back() {
        let mut rb = RingBuffer::<i32, 3>::new();
        rb.push_back(1);
        rb.push_back(2);
        rb.push_back(3);
        assert_eq!(rb.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn test_into_iter_double_ended() {
        let mut rb = RingBuffer::<i32, 4>::new();
        rb.push_back(1);
        rb.push_back(2);
        rb.push_back(3);
        let mut it = rb.into_iter();
        assert_eq!(it.next(), Some(1));
        assert_eq!(it.next_back(), Some(3));
        assert_eq!(it.next(), Some(2));
        assert_eq!(it.next(), None);
        assert_eq!(it.next_back(), None);
    }

    /// Verify `Drop` is called on every element — including evicted ones.
    /// With push_back/push_front returning `Option<T>`, the caller owns the
    /// evicted value; dropping the returned `Some` is what actually runs its
    /// destructor.
    #[test]
    fn test_drops_are_called() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering::SeqCst},
        };

        let count = Arc::new(AtomicUsize::new(0));
        struct Dropper(Arc<AtomicUsize>);
        impl Drop for Dropper {
            fn drop(&mut self) {
                self.0.fetch_add(1, SeqCst);
            }
        }
        let d = || Dropper(count.clone());

        {
            let mut rb = RingBuffer::<Dropper, 3>::new();
            rb.push_back(d());
            rb.push_back(d());
            rb.push_back(d());

            // Returned Some(Dropper) is dropped at the semicolon → count = 1.
            let evicted = rb.push_back(d());
            assert!(evicted.is_some());
            drop(evicted); // explicit drop; count becomes 1
            assert_eq!(count.load(SeqCst), 1);
        } // rb dropped → remaining 3 elements dropped → count = 4
        assert_eq!(count.load(SeqCst), 4);
    }

    #[test]
    fn test_clone() {
        let mut rb = RingBuffer::<i32, 4>::new();
        for v in [10, 20, 30] {
            rb.push_back(v);
        }
        let rb2 = rb.clone();
        assert_eq!(
            rb.iter().collect::<Vec<_>>(),
            rb2.iter().collect::<Vec<_>>(),
        );
    }

    #[test]
    fn test_debug_fmt() {
        let mut rb = RingBuffer::<i32, 3>::new();
        rb.push_back(1);
        rb.push_back(2);
        assert_eq!(format!("{rb:?}"), "[1, 2]");
    }
}
