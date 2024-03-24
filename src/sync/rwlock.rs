//! An asynchronously awaitable mutex for synchronization between concurrently
//! executing futures.

use crate::{
    intrusive_double_linked_list::{ControlFlow, LinkedList, ListNode},
    utils::update_waker_ref,
    NoopLock,
};
use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    pin::Pin,
};
use futures_core::{
    future::{FusedFuture, Future},
    task::{Context, Poll, Waker},
};
use lock_api::{Mutex as LockApiMutex, RawMutex};

/// Tracks how the future had interacted with the mutex
#[derive(PartialEq)]
enum PollState {
    /// The task has never interacted with the mutex.
    New,

    /// The task was added to the wait queue at the mutex.
    Waiting,

    /// The task had previously waited on the mutex, but was notified
    /// that the mutex was released in the meantime.
    Notified,

    /// The task had been polled to completion.
    Done,
}

/// The kind of entry.
#[derive(Debug, PartialEq)]
enum EntryKind {
    /// An exclusive write access.
    Write,

    /// A shared read access.
    Read,

    /// A upgradable shared read access.
    UpgradeRead,
}

/// Tracks the MutexLockFuture waiting state.
/// Access to this struct is synchronized through the mutex in the Event.
struct Entry {
    /// The task handle of the waiting task
    task: Option<Waker>,

    /// Current polling state
    state: PollState,

    /// The kind of entry.
    kind: EntryKind,
}

impl Entry {
    /// Creates a new Entry
    fn new(kind: EntryKind) -> Entry {
        Entry {
            task: None,
            state: PollState::New,
            kind,
        }
    }

    /// Update the state & wakeup if possible.
    fn notify(&mut self) {
        self.state = PollState::Notified;

        if let Some(waker) = self.task.take() {
            waker.wake();
        }
    }
}

/// Internal state of the `Mutex`
struct MutexState {
    is_fair: bool,
    nb_reads: usize,
    has_upgrade_read: bool,
    has_write: bool,
    waiters: LinkedList<Entry>,
    nb_waiting_writes: usize,
    nb_waiting_reads: usize,
    nb_waiting_upgrade_reads: usize,
}

impl MutexState {
    fn new(is_fair: bool) -> Self {
        MutexState {
            is_fair,
            nb_reads: 0,
            has_upgrade_read: false,
            has_write: false,
            waiters: LinkedList::new(),
            nb_waiting_writes: 0,
            nb_waiting_reads: 0,
            nb_waiting_upgrade_reads: 0,
        }
    }

    /// Reduce the number of reads on the lock by one, waking
    /// up a write if needed.
    ///
    /// If the Mutex is not fair, removes the woken up node from
    /// the wait queue
    fn unlock_read(&mut self) {
        self.nb_reads -= 1;
        if self.nb_reads == 0 {
            // Wakeup the next write in line.
            if self.is_fair {
                self.waiters.reverse_apply_while(|entry| {
                    if let EntryKind::Write = entry.kind {
                        entry.notify();
                        ControlFlow::Stop
                    } else {
                        ControlFlow::Continue
                    }
                });
            } else {
                self.waiters.reverse_apply_while(|entry| {
                    if let EntryKind::Write = entry.kind {
                        entry.notify();
                        ControlFlow::RemoveAndStop
                    } else {
                        ControlFlow::Continue
                    }
                });
            };
        }
    }

    /// Release the exclusive writer lock, waking up entries
    /// if needed.
    ///
    /// If the Mutex is not fair, removes the woken up node from
    /// the wait queue
    fn unlock_write(&mut self) {
        self.has_write = false;
        debug_assert_eq!(self.nb_reads, 0);

        self.wakeup_any_waiters()
    }

    /// Release the upgradable reader lock, waking up entries
    /// if needed.
    ///
    /// If the Mutex is not fair, removes the woken up node from
    /// the wait queue
    fn unlock_upgrade_read(&mut self) {
        self.has_upgrade_read = false;
        self.nb_reads -= 1;

        if self.nb_reads == 0 {
            self.wakeup_any_waiters();
        }
    }

    /// Release the upgradable reader lock to upgrade
    /// it into a writer lock, waking up entries if needed.
    ///
    /// If the Mutex is not fair does not wakeup anyone.
    fn unlock_upgrade_read_for_upgrade(&mut self) {
        self.has_upgrade_read = false;
        self.nb_reads -= 1;

        if self.nb_reads == 0 && self.is_fair {
            self.wakeup_any_waiters();
        }
    }

    fn wakeup_any_waiters(&mut self) {
        let mut nb_reads = 0;
        if self.is_fair {
            self.waiters.reverse_apply_while(|entry| match entry.kind {
                EntryKind::Read | EntryKind::UpgradeRead => {
                    entry.notify();
                    nb_reads += 1;
                    ControlFlow::Continue
                }
                EntryKind::Write => {
                    if nb_reads == 0 {
                        entry.notify();
                        ControlFlow::Stop
                    } else {
                        ControlFlow::Continue
                    }
                }
            });
        } else {
            self.waiters.reverse_apply_while(|entry| match entry.kind {
                EntryKind::Read | EntryKind::UpgradeRead => {
                    entry.notify();
                    nb_reads += 1;
                    ControlFlow::RemoveAndContinue
                }
                EntryKind::Write => {
                    if nb_reads == 0 {
                        entry.notify();
                        ControlFlow::RemoveAndStop
                    } else {
                        ControlFlow::Continue
                    }
                }
            });
        };
    }

    /// Attempt To gain shared read access.
    ///
    /// Returns true if the access is obtained.
    fn try_lock_read_sync(&mut self) -> bool {
        // The lock can only be obtained synchronously if
        // - has no write
        // - the Semaphore is either not fair, or there are no waiting writes.
        if !self.has_write && (!self.is_fair || self.nb_waiting_writes == 0) {
            self.nb_reads += 1;
            true
        } else {
            false
        }
    }

    /// Attempt To gain upgradable shared read access.
    ///
    /// Returns true if the access is obtained.
    fn try_lock_upgrade_read_sync(&mut self) -> bool {
        // The lock can only be obtained synchronously if
        // - has no write
        // - the Semaphore is either not fair, or there are no waiting writes
        //   or upgradable reads
        if !self.has_write
            && !self.has_upgrade_read
            && (!self.is_fair
                || (self.nb_waiting_writes == 0
                    && self.nb_waiting_upgrade_reads == 0))
        {
            self.nb_reads += 1;
            self.has_upgrade_read = true;
            true
        } else {
            false
        }
    }

    /// Attempt to gain exclusive write access.
    ///
    /// Returns true if the access is obtained.
    fn try_lock_write_sync(&mut self) -> bool {
        // The lock can only be obtained synchronously if
        // - has no write
        // - has no read
        // - the Semaphore is either not fair, or there are no waiting writes.
        if !self.has_write
            && self.nb_reads == 0
            && (!self.is_fair
                || (self.nb_waiting_writes == 0
                    && self.nb_waiting_upgrade_reads == 0))
        {
            self.has_write = true;
            true
        } else {
            false
        }
    }

    /// Attempt to gain exclusive write access.
    ///
    /// Returns true if the access is obtained.
    fn try_upgrade_read_sync(&mut self) -> bool {
        // The lock can only be obtained synchronously if
        // - has no write
        // - has 1 read (the caller)
        // - the Semaphore is either not fair, or there are no waiting writes.
        debug_assert!(self.has_upgrade_read);
        if !self.has_write
            && self.nb_reads == 1
            && (!self.is_fair
                || (self.nb_waiting_writes == 0
                    && self.nb_waiting_upgrade_reads == 0))
        {
            self.has_write = true;
            self.nb_reads -= 1;
            self.has_upgrade_read = false;
            true
        } else {
            false
        }
    }

    /// Add a read to the wait queue.
    ///
    /// Safety: This function is only safe as long as `node` is guaranteed to
    /// get removed from the list before it gets moved or dropped.
    /// In addition to this `node` may not be added to another other list before
    /// it is removed from the current one.
    unsafe fn add_read(&mut self, wait_node: &mut ListNode<Entry>) {
        debug_assert_eq!(wait_node.kind, EntryKind::Read);
        self.waiters.add_front(wait_node);
        self.nb_waiting_reads += 1;
    }

    /// Add a write to the wait queue.
    ///
    /// Safety: This function is only safe as long as `node` is guaranteed to
    /// get removed from the list before it gets moved or dropped.
    /// In addition to this `node` may not be added to another other list before it is removed from the current one.
    unsafe fn add_write(&mut self, wait_node: &mut ListNode<Entry>) {
        debug_assert_eq!(wait_node.kind, EntryKind::Write);
        self.waiters.add_front(wait_node);
        self.nb_waiting_writes += 1;
    }

    /// Add a write to the wait queue.
    ///
    /// Safety: This function is only safe as long as `node` is guaranteed to
    /// get removed from the list before it gets moved or dropped.
    /// In addition to this `node` may not be added to another other list before
    /// it is removed from the current one.
    unsafe fn add_upgrade_read(&mut self, wait_node: &mut ListNode<Entry>) {
        debug_assert_eq!(wait_node.kind, EntryKind::UpgradeRead);
        self.waiters.add_front(wait_node);
        self.nb_waiting_upgrade_reads += 1;
    }

    /// Tries to acquire the shared read access from a Entry.
    ///
    /// If it isn't available, the Entry gets added to the wait
    /// queue at the Mutex, and will be signalled once ready.
    /// This function is only safe as long as the `wait_node`s address is guaranteed
    /// to be stable until it gets removed from the queue.
    unsafe fn try_lock_read(
        &mut self,
        wait_node: &mut ListNode<Entry>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        match wait_node.state {
            PollState::New => {
                // The fast path - the Mutex isn't locked by anyone else.
                // If the mutex is fair, noone must be in the wait list before us.
                if self.try_lock_read_sync() {
                    wait_node.state = PollState::Done;
                    Poll::Ready(())
                } else {
                    // Add the task to the wait queue
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = PollState::Waiting;
                    self.add_read(wait_node);
                    Poll::Pending
                }
            }
            PollState::Waiting => {
                // The RwLockReadFuture is already in the queue.
                if self.is_fair {
                    // The task needs to wait until it gets notified in order to
                    // maintain the ordering. However the caller might have
                    // passed a different `Waker`. In this case we need to update it.
                    update_waker_ref(&mut wait_node.task, cx);
                    Poll::Pending
                } else {
                    // For throughput improvement purposes, grab the lock immediately
                    // if it's available.
                    if !self.has_write {
                        self.nb_reads += 1;
                        wait_node.state = PollState::Done;
                        // Since this waiter has been registered before, it must
                        // get removed from the waiter list.
                        // Safety: Due to the state, we know that the node must be part
                        // of the waiter list
                        self.force_remove_read(wait_node);
                        Poll::Ready(())
                    } else {
                        // The caller might have passed a different `Waker`.
                        // In this case we need to update it.
                        update_waker_ref(&mut wait_node.task, cx);
                        Poll::Pending
                    }
                }
            }
            PollState::Notified => {
                // We had been woken by the mutex, since the mutex is available again.
                // The mutex thereby removed us from the waiters list.
                // Just try to lock again. If the mutex isn't available,
                // we need to add it to the wait queue again.
                if !self.has_write {
                    if self.is_fair {
                        // In a fair Mutex, the Entry is kept in the
                        // linked list and must be removed here
                        // Safety: Due to the state, we know that the node must be part
                        // of the waiter list
                        self.force_remove_read(wait_node);
                    }
                    self.nb_reads += 1;
                    wait_node.state = PollState::Done;
                    Poll::Ready(())
                } else {
                    // Fair mutexes should always be able to acquire the lock
                    // after they had been notified
                    debug_assert!(!self.is_fair);
                    // Add to queue
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = PollState::Waiting;
                    self.add_read(wait_node);
                    Poll::Pending
                }
            }
            PollState::Done => {
                // The future had been polled to completion before
                panic!("polled Mutex after completion");
            }
        }
    }

    /// Tries to acquire an exclusive write access from a Entry.
    ///
    /// If it isn't available, the Entry gets added to the wait
    /// queue at the Mutex, and will be signalled once ready.
    /// This function is only safe as long as the `wait_node`s address is guaranteed
    /// to be stable until it gets removed from the queue.
    unsafe fn try_lock_write(
        &mut self,
        wait_node: &mut ListNode<Entry>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        match wait_node.state {
            PollState::New => {
                // The fast path - the Mutex isn't locked by anyone else.
                // If the mutex is fair, noone must be in the wait list before us.
                if self.try_lock_write_sync() {
                    wait_node.state = PollState::Done;
                    Poll::Ready(())
                } else {
                    // Add the task to the wait queue
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = PollState::Waiting;
                    self.add_write(wait_node);
                    Poll::Pending
                }
            }
            PollState::Waiting => {
                // The RwLockReadFuture is already in the queue.
                if self.is_fair {
                    // The task needs to wait until it gets notified in order to
                    // maintain the ordering. However the caller might have
                    // passed a different `Waker`. In this case we need to update it.
                    update_waker_ref(&mut wait_node.task, cx);
                    Poll::Pending
                } else {
                    // For throughput improvement purposes, grab the lock immediately
                    // if it's available.
                    if !self.has_write && self.nb_reads == 0 {
                        self.has_write = true;
                        wait_node.state = PollState::Done;
                        // Since this waiter has been registered before, it must
                        // get removed from the waiter list.
                        // Safety: Due to the state, we know that the node must be part
                        // of the waiter list
                        self.force_remove_write(wait_node);
                        Poll::Ready(())
                    } else {
                        // The caller might have passed a different `Waker`.
                        // In this case we need to update it.
                        update_waker_ref(&mut wait_node.task, cx);
                        Poll::Pending
                    }
                }
            }
            PollState::Notified => {
                // We had been woken by the mutex, since the mutex is available again.
                // The mutex thereby removed us from the waiters list.
                // Just try to lock again. If the mutex isn't available,
                // we need to add it to the wait queue again.
                if !self.has_write && self.nb_reads == 0 {
                    if self.is_fair {
                        // In a fair Mutex, the Entry is kept in the
                        // linked list and must be removed here
                        // Safety: Due to the state, we know that the node must be part
                        // of the waiter list
                        self.force_remove_write(wait_node);
                    }
                    self.has_write = true;
                    wait_node.state = PollState::Done;
                    Poll::Ready(())
                } else {
                    // Fair mutexes should always be able to acquire the lock
                    // after they had been notified
                    debug_assert!(!self.is_fair);
                    // Add to queue
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = PollState::Waiting;
                    self.add_write(wait_node);
                    Poll::Pending
                }
            }
            PollState::Done => {
                // The future had been polled to completion before
                panic!("polled Mutex after completion");
            }
        }
    }

    /// Tries to acquire an upgradable shared read access from a Entry.
    ///
    /// If it isn't available, the Entry gets added to the wait
    /// queue at the Mutex, and will be signalled once ready.
    /// This function is only safe as long as the `wait_node`s address is guaranteed
    /// to be stable until it gets removed from the queue.
    unsafe fn try_lock_upgrade_read(
        &mut self,
        wait_node: &mut ListNode<Entry>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        match wait_node.state {
            PollState::New => {
                // The fast path - the Mutex isn't locked by anyone else.
                // If the mutex is fair, noone must be in the wait list before us.
                if self.try_lock_upgrade_read_sync() {
                    wait_node.state = PollState::Done;
                    Poll::Ready(())
                } else {
                    // Add the task to the wait queue
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = PollState::Waiting;
                    self.add_upgrade_read(wait_node);
                    Poll::Pending
                }
            }
            PollState::Waiting => {
                // The RwLockReadFuture is already in the queue.
                if self.is_fair {
                    // The task needs to wait until it gets notified in order to
                    // maintain the ordering. However the caller might have
                    // passed a different `Waker`. In this case we need to update it.
                    update_waker_ref(&mut wait_node.task, cx);
                    Poll::Pending
                } else {
                    // For throughput improvement purposes, grab the lock immediately
                    // if it's available.
                    if !self.has_write && !self.has_upgrade_read {
                        self.has_upgrade_read = true;
                        self.nb_reads += 1;
                        wait_node.state = PollState::Done;
                        // Since this waiter has been registered before, it must
                        // get removed from the waiter list.
                        // Safety: Due to the state, we know that the node must be part
                        // of the waiter list
                        self.force_remove_upgrade_read(wait_node);
                        Poll::Ready(())
                    } else {
                        // The caller might have passed a different `Waker`.
                        // In this case we need to update it.
                        update_waker_ref(&mut wait_node.task, cx);
                        Poll::Pending
                    }
                }
            }
            PollState::Notified => {
                // We had been woken by the mutex, since the mutex is available again.
                // The mutex thereby removed us from the waiters list.
                // Just try to lock again. If the mutex isn't available,
                // we need to add it to the wait queue again.
                if !self.has_write && !self.has_upgrade_read {
                    if self.is_fair {
                        // In a fair Mutex, the Entry is kept in the
                        // linked list and must be removed here
                        // Safety: Due to the state, we know that the node must be part
                        // of the waiter list
                        self.force_remove_upgrade_read(wait_node);
                    }
                    self.has_upgrade_read = true;
                    self.nb_reads += 1;
                    wait_node.state = PollState::Done;
                    Poll::Ready(())
                } else {
                    // Fair mutexes should always be able to acquire the lock
                    // after they had been notified
                    debug_assert!(!self.is_fair);
                    // Add to queue
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = PollState::Waiting;
                    self.add_upgrade_read(wait_node);
                    Poll::Pending
                }
            }
            PollState::Done => {
                // The future had been polled to completion before
                panic!("polled Mutex after completion");
            }
        }
    }

    /// Tries to remove a read waiter from the wait queue, and panics if the
    /// waiter is no longer valid.
    unsafe fn force_remove_read(&mut self, wait_node: &mut ListNode<Entry>) {
        if !self.waiters.remove(wait_node) {
            // Panic if the address isn't found. This can only happen if the contract was
            // violated, e.g. the Entry got moved after the initial poll.
            panic!("Future could not be removed from wait queue");
        }
        self.nb_waiting_reads -= 1;
    }

    /// Tries to remove a write waiter from the wait queue, and panics if the
    /// waiter is no longer valid.
    unsafe fn force_remove_write(&mut self, wait_node: &mut ListNode<Entry>) {
        if !self.waiters.remove(wait_node) {
            // Panic if the address isn't found. This can only happen if the contract was
            // violated, e.g. the Entry got moved after the initial poll.
            panic!("Future could not be removed from wait queue");
        }
        self.nb_waiting_writes -= 1;
    }

    /// Tries to remove a upgrade_read waiter from the wait queue, and panics if the
    /// waiter is no longer valid.
    unsafe fn force_remove_upgrade_read(
        &mut self,
        wait_node: &mut ListNode<Entry>,
    ) {
        if !self.waiters.remove(wait_node) {
            // Panic if the address isn't found. This can only happen if the contract was
            // violated, e.g. the Entry got moved after the initial poll.
            panic!("Future could not be removed from wait queue");
        }
        self.nb_waiting_upgrade_reads -= 1;
    }

    /// Removes the read from the wait list.
    ///
    /// This function is only safe as long as the reference that is passed here
    /// equals the reference/address under which the waiter was added.
    /// The waiter must not have been moved in between.
    ///
    /// Returns the `Waker` of another task which might get ready to run due to
    /// this.
    fn remove_read(&mut self, wait_node: &mut ListNode<Entry>) {
        // MutexLockFuture only needs to get removed if it had been added to
        // the wait queue of the Mutex. This has happened in the PollState::Waiting case.
        // If the current waiter was notified, another waiter must get notified now.
        match wait_node.state {
            PollState::Notified => {
                if self.is_fair {
                    // In a fair Mutex, the Entry is kept in the
                    // linked list and must be removed here
                    // Safety: Due to the state, we know that the node must be part
                    // of the waiter list
                    unsafe { self.force_remove_read(wait_node) };
                }
                wait_node.state = PollState::Done;
                // Since the task was notified but did not lock the Mutex,
                // another task gets the chance to run.
                self.unlock_read();
            }
            PollState::Waiting => {
                // Remove the Entry from the linked list
                // Safety: Due to the state, we know that the node must be part
                // of the waiter list
                unsafe { self.force_remove_read(wait_node) };
                wait_node.state = PollState::Done;
            }
            PollState::New | PollState::Done => (),
        }
    }

    /// Removes the write from the wait list.
    ///
    /// This function is only safe as long as the reference that is passed here
    /// equals the reference/address under which the waiter was added.
    /// The waiter must not have been moved in between.
    ///
    /// Returns the `Waker` of another task which might get ready to run due to
    /// this.
    fn remove_write(&mut self, wait_node: &mut ListNode<Entry>) {
        // MutexLockFuture only needs to get removed if it had been added to
        // the wait queue of the Mutex. This has happened in the PollState::Waiting case.
        // If the current waiter was notified, another waiter must get notified now.
        match wait_node.state {
            PollState::Notified => {
                if self.is_fair {
                    // In a fair Mutex, the Entry is kept in the
                    // linked list and must be removed here
                    // Safety: Due to the state, we know that the node must be part
                    // of the waiter list
                    unsafe { self.force_remove_write(wait_node) };
                }
                wait_node.state = PollState::Done;
                // Since the task was notified but did not lock the Mutex,
                // another task gets the chance to run.
                self.unlock_write();
            }
            PollState::Waiting => {
                // Remove the Entry from the linked list
                // Safety: Due to the state, we know that the node must be part
                // of the waiter list
                unsafe { self.force_remove_write(wait_node) };
                wait_node.state = PollState::Done;
            }
            PollState::New | PollState::Done => (),
        }
    }

    /// Removes the upgrade_read from the wait list.
    ///
    /// This function is only safe as long as the reference that is passed here
    /// equals the reference/address under which the waiter was added.
    /// The waiter must not have been moved in between.
    ///
    /// Returns the `Waker` of another task which might get ready to run due to
    /// this.
    fn remove_upgrade_read(&mut self, wait_node: &mut ListNode<Entry>) {
        // MutexLockFuture only needs to get removed if it had been added to
        // the wait queue of the Mutex. This has happened in the PollState::Waiting case.
        // If the current waiter was notified, another waiter must get notified now.
        match wait_node.state {
            PollState::Notified => {
                if self.is_fair {
                    // In a fair Mutex, the Entry is kept in the
                    // linked list and must be removed here
                    // Safety: Due to the state, we know that the node must be part
                    // of the waiter list
                    unsafe { self.force_remove_upgrade_read(wait_node) };
                }
                wait_node.state = PollState::Done;
                // Since the task was notified but did not lock the Mutex,
                // another task gets the chance to run.
                self.unlock_upgrade_read();
            }
            PollState::Waiting => {
                // Remove the Entry from the linked list
                // Safety: Due to the state, we know that the node must be part
                // of the waiter list
                unsafe { self.force_remove_upgrade_read(wait_node) };
                wait_node.state = PollState::Done;
            }
            PollState::New | PollState::Done => (),
        }
    }
}

/// An RAII guard returned by the `read` and `try_read` methods.
/// When this structure is dropped (falls out of scope), the shared
/// read access will be released.
pub struct GenericRwLockReadGuard<'a, MutexType: RawMutex, T: 'a> {
    /// The Mutex which is associated with this Guard
    mutex: &'a GenericRwLock<MutexType, T>,
}

impl<MutexType: RawMutex, T: core::fmt::Debug> core::fmt::Debug
    for GenericRwLockReadGuard<'_, MutexType, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("GenericRwLockReadGuard").finish()
    }
}

impl<MutexType: RawMutex, T> Drop for GenericRwLockReadGuard<'_, MutexType, T> {
    fn drop(&mut self) {
        // Release the guard.
        self.mutex.state.lock().unlock_read();
    }
}

impl<MutexType: RawMutex, T> Deref
    for GenericRwLockReadGuard<'_, MutexType, T>
{
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

// Safety: GenericRwLockReadGuard may only be used across threads if the underlying
// type is Sync.
unsafe impl<MutexType: RawMutex, T: Sync> Sync
    for GenericRwLockReadGuard<'_, MutexType, T>
{
}

/// A future which resolves when shared read access has been successfully acquired.
#[must_use = "futures do nothing unless polled"]
pub struct GenericRwLockReadFuture<'a, MutexType: RawMutex, T: 'a> {
    /// The Mutex which should get locked trough this Future
    mutex: Option<&'a GenericRwLock<MutexType, T>>,
    /// Node for waiting at the mutex
    wait_node: ListNode<Entry>,
}

// Safety: Futures can be sent between threads as long as the underlying
// mutex is thread-safe (Sync), which allows to poll/register/unregister from
// a different thread.
unsafe impl<'a, MutexType: RawMutex + Sync, T: 'a> Send
    for GenericRwLockReadFuture<'a, MutexType, T>
{
}

impl<'a, MutexType: RawMutex, T: core::fmt::Debug> core::fmt::Debug
    for GenericRwLockReadFuture<'a, MutexType, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("GenericRwLockReadFuture").finish()
    }
}

impl<'a, MutexType: RawMutex, T> Future
    for GenericRwLockReadFuture<'a, MutexType, T>
{
    type Output = GenericRwLockReadGuard<'a, MutexType, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: The next operations are safe, because Pin promises us that
        // the address of the wait queue entry inside this future is stable,
        // and we don't move any fields inside the future until it gets dropped.
        let mut_self: &mut GenericRwLockReadFuture<MutexType, T> =
            unsafe { Pin::get_unchecked_mut(self) };

        let mutex = mut_self
            .mutex
            .expect("polled GenericRwLockReadFuture after completion");
        let mut mutex_state = mutex.state.lock();

        let poll_res =
            unsafe { mutex_state.try_lock_read(&mut mut_self.wait_node, cx) };

        match poll_res {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => {
                // The mutex was acquired
                mut_self.mutex = None;
                Poll::Ready(GenericRwLockReadGuard::<'a, MutexType, T> {
                    mutex,
                })
            }
        }
    }
}

impl<'a, MutexType: RawMutex, T> FusedFuture
    for GenericRwLockReadFuture<'a, MutexType, T>
{
    fn is_terminated(&self) -> bool {
        self.mutex.is_none()
    }
}

impl<'a, MutexType: RawMutex, T> Drop
    for GenericRwLockReadFuture<'a, MutexType, T>
{
    fn drop(&mut self) {
        // If this GenericRwLockReadFuture has been polled and it was added to the
        // wait queue at the mutex, it must be removed before dropping.
        // Otherwise the mutex would access invalid memory.
        if let Some(mutex) = self.mutex {
            let mut mutex_state = mutex.state.lock();
            mutex_state.remove_read(&mut self.wait_node)
        }
    }
}

/// An RAII guard returned by the `write` and `try_write` methods.
/// When this structure is dropped (falls out of scope), the
/// exclusive write access will be released.
pub struct GenericRwLockWriteGuard<'a, MutexType: RawMutex, T: 'a> {
    /// The Mutex which is associated with this Guard
    mutex: &'a GenericRwLock<MutexType, T>,
}

impl<MutexType: RawMutex, T: core::fmt::Debug> core::fmt::Debug
    for GenericRwLockWriteGuard<'_, MutexType, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("GenericRwLockWriteGuard").finish()
    }
}

impl<MutexType: RawMutex, T> Drop
    for GenericRwLockWriteGuard<'_, MutexType, T>
{
    fn drop(&mut self) {
        // Release the guard.
        self.mutex.state.lock().unlock_write();
    }
}

impl<MutexType: RawMutex, T> Deref
    for GenericRwLockWriteGuard<'_, MutexType, T>
{
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<MutexType: RawMutex, T> DerefMut
    for GenericRwLockWriteGuard<'_, MutexType, T>
{
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

// Safety: GenericRwLockReadGuard may only be used across threads if the underlying
// type is Sync.
unsafe impl<MutexType: RawMutex, T: Sync> Sync
    for GenericRwLockWriteGuard<'_, MutexType, T>
{
}

/// A future which resolves when exclusive write access has been successfully acquired.
#[must_use = "futures do nothing unless polled"]
pub struct GenericRwLockWriteFuture<'a, MutexType: RawMutex, T: 'a> {
    /// The Mutex which should get locked trough this Future
    mutex: Option<&'a GenericRwLock<MutexType, T>>,
    /// Node for waiting at the mutex
    wait_node: ListNode<Entry>,
}

// Safety: Futures can be sent between threads as long as the underlying
// mutex is thread-safe (Sync), which allows to poll/register/unregister from
// a different thread.
unsafe impl<'a, MutexType: RawMutex + Sync, T: 'a> Send
    for GenericRwLockWriteFuture<'a, MutexType, T>
{
}

impl<'a, MutexType: RawMutex, T: core::fmt::Debug> core::fmt::Debug
    for GenericRwLockWriteFuture<'a, MutexType, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("GenericRwLockWriteFuture").finish()
    }
}

impl<'a, MutexType: RawMutex, T> Future
    for GenericRwLockWriteFuture<'a, MutexType, T>
{
    type Output = GenericRwLockWriteGuard<'a, MutexType, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: The next operations are safe, because Pin promises us that
        // the address of the wait queue entry inside this future is stable,
        // and we don't move any fields inside the future until it gets dropped.
        let mut_self: &mut GenericRwLockWriteFuture<MutexType, T> =
            unsafe { Pin::get_unchecked_mut(self) };

        let mutex = mut_self
            .mutex
            .expect("polled GenericRwLockWriteFuture after completion");
        let mut mutex_state = mutex.state.lock();

        let poll_res =
            unsafe { mutex_state.try_lock_write(&mut mut_self.wait_node, cx) };

        match poll_res {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => {
                // The mutex was acquired
                mut_self.mutex = None;
                Poll::Ready(GenericRwLockWriteGuard::<'a, MutexType, T> {
                    mutex,
                })
            }
        }
    }
}

impl<'a, MutexType: RawMutex, T> FusedFuture
    for GenericRwLockWriteFuture<'a, MutexType, T>
{
    fn is_terminated(&self) -> bool {
        self.mutex.is_none()
    }
}

impl<'a, MutexType: RawMutex, T> Drop
    for GenericRwLockWriteFuture<'a, MutexType, T>
{
    fn drop(&mut self) {
        // If this GenericRwLockWriteFuture has been polled and it was added to the
        // wait queue at the mutex, it must be removed before dropping.
        // Otherwise the mutex would access invalid memory.
        if let Some(mutex) = self.mutex {
            let mut mutex_state = mutex.state.lock();
            mutex_state.remove_write(&mut self.wait_node)
        }
    }
}

/// An RAII guard returned by the `write` and `try_write` methods.
/// When this structure is dropped (falls out of scope), the
/// exclusive write access will be released.
pub struct GenericRwLockUpgradableReadGuard<'a, MutexType: RawMutex, T: 'a> {
    /// The Mutex which is associated with this Guard
    mutex: Option<&'a GenericRwLock<MutexType, T>>,
}

impl<'a, MutexType: RawMutex, T>
    GenericRwLockUpgradableReadGuard<'a, MutexType, T>
{
    /// Asynchrousnly upgrade the shared read lock into an exclusive write lock.
    pub fn upgrade(mut self) -> GenericRwLockWriteFuture<'a, MutexType, T> {
        let mutex = self.mutex.take().unwrap();
        let mut state = mutex.state.lock();
        state.unlock_upgrade_read_for_upgrade();

        GenericRwLockWriteFuture::<MutexType, T> {
            mutex: Some(mutex),
            wait_node: ListNode::new(Entry::new(EntryKind::Write)),
        }
    }

    /// Atomically upgrade the shared read lock into an exclusive write lock,
    /// blocking the current thread.
    pub fn try_upgrade(
        mut self,
    ) -> Result<GenericRwLockWriteGuard<'a, MutexType, T>, Self> {
        let mutex = self.mutex.take().unwrap();
        let mut state = mutex.state.lock();
        if state.try_upgrade_read_sync() {
            Ok(GenericRwLockWriteGuard { mutex })
        } else {
            Err(Self { mutex: Some(mutex) })
        }
    }
}

impl<MutexType: RawMutex, T: core::fmt::Debug> core::fmt::Debug
    for GenericRwLockUpgradableReadGuard<'_, MutexType, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("GenericRwLockUpgradableReadGuard").finish()
    }
}

impl<MutexType: RawMutex, T> Drop
    for GenericRwLockUpgradableReadGuard<'_, MutexType, T>
{
    fn drop(&mut self) {
        if let Some(mutex) = self.mutex.take() {
            // Release the guard.
            mutex.state.lock().unlock_upgrade_read();
        }
    }
}

impl<MutexType: RawMutex, T> Deref
    for GenericRwLockUpgradableReadGuard<'_, MutexType, T>
{
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.as_ref().unwrap().value.get() }
    }
}

// Safety: GenericRwLockReadGuard may only be used across threads if the underlying
// type is Sync.
unsafe impl<MutexType: RawMutex, T: Sync> Sync
    for GenericRwLockUpgradableReadGuard<'_, MutexType, T>
{
}

/// A future which resolves when exclusive write access has been successfully acquired.
#[must_use = "futures do nothing unless polled"]
pub struct GenericRwLockUpgradableReadFuture<'a, MutexType: RawMutex, T: 'a> {
    /// The Mutex which should get locked trough this Future
    mutex: Option<&'a GenericRwLock<MutexType, T>>,
    /// Node for waiting at the mutex
    wait_node: ListNode<Entry>,
}

// Safety: Futures can be sent between threads as long as the underlying
// mutex is thread-safe (Sync), which allows to poll/register/unregister from
// a different thread.
unsafe impl<'a, MutexType: RawMutex + Sync, T: 'a> Send
    for GenericRwLockUpgradableReadFuture<'a, MutexType, T>
{
}

impl<'a, MutexType: RawMutex, T: core::fmt::Debug> core::fmt::Debug
    for GenericRwLockUpgradableReadFuture<'a, MutexType, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("GenericRwLockUpgradableReadFuture").finish()
    }
}

impl<'a, MutexType: RawMutex, T> Future
    for GenericRwLockUpgradableReadFuture<'a, MutexType, T>
{
    type Output = GenericRwLockUpgradableReadGuard<'a, MutexType, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: The next operations are safe, because Pin promises us that
        // the address of the wait queue entry inside the future is stable,
        // and we don't move any fields inside the future until it gets dropped.
        let mut_self: &mut GenericRwLockUpgradableReadFuture<MutexType, T> =
            unsafe { Pin::get_unchecked_mut(self) };

        let mutex = mut_self.mutex.expect(
            "polled GenericRwLockUpgradableReadFuture after completion",
        );
        let mut mutex_state = mutex.state.lock();

        let poll_res = unsafe {
            mutex_state.try_lock_upgrade_read(&mut mut_self.wait_node, cx)
        };

        match poll_res {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => {
                // The mutex was acquired
                mut_self.mutex = None;
                Poll::Ready(GenericRwLockUpgradableReadGuard::<
                    'a,
                    MutexType,
                    T,
                > {
                    mutex: Some(mutex),
                })
            }
        }
    }
}

impl<'a, MutexType: RawMutex, T> FusedFuture
    for GenericRwLockUpgradableReadFuture<'a, MutexType, T>
{
    fn is_terminated(&self) -> bool {
        self.mutex.is_none()
    }
}

impl<'a, MutexType: RawMutex, T> Drop
    for GenericRwLockUpgradableReadFuture<'a, MutexType, T>
{
    fn drop(&mut self) {
        // If this future has been polled and it was added to the
        // wait queue at the mutex, it must be removed before dropping.
        // Otherwise the mutex would access invalid memory.
        if let Some(mutex) = self.mutex {
            let mut mutex_state = mutex.state.lock();
            mutex_state.remove_upgrade_read(&mut self.wait_node)
        }
    }
}

/// A futures-aware mutex.
pub struct GenericRwLock<MutexType: RawMutex, T> {
    value: UnsafeCell<T>,
    state: LockApiMutex<MutexType, MutexState>,
}

// It is safe to send mutexes between threads, as long as they are not used and
// thereby borrowed
unsafe impl<T: Send, MutexType: RawMutex + Send> Send
    for GenericRwLock<MutexType, T>
{
}
// The mutex is thread-safe as long as the utilized mutex is thread-safe
unsafe impl<T: Send, MutexType: RawMutex + Sync> Sync
    for GenericRwLock<MutexType, T>
{
}

impl<MutexType: RawMutex, T: core::fmt::Debug> core::fmt::Debug
    for GenericRwLock<MutexType, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("Mutex")
            .field("is_exclusive", &self.is_exclusive())
            .finish()
    }
}

impl<MutexType: RawMutex, T> GenericRwLock<MutexType, T> {
    /// Creates a new futures-aware mutex.
    ///
    /// `is_fair` defines whether the `Mutex` should behave be fair regarding the
    /// order of waiters. A fair `Mutex` will only allow the first waiter which
    /// tried to lock but failed to lock the `Mutex` once it's available again.
    /// Other waiters must wait until either this locking attempt completes, and
    /// the `Mutex` gets unlocked again, or until the `MutexLockFuture` which
    /// tried to gain the lock is dropped.
    pub fn new(value: T, is_fair: bool) -> GenericRwLock<MutexType, T> {
        GenericRwLock::<MutexType, T> {
            value: UnsafeCell::new(value),
            state: LockApiMutex::new(MutexState::new(is_fair)),
        }
    }

    /// Acquire shared read access asynchronously.
    ///
    /// This method returns a future that will resolve once the shared
    /// read access has been successfully acquired.
    pub fn read(&self) -> GenericRwLockReadFuture<'_, MutexType, T> {
        GenericRwLockReadFuture::<MutexType, T> {
            mutex: Some(&self),
            wait_node: ListNode::new(Entry::new(EntryKind::Read)),
        }
    }

    /// Tries to acquire shared read access without waiting.
    ///
    /// If acquiring the mutex is successful, a [`GenericRwLockReadGuard`]
    /// will be returned, which allows to access the contained data.
    ///
    /// Otherwise `None` will be returned.
    pub fn try_read(&self) -> Option<GenericRwLockReadGuard<'_, MutexType, T>> {
        if self.state.lock().try_lock_read_sync() {
            Some(GenericRwLockReadGuard { mutex: self })
        } else {
            None
        }
    }

    /// Acquire exclusive write access asynchronously.
    ///
    /// This method returns a future that will resolve once the exclusive
    /// write access has been successfully acquired.
    pub fn write(&self) -> GenericRwLockWriteFuture<'_, MutexType, T> {
        GenericRwLockWriteFuture::<MutexType, T> {
            mutex: Some(&self),
            wait_node: ListNode::new(Entry::new(EntryKind::Write)),
        }
    }

    /// Tries to acquire exclusive write access without waiting.
    ///
    /// If acquiring the mutex is successful, a [`GenericRwLockReadGuard`]
    /// will be returned, which allows to access the contained data.
    ///
    /// Otherwise `None` will be returned.
    pub fn try_write(
        &self,
    ) -> Option<GenericRwLockWriteGuard<'_, MutexType, T>> {
        if self.state.lock().try_lock_write_sync() {
            Some(GenericRwLockWriteGuard { mutex: self })
        } else {
            None
        }
    }

    /// Acquire upgradable shared read access asynchronously.
    ///
    /// This method returns a future that will resolve once the upgradable
    /// shared read access has been successfully acquired.
    pub fn upgradable_read(
        &self,
    ) -> GenericRwLockUpgradableReadFuture<'_, MutexType, T> {
        GenericRwLockUpgradableReadFuture::<MutexType, T> {
            mutex: Some(&self),
            wait_node: ListNode::new(Entry::new(EntryKind::UpgradeRead)),
        }
    }

    /// Tries to acquire exclusive write access without waiting.
    ///
    /// If acquiring the mutex is successful, a [`GenericRwLockReadGuard`]
    /// will be returned, which allows to access the contained data.
    ///
    /// Otherwise `None` will be returned.
    pub fn try_upgradable_read(
        &self,
    ) -> Option<GenericRwLockUpgradableReadGuard<'_, MutexType, T>> {
        if self.state.lock().try_lock_upgrade_read_sync() {
            Some(GenericRwLockUpgradableReadGuard { mutex: Some(self) })
        } else {
            None
        }
    }

    /// Returns whether the rwlock is locked in exclusive access.
    pub fn is_exclusive(&self) -> bool {
        self.state.lock().has_write
    }

    /// Returns the number of shared read access guards currently held.
    pub fn nb_readers(&self) -> usize {
        self.state.lock().nb_reads
    }
}

// Export a non thread-safe version using NoopLock

/// A [`GenericRwLock`] which is not thread-safe.
pub type LocalRwLock<T> = GenericRwLock<NoopLock, T>;

/// A [`GenericRwLockReadGuard`] for [`LocalMutex`].
pub type LocalRwLockReadGuard<'a, T> = GenericRwLockReadGuard<'a, NoopLock, T>;

/// A [`GenericRwLockReadFuture`] for [`LocalMutex`].
pub type LocalRwLockReadFuture<'a, T> =
    GenericRwLockReadFuture<'a, NoopLock, T>;

/// A [`GenericRwLockWriteGuard`] for [`LocalMutex`].
pub type LocalRwLockWriteGuard<'a, T> =
    GenericRwLockWriteGuard<'a, NoopLock, T>;

/// A [`GenericRwLockWriteFuture`] for [`LocalMutex`].
pub type LocalRwLockWriteFuture<'a, T> =
    GenericRwLockWriteFuture<'a, NoopLock, T>;

/// A [`GenericRwLockUpgradableReadGuard`] for [`LocalMutex`].
pub type LocalRwLockUpgradableReadGuard<'a, T> =
    GenericRwLockUpgradableReadGuard<'a, NoopLock, T>;

/// A [`GenericRwLockUpgradableReadFuture`] for [`LocalMutex`].
pub type LocalRwLockUpgradableReadFuture<'a, T> =
    GenericRwLockUpgradableReadFuture<'a, NoopLock, T>;

#[cfg(feature = "std")]
mod if_std {
    use super::*;

    // Export a thread-safe version using parking_lot::RawMutex

    /// A [`GenericRwLock`] backed by [`parking_lot`].
    pub type RwLock<T> = GenericRwLock<parking_lot::RawMutex, T>;

    /// A [`GenericRwLockReadGuard`] for [`Mutex`].
    pub type RwLockReadGuard<'a, T> =
        GenericRwLockReadGuard<'a, parking_lot::RawMutex, T>;

    /// A [`GenericRwLockReadFuture`] for [`Mutex`].
    pub type RwLockReadFuture<'a, T> =
        GenericRwLockReadFuture<'a, parking_lot::RawMutex, T>;

    /// A [`GenericRwLockWriteGuard`] for [`Mutex`].
    pub type RwLockWriteGuard<'a, T> =
        GenericRwLockWriteGuard<'a, parking_lot::RawMutex, T>;

    /// A [`GenericRwLockWriteFuture`] for [`Mutex`].
    pub type RwLockWriteFuture<'a, T> =
        GenericRwLockWriteFuture<'a, parking_lot::RawMutex, T>;

    /// A [`GenericRwLockUpgradableReadGuard`] for [`Mutex`].
    pub type RwLockUpgradableReadGuard<'a, T> =
        GenericRwLockUpgradableReadGuard<'a, parking_lot::RawMutex, T>;

    /// A [`GenericRwLockUpgradableReadFuture`] for [`Mutex`].
    pub type RwLockUpgradableReadFuture<'a, T> =
        GenericRwLockUpgradableReadFuture<'a, parking_lot::RawMutex, T>;
}

#[cfg(feature = "std")]
pub use self::if_std::*;
