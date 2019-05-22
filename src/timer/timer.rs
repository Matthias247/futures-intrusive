//! An asynchronously awaitable timer

use futures_core::future::{Future, FusedFuture};
use futures_core::task::{Context, Poll, Waker};
use core::pin::Pin;
use core::time::{Duration};
use lock_api::{RawMutex, Mutex};
use super::clock::Clock;
use crate::NoopLock;
use crate::intrusive_double_linked_list::{LinkedList, ListNode};

/// Tracks how the future had interacted with the timer
#[derive(PartialEq)]
enum PollState {
    /// The task is not registered at the wait queue at the timer
    Unregistered,
    /// The task was added to the wait queue at the timer
    Registered,
    /// The timer has expired and was thereby removed from the wait queue at
    /// the timer. Having this extra state avoids to query the clock for an
    /// extra time.
    Expired,
}

/// Tracks the timer futures waiting state.
struct TimerQueueEntry {
    /// Timestamp when the timer expires
    expiry: u64,
    /// The task handle of the waiting task
    task: Option<Waker>,
    /// Current polling state
    state: PollState,
}

impl TimerQueueEntry {
    /// Creates a new TimerQueueEntry
    fn new(expiry: u64) -> TimerQueueEntry {
        TimerQueueEntry {
            expiry,
            task: None,
            state: PollState::Unregistered,
        }
    }
}

impl PartialEq for TimerQueueEntry {
    fn eq(&self, other: &TimerQueueEntry) -> bool {
        // This is technically not correct. However for the usage in this module
        // we only need to compare timers by expiration.
        self.expiry == other.expiry
    }
}

impl PartialOrd for TimerQueueEntry {
    fn partial_cmp(&self, other: &TimerQueueEntry) -> Option<core::cmp::Ordering> {
        // Compare timer queue entries by expiration time
        self.expiry.partial_cmp(&other.expiry)
    }
}

/// Internal state of the timer
struct TimerState {
    /// The clock which is utilized
    clock: &'static dyn Clock,
    /// The list of waiters, which are waiting for their timer to expire
    waiters: LinkedList<TimerQueueEntry>,
}

impl TimerState {
    fn new(clock: &'static dyn Clock) -> TimerState {
        TimerState {
            clock,
            waiters: LinkedList::new(),
        }
    }

    /// Registers the timer future at the Timer.
    /// This function is only safe as long as the `wait_node`s address is guaranteed
    /// to be stable until it gets removed from the queue.
    unsafe fn try_wait(
        &mut self,
        wait_node: &mut ListNode<TimerQueueEntry>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        match wait_node.state {
            PollState::Unregistered => {
                let now = self.clock.now();
                if now >= wait_node.expiry {
                    // The timer is already expired
                    wait_node.state = PollState::Expired;
                    Poll::Ready(())
                }
                else {
                    // Added the task to the wait queue
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = PollState::Registered;
                    self.waiters.add_sorted(wait_node);
                    Poll::Pending
                }
            },
            PollState::Registered => {
                // Since the timer wakes up all waiters and moves their states to
                // Expired when the timer expired, it can't be expired here yet
                Poll::Pending
            },
            PollState::Expired => {
                Poll::Ready(())
            },
        }
    }

    fn remove_waiter(&mut self, wait_node: &mut ListNode<TimerQueueEntry>) {
        // TimerFuture only needs to get removed if it had been added to
        // the wait queue of the timer. This has happened in the PollState::Registered case.
        if let PollState::Registered = wait_node.state {
            if ! unsafe { self.waiters.remove(wait_node) } {
                // Panic if the address isn't found. This can only happen if the contract was
                // violated, e.g. the TimerQueueEntry got moved after the initial poll.
                panic!("Future could not be removed from wait queue");
            }
            wait_node.state = PollState::Unregistered;
        }
    }

    /// Returns a timestamp when the next timer expires.
    ///
    /// For thread-safe timers, the returned value is not precise and subject to
    /// race-conditions, since other threads can add timer in the meantime.
    fn next_expiration(&self) -> Option<u64> {
        let first = self.waiters.peek_first();
        if first.is_null() {
            return None;
        }
        Some(unsafe {(*first).expiry})
    }

    /// Checks whether any of the attached Futures is expired
    fn check_expirations(&mut self) {
        unsafe {
            let now = self.clock.now();
            loop {
                let first = self.waiters.peek_first();
                if first.is_null() {
                    return;
                }
                let first_expiry = (*first).expiry;
                if now >= first_expiry {
                    // The timer is expired.
                    (*first).state = PollState::Expired;
                    if let Some(task) = (*first).task.take() {
                        task.wake();
                    }
                    self.waiters.remove(first);
                }
                else {
                    // Remaining timers are not expired
                    break;
                }
            }
        }
    }
}

/// Adapter trait that allows Futures to generically interact with timer
/// implementations via dynamic dispatch.
trait TimerAccess {
    unsafe fn try_wait(
        &self,
        wait_node: &mut ListNode<TimerQueueEntry>,
        cx: &mut Context<'_>,
    ) -> Poll<()>;

    fn remove_waiter(&self, wait_node: &mut ListNode<TimerQueueEntry>);
}

/// An asynchronously awaitable timer.
///
/// The timer operates on millisecond precision and makes use of a configurable
/// clock source.
///
/// The timer allows to wait asynchronously either for a certain duration,
/// or until the provided [`Clock`] reaches a certain timestamp.
pub trait Timer {
    /// Returns a future that gets fulfilled after the given `Duration`
    fn delay(&self, delay: Duration) -> TimerFuture;

    /// Returns a future that gets fulfilled when the utilized [`Clock`] reaches
    /// the given timestamp.
    fn deadline(&self, timestamp: u64) -> TimerFuture;
}

/// An asynchronously awaitable timer.
///
/// The timer operates on millisecond precision and makes use of a configurable
/// clock source.
///
/// The timer allows to wait asynchronously either for a certain duration,
/// or until the provided [`Clock`] reaches a certain timestamp.
///
/// In order to unblock tasks that are waiting on the timer,
/// [`check_expirations`](GenericTimerService::check_expirations)
/// must be called in regular intervals on this timer service.
///
/// The timer can either be running on a separate timer thread (in case a
/// thread-safe timer type is utilize), or it can be integrated into an executor
/// in order to minimize context switches.
pub struct GenericTimerService<MutexType: RawMutex> {
    inner: Mutex<MutexType, TimerState>,
}

// The timer can be sent to other threads as long as it's not borrowed
unsafe impl<MutexType: RawMutex+Send> Send for GenericTimerService<MutexType> {}

impl<MutexType: RawMutex> core::fmt::Debug for GenericTimerService<MutexType> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("TimerService")
            .finish()
    }
}

impl<MutexType: RawMutex> GenericTimerService<MutexType> {
    /// Creates a new Timer in the given state.
    ///
    /// The Timer will query the provided [`Clock`] instance for the current
    /// time whenever required.
    ///
    /// In order to create a create a clock which utilizes system time,
    /// [`StdClock`](super::StdClock) can be utilized.
    /// In order to simulate time for test purposes,
    /// [`MockClock`](super::MockClock) can be utilized.
    pub fn new(clock: &'static dyn Clock) -> GenericTimerService<MutexType> {
        GenericTimerService::<MutexType> {
            inner: Mutex::new(TimerState::new(clock)),
        }
    }

    /// Returns a timestamp when the next timer expires.
    ///
    /// For thread-safe timers, the returned value is not precise and subject to
    /// race-conditions, since other threads can add timer in the meantime.
    ///
    /// Therefore adding any timer to the [`GenericTimerService`] should  also
    /// make sure to wake up the executor which polls for timeouts, in order to
    /// let it capture the latest change.
    pub fn next_expiration(&self) -> Option<u64> {
        self.inner.lock().next_expiration()
    }

    /// Checks whether any of the attached [`TimerFuture`]s has expired.
    /// In this case the associated task is woken up.
    pub fn check_expirations(&self) {
        self.inner.lock().check_expirations()
    }
}

impl<MutexType: RawMutex> Timer for GenericTimerService<MutexType> {
    /// Returns a future that gets fulfilled after the given [`Duration`]
    fn delay(&self, delay: Duration) -> TimerFuture {
        // TODO: Make this more efficient and with better overflow checking
        let now = self.inner.lock().clock.now() as u128;
        let delay_ms = core::cmp::min(delay.as_millis(), core::u64::MAX as u128);
        let expiry = core::cmp::min(now + delay_ms, core::u64::MAX as u128) as u64;

        TimerFuture {
            timer: Some(self),
            wait_node: ListNode::new(TimerQueueEntry::new(expiry)),
        }
    }

    /// Returns a future that gets fulfilled when the utilized [`Clock`] reaches
    /// the given timestamp.
    fn deadline(&self, timestamp: u64) -> TimerFuture {
        TimerFuture {
            timer: Some(self),
            wait_node: ListNode::new(TimerQueueEntry::new(timestamp)),
        }
    }
}

impl<MutexType: RawMutex> TimerAccess for GenericTimerService<MutexType> {
    unsafe fn try_wait(
        &self,
        wait_node: &mut ListNode<TimerQueueEntry>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        self.inner.lock().try_wait(wait_node, cx)
    }

    fn remove_waiter(&self, wait_node: &mut ListNode<TimerQueueEntry>) {
        self.inner.lock().remove_waiter(wait_node)
    }
}

/// A Future that is resolved once the requested time has elapsed.
#[must_use = "futures do nothing unless polled"]
pub struct TimerFuture<'a> {
    /// The Timer that is associated with this TimerFuture
    timer: Option<&'a dyn TimerAccess>,
    /// Node for waiting on the timer
    wait_node: ListNode<TimerQueueEntry>,
}

impl<'a> core::fmt::Debug for TimerFuture<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("TimerFuture")
            .finish()
    }
}

impl<'a> Future for TimerFuture<'a> {
    type Output = ();

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        // It might be possible to use Pin::map_unchecked here instead of the two unsafe APIs.
        // However this didn't seem to work for some borrow checker reasons

        // Safety: The next operations are safe, because Pin promises us that
        // the address of the wait queue entry inside TimerFuture is stable,
        // and we don't move any fields inside the future until it gets dropped.
        let mut_self: &mut TimerFuture = unsafe {
            Pin::get_unchecked_mut(self)
        };

        let timer = mut_self.timer.expect("polled TimerFuture after completion");

        let poll_res = unsafe {
            timer.try_wait(&mut mut_self.wait_node, cx)
        };

        if poll_res.is_ready() {
            // A value was available
            mut_self.timer = None;
        }

        poll_res
    }
}

impl<'a> FusedFuture for TimerFuture<'a> {
    fn is_terminated(&self) -> bool {
        self.timer.is_none()
    }
}

impl<'a> Drop for TimerFuture<'a> {
    fn drop(&mut self) {
        // If this TimerFuture has been polled and it was added to the
        // wait queue at the timer, it must be removed before dropping.
        // Otherwise the timer would access invalid memory.
        if let Some(timer) = self.timer {
            timer.remove_waiter(&mut self.wait_node);
        }
    }
}

// Export a non thread-safe version using NoopLock

/// A [`GenericTimerService`] implementation which is not thread-safe.
pub type LocalTimerService = GenericTimerService<NoopLock>;

#[cfg(feature = "std")]
mod if_std {
    use super::*;

    // Export a thread-safe version using parking_lot::RawMutex

    /// A [`GenericTimerService`] implementation backed by [`parking_lot`].
    pub type TimerService = GenericTimerService<parking_lot::RawMutex>;
    // The timer is thread-safe and can be sent to other threads.
    unsafe impl Sync for TimerService {}
}

#[cfg(feature = "std")]
pub use self::if_std::*;