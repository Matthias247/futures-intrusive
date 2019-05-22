//! An unsafe (non-thread-safe) lock, equivalent to UnsafeCell

use lock_api::{RawMutex, GuardSend};
use core::marker::PhantomData;

/// An unsafe (non-thread-safe) lock, equivalent to UnsafeCell
#[derive(Debug)]
pub struct NoopLock{
    /// Assigned in order to make the type !Sync
    _phantom: PhantomData<*mut()>,
}

unsafe impl RawMutex for NoopLock {
    const INIT: NoopLock = NoopLock{
        _phantom: PhantomData,
    };

    type GuardMarker = GuardSend;

    fn lock(&self) {
    }

    fn try_lock(&self) -> bool {
        true
    }

    fn unlock(&self) {
    }
}