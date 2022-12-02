//! Buffer types

mod ring_buffer;
pub use ring_buffer::{ArrayBuf, RingBuf};

#[cfg(feature = "alloc")]
pub use ring_buffer::FixedHeapBuf;
#[cfg(feature = "alloc")]
pub use ring_buffer::GrowingHeapBuf;
