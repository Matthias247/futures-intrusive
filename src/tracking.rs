//! Tracker types.
//!
//! Tracker is used to collect statistics about data traveling across the channels.

use core::fmt;

/// Trait for tracking statistics
pub trait Tracker<T> {
    /// Type of report, which is returned by `report` function
    type Report: fmt::Debug;

    /// This is called when item is received by channel, right
    /// before getting into the underlying `RingBuf`
    fn track_incoming(&mut self, item: &T);

    /// This is called right after item is taken from underlying
    /// `RingBuf` and returned to the receiver
    fn track_outgoing(&mut self, item: &T);

    /// Get the report
    fn report(&mut self) -> Self::Report;
}

/// Tracker which does nothing
#[derive(Debug)]
pub struct NoopTracker;

impl<T> Tracker<T> for NoopTracker {
    type Report = ();

    #[inline]
    fn track_incoming(&mut self, _item: &T) {}

    #[inline]
    fn track_outgoing(&mut self, _item: &T) {}

    fn report(&mut self) -> () {}
}

impl Default for NoopTracker {
    fn default() -> Self {
        NoopTracker
    }
}
