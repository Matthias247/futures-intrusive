//! Asynchronous synchronization primitives based on intrusive collections.
//!
//! This module provides various primitives for synchronizing concurrently
//! executing futures.

mod manual_reset_event;

pub use self::manual_reset_event::{
    GenericManualResetEvent, GenericWaitForEventFuture,
    LocalManualResetEvent, LocalWaitForEventFuture,
};

#[cfg(feature = "std")]
pub use self::manual_reset_event::{
    ManualResetEvent, WaitForEventFuture,
};

mod mutex;

pub use self::mutex::{
    GenericMutex, GenericMutexLockFuture, GenericMutexGuard,
    LocalMutex, LocalMutexLockFuture, LocalMutexGuard,
};

#[cfg(feature = "std")]
pub use self::mutex::{
    Mutex, MutexLockFuture, MutexGuard,
};

mod semaphore;

pub use self::semaphore::{
    GenericSemaphore, GenericSemaphoreAcquireFuture, GenericSemaphoreReleaser,
    LocalSemaphore, LocalSemaphoreAcquireFuture, LocalSemaphoreReleaser,
};

#[cfg(feature = "std")]
pub use self::semaphore::{
    Semaphore, SemaphoreAcquireFuture, SemaphoreReleaser,
};



// The next section should really integrated if the alloc feature is active,
// since it mainly requires `Arc` to be available. However for simplicity reasons
// it is currently only activated in std environments.
#[cfg(feature = "std")]
mod if_alloc {

    /// Semaphore is cloneable. The Futures produced by the semaphore in this
    /// module don't require a lifetime parameter.
    pub mod shared {
        pub use super::super::semaphore::shared::*;
    }
}

#[cfg(feature = "std")]
pub use self::if_alloc::*;
