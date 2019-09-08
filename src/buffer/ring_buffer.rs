use core::mem::MaybeUninit;
use core::marker::PhantomData;
use super::RealArray;

/// A Ring Buffer of items
pub trait RingBuf {
    /// The type of stored items inside the Ring Buffer
    type Item;

    /// Creates a new instance of the Ring Buffer
    fn new() -> Self;
    /// Creates a new instance of the Ring Buffer with the given capacity.
    /// `RingBuf` implementations are allowed to ignore the `capacity` hint and
    /// utilize their default capacity.
    fn with_capacity(cap: usize) -> Self;

    /// The capacity of the buffer
    fn capacity(&self) -> usize;
    /// The amount of stored items in the buffer
    fn len(&self) -> usize;
    /// Returns true if no item is stored inside the buffer.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns true if there is enough space in the buffer to
    /// store another item.
    fn can_push(&self) -> bool;
    /// Stores the item at the end of the buffer.
    /// Panics if there is not enough free space.
    fn push(&mut self, item: Self::Item);
    /// Returns the oldest item inside the buffer.
    /// Panics if there is no available item.
    fn pop(&mut self) -> Self::Item;
}

/// An array-backed Ring Buffer
///
/// `A` is the type of the backing array. The backing array must be a real
/// array. In order to verify this it must satisfy the [`RealArray`] constraint.
/// In order to create a Ring Buffer backed by an array of 5 integer elements,
/// the following code can be utilized:
///
/// ```
/// use futures_intrusive::buffer::{ArrayRingBuf, RingBuf};
///
/// type Buffer5 = ArrayRingBuf<i32, [i32; 5]>;
/// let buffer = Buffer5::new();
/// ```
pub struct ArrayRingBuf<T, A>
where A: core::convert::AsMut<[T]> + core::convert::AsRef<[T]> + RealArray<T> {
    buffer: MaybeUninit<A>,
    size: usize,
    recv_idx: usize,
    send_idx: usize,
    _phantom: PhantomData<T>,
}

impl<T, A> core::fmt::Debug for ArrayRingBuf<T, A>
where A: core::convert::AsMut<[T]> + core::convert::AsRef<[T]> + RealArray<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> Result<(), core::fmt::Error> {
        f.debug_struct("ArrayRingBuf")
            .field("size", &self.size)
            .field("cap",&self.capacity())
            .finish()
    }
}

impl<T, A> ArrayRingBuf<T, A>
where A: core::convert::AsMut<[T]> + core::convert::AsRef<[T]> + RealArray<T> {
    fn next_idx(&mut self, last_idx: usize) -> usize {
        if last_idx + 1 == self.capacity() {
            return 0;
        }
        last_idx + 1
    }
}

impl<T, A> RingBuf for ArrayRingBuf<T, A>
where A: core::convert::AsMut<[T]> + core::convert::AsRef<[T]> + RealArray<T> {
    type Item = T;

    fn new() -> Self {
        ArrayRingBuf {
            buffer: MaybeUninit::uninit(),
            send_idx: 0,
            recv_idx: 0,
            size: 0,
            _phantom: PhantomData,
        }
    }

    fn with_capacity(_cap: usize) -> Self {
        // The fixed size array backed Ring Buffer doesn't support an adjustable
        // capacity. Therefore only the default capacity is utilized.
        Self::new()
    }

    #[inline]
    fn capacity(&self) -> usize {
        A::LEN
    }

    #[inline]
    fn len(&self) -> usize {
        self.size
    }

    #[inline]
    fn can_push(&self) -> bool {
        self.len() != self.capacity()
    }

    #[inline]
    fn push(&mut self, value: Self::Item) {
        assert!(self.can_push());
        // Safety: We asserted that there is available space for an item.
        // Therefore the memory address is valid.
        unsafe {
            let arr_ptr = self.buffer.as_mut_ptr() as *mut T;
            arr_ptr.add(self.send_idx).write(value);
        }
        self.send_idx = self.next_idx(self.send_idx);
        self.size += 1;
    }

    #[inline]
    fn pop(&mut self) -> Self::Item {
        assert!(self.size > 0);
        // Safety: We asserted that there is an element available, so it must
        // have been written before.
        let val = unsafe {
            let arr_ptr = self.buffer.as_mut_ptr() as *mut T;
            arr_ptr.add(self.recv_idx).read()
        };
        self.recv_idx = self.next_idx(self.recv_idx);
        self.size -= 1;
        val
    }
}

impl<T, A> Drop for ArrayRingBuf<T, A>
where
    A: core::convert::AsMut<[T]> + core::convert::AsRef<[T]> + RealArray<T>
{
    fn drop(&mut self) {
        // Drop all elements which are still stored inside the buffer
        while self.size > 0 {
            // Safety: This drops only as many elements as have been written via
            // ptr::write and haven't read via ptr::read before
            unsafe {
                let arr_ptr = self.buffer.as_mut_ptr() as *mut T;
                arr_ptr.add(self.recv_idx).drop_in_place();
            }
            self.recv_idx = self.next_idx(self.recv_idx);
            self.size -= 1;
        }
    }
}

#[cfg(feature = "std")]
mod if_std {
    use super::*;
    use std::collections::VecDeque;

    /// A Ring Buffer which stores all items on the heap.
    pub struct HeapRingBuf<T> {
        buffer: VecDeque<T>,
        /// The capacity is stored extra, since VecDeque can allocate space for
        /// more elements than specified.
        cap: usize,
    }

    impl<T> core::fmt::Debug for HeapRingBuf<T> {
        fn fmt(&self, f: &mut core::fmt::Formatter) -> Result<(), core::fmt::Error> {
            f.debug_struct("HeapRingBuf")
                .field("size", &self.buffer.len())
                .field("cap", &self.cap)
                .finish()
        }
    }

    impl<T> RingBuf for HeapRingBuf<T> {
        type Item = T;

        fn new() -> Self {
            HeapRingBuf {
                buffer: VecDeque::new(),
                cap: 0,
            }
        }

        fn with_capacity(cap: usize) -> Self {
            HeapRingBuf {
                buffer: VecDeque::with_capacity(cap),
                cap,
            }
        }

        #[inline]
        fn capacity(&self) -> usize {
            self.cap
        }

        #[inline]
        fn len(&self) -> usize {
            self.buffer.len()
        }

        #[inline]
        fn can_push(&self) -> bool {
            self.buffer.len() != self.cap
        }

        #[inline]
        fn push(&mut self, value: Self::Item) {
            assert!(self.can_push());
            self.buffer.push_back(value);
        }

        #[inline]
        fn pop(&mut self) -> Self::Item {
            assert!(self.buffer.len() > 0);
            self.buffer.pop_front().unwrap()
        }
    }
}

#[cfg(feature = "std")]
pub use if_std::*;

#[cfg(test)]
#[cfg(feature = "std")]
mod tests {
    use super::*;
    use crate::buffer::ring_buffer::if_std::HeapRingBuf;

    fn test_ring_buf<Buf: RingBuf<Item=u32>>(mut buf: Buf) {
        assert_eq!(5, buf.capacity());
        assert_eq!(0, buf.len());
        assert_eq!(true, buf.is_empty());
        assert_eq!(true, buf.can_push());

        buf.push(1);
        buf.push(2);
        buf.push(3);
        assert_eq!(5, buf.capacity());
        assert_eq!(3, buf.len());
        assert_eq!(false, buf.is_empty());
        assert_eq!(true, buf.can_push());

        assert_eq!(1, buf.pop());
        assert_eq!(2, buf.pop());
        assert_eq!(1, buf.len());
        assert_eq!(false, buf.is_empty());
        assert_eq!(3, buf.pop());
        assert_eq!(0, buf.len());
        assert_eq!(true, buf.is_empty());

        for (i, val) in [4,5,6,7,8].iter().enumerate() {
            buf.push(*val);
            assert_eq!(i+1, buf.len());
            assert_eq!(i != 4, buf.can_push());
            assert_eq!(false, buf.is_empty());
        }

        for (i, val) in [4,5,6,7,8].iter().enumerate() {
            assert_eq!(*val, buf.pop());
            assert_eq!(4-i, buf.len());
            assert_eq!(true, buf.can_push());
            assert_eq!(i == 4, buf.is_empty());
        }
    }

    #[test]
    fn test_array_ring_buf() {
        let buf = ArrayRingBuf::<u32, [u32; 5]>::new();
        test_ring_buf(buf);
    }

    #[test]
    fn test_heap_ring_buf() {
        let buf = HeapRingBuf::<u32>::with_capacity(5);
        test_ring_buf(buf);
    }
}