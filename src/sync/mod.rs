//! Asynchronous synchronization primitives based on intrusive collections.
//!
//! This module provides various primitives for synchronizing concurrently
//! executing futures.

mod manual_reset_event;

pub use self::manual_reset_event::{
    GenericManualResetEvent, WaitForEventFuture,
    LocalManualResetEvent,
};

#[cfg(feature = "std")]
pub use self::manual_reset_event::{
    ManualResetEvent,
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

