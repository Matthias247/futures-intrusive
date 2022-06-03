//! An asynchronously awaitable event for signalization between tasks

use crate::{utils::update_waker_ref, NoopLock};
use core::pin::Pin;
use futures_core::{
    future::{FusedFuture, Future},
    task::{Context, Poll, Waker},
};
use lock_api::{Mutex, RawMutex};
use pin_list::PinList;
use pin_project_lite::pin_project;

type PinListTypes = dyn pin_list::Types<
    Id = pin_list::id::Checked,
    // The waker that will wake this task in the list.
    Protected = Waker,
    Removed = (),
    Unprotected = (),
>;

/// Internal state of the `ManualResetEvent` pair above
struct EventState {
    is_set: bool,
    waiters: PinList<PinListTypes>,
}

impl EventState {
    fn new(is_set: bool) -> EventState {
        EventState {
            is_set,
            waiters: PinList::new(pin_list::id::Checked::new()),
        }
    }

    fn reset(&mut self) {
        self.is_set = false;
    }

    fn set(&mut self) {
        if self.is_set != true {
            self.is_set = true;

            // Wakeup all waiters
            // This happens inside the lock to make cancellation reliable
            // If we would access waiters outside of the lock, the pointers
            // may no longer be valid.
            // Typically this shouldn't be an issue, since waking a task should
            // only move it from the blocked into the ready state and not have
            // further side effects.

            let mut cursor = self.waiters.cursor_front_mut();
            while cursor.remove_current_with_or(|waker| waker.wake(), || {}) {}
        }
    }

    fn is_set(&self) -> bool {
        self.is_set
    }

    /// Checks if the event is set. If it is this returns immediately.
    /// If the event isn't set, the WaitForEventFuture gets added to the wait
    /// queue at the event, and will be signalled once ready.
    fn try_wait(
        &mut self,
        mut wait_node: Pin<&mut pin_list::Node<PinListTypes>>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        match wait_node.as_mut().initialized_mut() {
            // We haven't started waiting yet
            None => {
                if self.is_set {
                    // The event is already signaled
                    Poll::Ready(())
                } else {
                    // Added the task to the wait queue
                    self.waiters.push_back(wait_node, cx.waker().clone(), ());
                    Poll::Pending
                }
            }
            // We are in the middle of waiting, or we have been woken
            Some(node) => {
                match node.take_removed(&self.waiters) {
                    // The WaitForEventFuture is still in the queue.
                    // The event can't have been set, since this would change the
                    // waitstate inside the mutex. However the caller might have
                    // passed a different `Waker`. In this case we need to update it.
                    Err(node) => {
                        let waker =
                            node.protected_mut(&mut self.waiters).unwrap();
                        update_waker_ref(waker, cx);
                        Poll::Pending
                    }
                    // We have been woken up by the event.
                    // This does not guarantee that the event is still set. It could
                    // have been reset it in the meantime.
                    Ok(((), ())) => Poll::Ready(()),
                }
            }
        }
    }

    fn remove_waiter(
        &mut self,
        wait_node: Pin<&mut pin_list::Node<PinListTypes>>,
    ) {
        // WaitForEventFuture only needs to get removed if it has been added to
        // the wait queue of the Event.
        if let Some(node) = wait_node.initialized_mut() {
            let _ = node.reset(&mut self.waiters);
        }
    }
}

/// A synchronization primitive which can be either in the set or reset state.
///
/// Tasks can wait for the event to get set by obtaining a Future via `wait`.
/// This Future will get fulfilled when the event has been set.
pub struct GenericManualResetEvent<MutexType: RawMutex> {
    inner: Mutex<MutexType, EventState>,
}

impl<MutexType: RawMutex> core::fmt::Debug
    for GenericManualResetEvent<MutexType>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("ManualResetEvent").finish()
    }
}

impl<MutexType: RawMutex> GenericManualResetEvent<MutexType> {
    /// Creates a new ManualResetEvent in the given state
    pub fn new(is_set: bool) -> GenericManualResetEvent<MutexType> {
        GenericManualResetEvent {
            inner: Mutex::<MutexType, EventState>::new(EventState::new(is_set)),
        }
    }

    /// Sets the event.
    ///
    /// Setting the event will notify all pending waiters.
    pub fn set(&self) {
        self.inner.lock().set()
    }

    /// Resets the event.
    pub fn reset(&self) {
        self.inner.lock().reset()
    }

    /// Returns whether the event is set
    pub fn is_set(&self) -> bool {
        self.inner.lock().is_set()
    }

    /// Returns a future that gets fulfilled when the event is set.
    pub fn wait(&self) -> GenericWaitForEventFuture<MutexType> {
        GenericWaitForEventFuture {
            event: Some(self),
            wait_node: pin_list::Node::new(),
        }
    }

    fn try_wait(
        &self,
        wait_node: Pin<&mut pin_list::Node<PinListTypes>>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        self.inner.lock().try_wait(wait_node, cx)
    }

    fn remove_waiter(&self, wait_node: Pin<&mut pin_list::Node<PinListTypes>>) {
        self.inner.lock().remove_waiter(wait_node)
    }
}

pin_project! {
    /// A Future that is resolved once the corresponding ManualResetEvent has been set
    #[must_use = "futures do nothing unless polled"]
    pub struct GenericWaitForEventFuture<'a, MutexType: RawMutex> {
        // The ManualResetEvent that is associated with this WaitForEventFuture
        event: Option<&'a GenericManualResetEvent<MutexType>>,
        // Node for waiting at the event
        #[pin]
        wait_node: pin_list::Node<PinListTypes>,
    }

    impl<MutexType: RawMutex> PinnedDrop for GenericWaitForEventFuture<'_, MutexType> {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();

            // If this WaitForEventFuture has been polled and it was added to the
            // wait queue at the event, it must be removed before dropping.
            // Otherwise the event would access invalid memory.
            if let Some(ev) = this.event {
                ev.remove_waiter(this.wait_node);
            }
        }
    }
}

impl<'a, MutexType: RawMutex> core::fmt::Debug
    for GenericWaitForEventFuture<'a, MutexType>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("GenericWaitForEventFuture").finish()
    }
}

impl<'a, MutexType: RawMutex> Future
    for GenericWaitForEventFuture<'a, MutexType>
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.project();

        let event = this
            .event
            .expect("polled WaitForEventFuture after completion");

        let poll_res = event.try_wait(this.wait_node, cx);

        if let Poll::Ready(()) = poll_res {
            // The event was set
            *this.event = None;
        }

        poll_res
    }
}

impl<'a, MutexType: RawMutex> FusedFuture
    for GenericWaitForEventFuture<'a, MutexType>
{
    fn is_terminated(&self) -> bool {
        self.event.is_none()
    }
}

// Export a non thread-safe version using NoopLock

/// A [`GenericManualResetEvent`] which is not thread-safe.
pub type LocalManualResetEvent = GenericManualResetEvent<NoopLock>;
/// A [`GenericWaitForEventFuture`] for [`LocalManualResetEvent`].
pub type LocalWaitForEventFuture<'a> = GenericWaitForEventFuture<'a, NoopLock>;

#[cfg(feature = "std")]
mod if_std {
    use super::*;

    // Export a thread-safe version using parking_lot::RawMutex

    /// A [`GenericManualResetEvent`] implementation backed by [`parking_lot`].
    pub type ManualResetEvent = GenericManualResetEvent<parking_lot::RawMutex>;
    /// A [`GenericWaitForEventFuture`] for [`ManualResetEvent`].
    pub type WaitForEventFuture<'a> =
        GenericWaitForEventFuture<'a, parking_lot::RawMutex>;
}

#[cfg(feature = "std")]
pub use self::if_std::*;
