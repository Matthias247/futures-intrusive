//! An asynchronously awaitable multi producer multi consumer channel

use crate::intrusive_singly_linked_list::{LinkedList, ListNode};
use crate::{
    buffer::{ArrayRingBuf, RingBuf},
    NoopLock,
};
use core::marker::PhantomData;
use futures_core::task::{Context, Poll};
use lock_api::{Mutex, RawMutex};

use super::{
    ChannelReceiveAccess, ChannelReceiveFuture, ChannelSendAccess,
    ChannelSendFuture, RecvPollState, RecvWaitQueueEntry, SendPollState,
    SendWaitQueueEntry,
};

fn wake_recv_waiters(mut waiters: LinkedList<RecvWaitQueueEntry>) {
    unsafe {
        // Reverse the waiter list, so that the oldest waker (which is
        // at the end of the list), gets woken first and has the best
        // chance to grab the channel value.
        waiters.reverse();

        for waiter in waiters.into_iter() {
            if let Some(handle) = (*waiter).task.take() {
                handle.wake();
            }
            // The only kind of waiter that could have been stored here are
            // registered waiters (with a value), since others are removed
            // whenever their value had been copied into the channel.
            (*waiter).state = RecvPollState::Unregistered;
        }
    }
}

fn wake_send_waiters<T>(mut waiters: LinkedList<SendWaitQueueEntry<T>>) {
    unsafe {
        // Reverse the waiter list, so that the oldest waker (which is
        // at the end of the list), gets woken first and has the best
        // chance to grab the channel value.
        waiters.reverse();

        for waiter in waiters.into_iter() {
            if let Some(handle) = (*waiter).task.take() {
                handle.wake();
            }
            (*waiter).state = SendPollState::Unregistered;
        }
    }
}

/// Wakes up the last waiter and removes it from the wait queue
fn wakeup_last_receive_waiter(waiters: &mut LinkedList<RecvWaitQueueEntry>) {
    let last_waiter = waiters.remove_last();

    if !last_waiter.is_null() {
        unsafe {
            (*last_waiter).state = RecvPollState::Notified;

            if let Some(handle) = (*last_waiter).task.take() {
                handle.wake();
            }
        }
    }
}

/// Internal state of the channel
struct ChannelState<T, A>
where
    A: RingBuf<Item = T>,
{
    /// Whether the channel had been closed
    is_closed: bool,
    /// The value which is stored inside the channel
    buffer: A,
    /// Futures which are blocked on receive
    receive_waiters: LinkedList<RecvWaitQueueEntry>,
    /// Futures which are blocked on send
    send_waiters: LinkedList<SendWaitQueueEntry<T>>,
}

impl<T, A> ChannelState<T, A>
where
    A: RingBuf<Item = T>,
{
    fn new(buffer: A) -> ChannelState<T, A> {
        ChannelState::<T, A> {
            is_closed: false,
            buffer,
            receive_waiters: LinkedList::new(),
            send_waiters: LinkedList::new(),
        }
    }

    fn close(&mut self) {
        if self.is_closed {
            return;
        }
        self.is_closed = true;

        // Wakeup all send and receive waiters, since they are now guaranteed
        // to make progress.
        let recv_waiters = self.receive_waiters.take();
        wake_recv_waiters(recv_waiters);
        let send_waiters = self.send_waiters.take();
        wake_send_waiters(send_waiters);
    }

    /// Tries to send a value to the channel.
    /// If the value isn't available yet, the ChannelSendFuture gets added to the
    /// wait queue at the channel, and will be signalled once ready.
    /// If the channels is already closed, the value to send is returned.
    /// This function is only safe as long as the `wait_node`s address is guaranteed
    /// to be stable until it gets removed from the queue.
    unsafe fn try_send(
        &mut self,
        wait_node: &mut ListNode<SendWaitQueueEntry<T>>,
        cx: &mut Context<'_>,
    ) -> (Poll<()>, Option<T>) {
        match wait_node.state {
            SendPollState::Unregistered => {
                if self.is_closed {
                    let value = wait_node.value.take();
                    return (Poll::Ready(()), value);
                }

                if !self.buffer.can_push() {
                    // If the capacity is exhausted, register a waiter
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = SendPollState::Registered;
                    self.send_waiters.add_front(wait_node);

                    // Wakeup the oldest receive waiter
                    wakeup_last_receive_waiter(&mut self.receive_waiters);
                    return (Poll::Pending, None);
                } else {
                    // Otherwise copy the value directly into the channel
                    let value = wait_node
                        .value
                        .take()
                        .expect("wait_node must contain value");
                    self.buffer.push(value);

                    // Wakeup the oldest receive waiter
                    wakeup_last_receive_waiter(&mut self.receive_waiters);

                    (Poll::Ready(()), None)
                }
            }
            SendPollState::Registered => {
                // Since the channel wakes up all waiters and moves their states to unregistered
                // there can't be space available in the channel.
                (Poll::Pending, None)
            }
            SendPollState::SendComplete => {
                // The transfer is complete, and the sender has already been removed from the
                // list of pending senders
                (Poll::Ready(()), None)
            }
        }
    }

    /// If there is a send waiter, copy it's value into the channel buffer and complete it.
    /// The method may only be called if there is space in the receive buffer.
    unsafe fn try_copy_value_from_last_waiter(&mut self) {
        let last_waiter = self.send_waiters.remove_last();

        if !last_waiter.is_null() {
            let last_waiter = &mut (*last_waiter);
            let value = last_waiter
                .value
                .take()
                .expect("wait_node must contain value");
            self.buffer.push(value);

            last_waiter.state = SendPollState::SendComplete;

            if let Some(ref handle) = &last_waiter.task {
                handle.wake_by_ref();
            }
        }
    }

    /// Tries to read the value from the channel.
    /// If the value isn't available yet, the ChannelReceiveFuture gets added to the
    /// wait queue at the channel, and will be signalled once ready.
    /// This function is only safe as long as the `wait_node`s address is guaranteed
    /// to be stable until it gets removed from the queue.
    unsafe fn try_receive(
        &mut self,
        wait_node: &mut ListNode<RecvWaitQueueEntry>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<T>> {
        match wait_node.state {
            RecvPollState::Unregistered | RecvPollState::Notified => {
                wait_node.state = RecvPollState::Unregistered;

                if !self.buffer.is_empty() {
                    // A value is available - grab it.
                    let val = self.buffer.pop();

                    // Since this means a space in the buffer had been freed,
                    // try to copy a value from a potential waiter into the channel.
                    self.try_copy_value_from_last_waiter();
                    Poll::Ready(Some(val))
                } else if !self.send_waiters.is_empty() {
                    // This path should be only used for 0 capacity queues.
                    // Since the list is not empty, a value is available.
                    // Extract it from the sender in order to return it
                    assert_eq!(0, self.buffer.capacity());
                    let last_sender = self.send_waiters.remove_last();
                    assert!(!last_sender.is_null());

                    // Safety: The sender can't be null, since we only add valid
                    // senders to the queue
                    let last_sender = &mut (*last_sender);
                    let val = last_sender
                        .value
                        .take()
                        .expect("Value must be available");
                    last_sender.state = SendPollState::SendComplete;

                    // Wakeup the waiter
                    if let Some(ref task) = &last_sender.task {
                        task.wake_by_ref();
                    }

                    Poll::Ready(Some(val))
                } else if self.is_closed {
                    Poll::Ready(None)
                } else {
                    // Added the task to the wait queue
                    wait_node.task = Some(cx.waker().clone());
                    wait_node.state = RecvPollState::Registered;
                    self.receive_waiters.add_front(wait_node);
                    Poll::Pending
                }
            }
            RecvPollState::Registered => {
                // Since the channel wakes up all waiters and moves their states
                // to unregistered there can't be any value in the channel in
                // this state.
                Poll::Pending
            }
        }
    }

    fn remove_send_waiter(
        &mut self,
        wait_node: &mut ListNode<SendWaitQueueEntry<T>>,
    ) {
        // ChannelSendFuture only needs to get removed if it had been added to
        // the wait queue of the channel.
        // This has happened in the SendPollState::Registered case.
        match wait_node.state {
            SendPollState::Registered => {
                if !unsafe { self.send_waiters.remove(wait_node) } {
                    // Panic if the address isn't found. This can only happen if the contract was
                    // violated, e.g. the WaitQueueEntry got moved after the initial poll.
                    panic!("Future could not be removed from wait queue");
                }
                wait_node.state = SendPollState::Unregistered;
            }
            SendPollState::Unregistered => {}
            SendPollState::SendComplete => {
                // Send was complete. In that case the queue item is not in the list
            }
        }
    }

    fn remove_receive_waiter(
        &mut self,
        wait_node: &mut ListNode<RecvWaitQueueEntry>,
    ) {
        // ChannelReceiveFuture only needs to get removed if it had been added to
        // the wait queue of the channel. This has happened in the RecvPollState::Registered case.
        match wait_node.state {
            RecvPollState::Registered => {
                if !unsafe { self.receive_waiters.remove(wait_node) } {
                    // Panic if the address isn't found. This can only happen if the contract was
                    // violated, e.g. the WaitQueueEntry got moved after the initial poll.
                    panic!("Future could not be removed from wait queue");
                }
                wait_node.state = RecvPollState::Unregistered;
            }
            RecvPollState::Notified => {
                // wakeup another receive waiter instead
                wakeup_last_receive_waiter(&mut self.receive_waiters);
                wait_node.state = RecvPollState::Unregistered;
            }
            RecvPollState::Unregistered => {}
        }
    }
}

/// A channel which can be used to exchange values of type `T` between
/// concurrent tasks.
///
/// `A` represents the backing buffer for a Channel. E.g. a channel which
/// can buffer up to 4 u32 values can be created via:
///
/// ```
/// # use futures_intrusive::channel::LocalChannel;
/// let channel: LocalChannel<i32, [i32; 4]> = LocalChannel::new();
/// ```
///
/// Tasks can receive values from the channel through the `receive` method.
/// The returned Future will get resolved when a value is sent into the channel.
/// Values can be sent into the channel through `send`.
/// The returned Future will get resolved when the value has been stored
/// inside the channel.
pub struct GenericChannel<MutexType: RawMutex, T, A>
where
    A: RingBuf<Item = T>,
{
    inner: Mutex<MutexType, ChannelState<T, A>>,
}

// The channel can be sent to other threads as long as it's not borrowed and the
// value in it can be sent to other threads.
unsafe impl<MutexType: RawMutex + Send, T: Send, A> Send
    for GenericChannel<MutexType, T, A>
where
    A: RingBuf<Item = T> + Send,
{
}
// The channel is thread-safe as long as a thread-safe mutex is used
unsafe impl<MutexType: RawMutex + Sync, T: Send, A> Sync
    for GenericChannel<MutexType, T, A>
where
    A: RingBuf<Item = T>,
{
}

impl<MutexType: RawMutex, T, A> core::fmt::Debug
    for GenericChannel<MutexType, T, A>
where
    A: RingBuf<Item = T>,
{
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("Channel").finish()
    }
}

impl<MutexType: RawMutex, T, A> GenericChannel<MutexType, T, A>
where
    A: RingBuf<Item = T>,
{
    /// Creates a new Channel, utilizing the default capacity that
    /// the RingBuffer in `A` provides.
    pub fn new() -> Self {
        GenericChannel {
            inner: Mutex::new(ChannelState::new(A::new())),
        }
    }

    /// Creates a new Channel, which has storage for a `capacity` items.
    /// Depending on the utilized `RingBuf` type, the capacity argument might
    /// be ignored and the default capacity might be utilized.
    pub fn with_capacity(capacity: usize) -> Self {
        GenericChannel {
            inner: Mutex::new(ChannelState::new(A::with_capacity(capacity))),
        }
    }

    /// Returns a future that gets fulfilled when the value has been written to
    /// the channel.
    /// If the channel gets closed while the send is in progress, sending the
    /// value will fail, and the future will deliver the value back.
    pub fn send(&self, value: T) -> ChannelSendFuture<MutexType, T> {
        ChannelSendFuture {
            channel: Some(self),
            wait_node: ListNode::new(SendWaitQueueEntry::new(value)),
            _phantom: PhantomData,
        }
    }

    /// Returns a future that gets fulfilled when a value is written to the channel.
    /// If the channels gets closed, the future will resolve to `None`.
    pub fn receive(&self) -> ChannelReceiveFuture<MutexType, T> {
        ChannelReceiveFuture {
            channel: Some(self),
            wait_node: ListNode::new(RecvWaitQueueEntry::new()),
            _phantom: PhantomData,
        }
    }

    /// Closes the channel.
    /// All pending and future send attempts will fail.
    /// Receive attempts will continue to succeed as long as there are items
    /// stored inside the channel. Further attempts will fail.
    pub fn close(&self) {
        self.inner.lock().close()
    }
}

impl<MutexType: RawMutex, T, A> ChannelSendAccess<T>
    for GenericChannel<MutexType, T, A>
where
    A: RingBuf<Item = T>,
{
    unsafe fn try_send(
        &self,
        wait_node: &mut ListNode<SendWaitQueueEntry<T>>,
        cx: &mut Context<'_>,
    ) -> (Poll<()>, Option<T>) {
        self.inner.lock().try_send(wait_node, cx)
    }

    fn remove_send_waiter(
        &self,
        wait_node: &mut ListNode<SendWaitQueueEntry<T>>,
    ) {
        self.inner.lock().remove_send_waiter(wait_node)
    }
}

impl<MutexType: RawMutex, T, A> ChannelReceiveAccess<T>
    for GenericChannel<MutexType, T, A>
where
    A: RingBuf<Item = T>,
{
    unsafe fn try_receive(
        &self,
        wait_node: &mut ListNode<RecvWaitQueueEntry>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<T>> {
        self.inner.lock().try_receive(wait_node, cx)
    }

    fn remove_receive_waiter(
        &self,
        wait_node: &mut ListNode<RecvWaitQueueEntry>,
    ) {
        self.inner.lock().remove_receive_waiter(wait_node)
    }
}

// Export a non thread-safe version using NoopLock

/// A [`GenericChannel`] implementation which is not thread-safe.
pub type LocalChannel<T, A> = GenericChannel<NoopLock, T, ArrayRingBuf<T, A>>;

/// An unbuffered [`GenericChannel`] implementation which is not thread-safe.
pub type LocalUnbufferedChannel<T> = LocalChannel<T, [T; 0]>;

#[cfg(feature = "std")]
mod if_std {
    use super::*;
    // Export a thread-safe version using parking_lot::RawMutex

    // TODO: We might also want to bind Channel to GenericChannel<..., HeapRingBuf>,
    // which performs less type-churn.
    // However since we can't bind LocalChannel to that too due to no-std compatibility,
    // this would to introduce some inconsistency between those types.
    // It's also bit unfortunate that there are now `new()` and `with_capacity`
    // methods on both types, but for the array backed implementation only
    // `new()` is meaningful, while for the heap backed implementation only
    // `with_capacity()` is meaningful.

    /// A [`GenericChannel`] implementation backed by [`parking_lot`].
    pub type Channel<T, A> =
        GenericChannel<parking_lot::RawMutex, T, ArrayRingBuf<T, A>>;

    /// An unbuffered [`GenericChannel`] implementation backed by [`parking_lot`].
    pub type UnbufferedChannel<T> = Channel<T, [T; 0]>;
}

#[cfg(feature = "std")]
pub use self::if_std::*;

// The next section should really integrated if the alloc feature is active,
// since it mainly requires `Arc` to be available. However for simplicity reasons
// it is currently only activated in std environments.
#[cfg(feature = "std")]
mod if_alloc {
    use super::*;

    /// Channel implementations where Sender and Receiver sides are cloneable
    /// and owned.
    /// The Futures produced by channels in this module don't require a lifetime
    /// parameter.
    pub mod shared {
        use super::*;
        use crate::buffer::HeapRingBuf;
        use crate::channel::shared::{ChannelReceiveFuture, ChannelSendFuture};
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Shared Channel State, which is referenced by Senders and Receivers
        struct GenericChannelSharedState<MutexType, T, A>
        where
            MutexType: RawMutex,
            T: 'static,
            A: RingBuf<Item = T>,
        {
            /// The amount of [`GenericSender`] instances which reference this state.
            senders: AtomicUsize,
            /// The amount of [`GenericReceiver`] instances which reference this state.
            receivers: AtomicUsize,
            /// The channel on which is acted.
            channel: GenericChannel<MutexType, T, A>,
        }

        // Implement ChannelAccess trait for SharedChannelState, so that it can
        // be used for dynamic dispatch in futures.
        impl<MutexType, T, A> ChannelReceiveAccess<T>
            for GenericChannelSharedState<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
        {
            unsafe fn try_receive(
                &self,
                wait_node: &mut ListNode<RecvWaitQueueEntry>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<T>> {
                self.channel.try_receive(wait_node, cx)
            }

            fn remove_receive_waiter(
                &self,
                wait_node: &mut ListNode<RecvWaitQueueEntry>,
            ) {
                self.channel.remove_receive_waiter(wait_node)
            }
        }

        // Implement ChannelAccess trait for SharedChannelState, so that it can
        // be used for dynamic dispatch in futures.
        impl<MutexType, T, A> ChannelSendAccess<T>
            for GenericChannelSharedState<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
        {
            unsafe fn try_send(
                &self,
                wait_node: &mut ListNode<SendWaitQueueEntry<T>>,
                cx: &mut Context<'_>,
            ) -> (Poll<()>, Option<T>) {
                self.channel.try_send(wait_node, cx)
            }

            fn remove_send_waiter(
                &self,
                wait_node: &mut ListNode<SendWaitQueueEntry<T>>,
            ) {
                self.channel.remove_send_waiter(wait_node)
            }
        }

        /// The sending side of a channel which can be used to exchange values
        /// between concurrent tasks.
        ///
        /// Values can be sent into the channel through `send`.
        /// The returned Future will get resolved when the value has been stored inside the channel.
        pub struct GenericSender<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
            T: 'static,
        {
            inner: std::sync::Arc<GenericChannelSharedState<MutexType, T, A>>,
        }

        /// The receiving side of a channel which can be used to exchange values
        /// between concurrent tasks.
        ///
        /// Tasks can receive values from the channel through the `receive` method.
        /// The returned Future will get resolved when a value is sent into the channel.
        pub struct GenericReceiver<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
            T: 'static,
        {
            inner: std::sync::Arc<GenericChannelSharedState<MutexType, T, A>>,
        }

        impl<MutexType, T, A> core::fmt::Debug for GenericSender<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
        {
            fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.debug_struct("Sender").finish()
            }
        }

        impl<MutexType, T, A> core::fmt::Debug for GenericReceiver<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
        {
            fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.debug_struct("Receiver").finish()
            }
        }

        impl<MutexType, T, A> Clone for GenericSender<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
        {
            fn clone(&self) -> Self {
                let old_size =
                    self.inner.senders.fetch_add(1, Ordering::Relaxed);
                if old_size > (core::isize::MAX) as usize {
                    panic!("Reached maximum refcount");
                }
                GenericSender {
                    inner: self.inner.clone(),
                }
            }
        }

        impl<MutexType, T, A> Drop for GenericSender<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
        {
            fn drop(&mut self) {
                if self.inner.senders.fetch_sub(1, Ordering::Release) != 1 {
                    return;
                }
                std::sync::atomic::fence(Ordering::Acquire);
                // Close the channel, before last sender gets destroyed
                // TODO: We could potentially avoid this, if no receiver is left
                self.inner.channel.close();
            }
        }

        impl<MutexType, T, A> Clone for GenericReceiver<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
        {
            fn clone(&self) -> Self {
                let old_size =
                    self.inner.receivers.fetch_add(1, Ordering::Relaxed);
                if old_size > (core::isize::MAX) as usize {
                    panic!("Reached maximum refcount");
                }
                GenericReceiver {
                    inner: self.inner.clone(),
                }
            }
        }

        impl<MutexType, T, A> Drop for GenericReceiver<MutexType, T, A>
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
        {
            fn drop(&mut self) {
                if self.inner.receivers.fetch_sub(1, Ordering::Release) != 1 {
                    return;
                }
                std::sync::atomic::fence(Ordering::Acquire);
                // Close the channel, before last receiver gets destroyed
                // TODO: We could potentially avoid this, if no sender is left
                self.inner.channel.close();
            }
        }

        /// Creates a new Channel which can be used to exchange values of type `T` between
        /// concurrent tasks. The ends of the Channel are represented through
        /// the returned Sender and Receiver.
        /// Both the Sender and Receiver can be cloned in order to let more tasks
        /// interact with the Channel.
        ///
        /// As soon es either all Senders or all Receivers are closed, the Channel
        /// itself will be closed.
        ///
        /// The channel can buffer up to `capacity` items internally.
        ///
        /// ```
        /// # use futures_intrusive::channel::shared::channel;
        /// let (sender, receiver) = channel::<i32>(4);
        /// ```
        pub fn generic_channel<MutexType, T, A>(
            capacity: usize,
        ) -> (
            GenericSender<MutexType, T, A>,
            GenericReceiver<MutexType, T, A>,
        )
        where
            MutexType: RawMutex,
            A: RingBuf<Item = T>,
            T: Send,
        {
            let inner = std::sync::Arc::new(GenericChannelSharedState {
                channel: GenericChannel::with_capacity(capacity),
                senders: AtomicUsize::new(1),
                receivers: AtomicUsize::new(1),
            });

            let sender = GenericSender {
                inner: inner.clone(),
            };
            let receiver = GenericReceiver { inner };

            (sender, receiver)
        }

        impl<MutexType, T, A> GenericSender<MutexType, T, A>
        where
            MutexType: 'static + RawMutex,
            A: 'static + RingBuf<Item = T>,
        {
            /// Returns a future that gets fulfilled when the value has been written to
            /// the channel.
            /// If the channel gets closed while the send is in progress, sending the
            /// value will fail, and the future will deliver the value back.
            pub fn send(&self, value: T) -> ChannelSendFuture<MutexType, T> {
                ChannelSendFuture {
                    channel: Some(self.inner.clone()),
                    wait_node: ListNode::new(SendWaitQueueEntry::new(value)),
                    _phantom: PhantomData,
                }
            }

            /// Closes the channel.
            /// All pending future send attempts will fail.
            /// Receive attempts will continue to succeed as long as there are items
            /// stored inside the channel. Further attempts will return `None`.
            pub fn close(&self) {
                self.inner.channel.close()
            }
        }

        impl<MutexType, T, A> GenericReceiver<MutexType, T, A>
        where
            MutexType: 'static + RawMutex,
            A: 'static + RingBuf<Item = T>,
        {
            /// Returns a future that gets fulfilled when a value is written to the channel.
            /// If the channels gets closed, the future will resolve to `None`.
            pub fn receive(&self) -> ChannelReceiveFuture<MutexType, T> {
                ChannelReceiveFuture {
                    channel: Some(self.inner.clone()),
                    wait_node: ListNode::new(RecvWaitQueueEntry::new()),
                    _phantom: PhantomData,
                }
            }

            /// Closes the channel.
            /// All pending future send attempts will fail.
            /// Receive attempts will continue to succeed as long as there are items
            /// stored inside the channel. Further attempts will return `None`.
            pub fn close(&self) {
                self.inner.channel.close()
            }
        }

        // Export parking_lot based shared channels in std mode
        #[cfg(feature = "std")]
        mod if_std {
            use super::*;

            use crate::buffer::GrowingRingBuf;

            /// A [`GenericSender`] implementation backed by [`parking_lot`].
            ///
            /// Uses a `HeapRingBuf` which allocates the capacity ahead of time.
            /// Refer to [`HeapRingBuf`] for more information.
            ///
            /// [`HeapRingBuf`]: ../../buffer/struct.HeapRingBuf.html
            pub type Sender<T> =
                GenericSender<parking_lot::RawMutex, T, HeapRingBuf<T>>;
            /// A [`GenericReceiver`] implementation backed by [`parking_lot`].
            ///
            /// Uses a `HeapRingBuf` which allocates the capacity ahead of time.
            /// Refer to [`HeapRingBuf`] for more information.
            ///
            /// [`HeapRingBuf`]: ../../buffer/struct.HeapRingBuf.html
            pub type Receiver<T> =
                GenericReceiver<parking_lot::RawMutex, T, HeapRingBuf<T>>;

            /// Creates a new channel.
            ///
            /// Refer to [`generic_channel`] for details.
            pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>)
            where
                T: Send,
            {
                generic_channel::<parking_lot::RawMutex, T, HeapRingBuf<T>>(
                    capacity,
                )
            }

            /// A [`GenericSender`] implementation backed by [`parking_lot`].
            pub type UnbufferedSender<T> =
                GenericSender<parking_lot::RawMutex, T, HeapRingBuf<T>>;
            /// A [`GenericReceiver`] implementation backed by [`parking_lot`].
            pub type UnbufferedReceiver<T> =
                GenericReceiver<parking_lot::RawMutex, T, HeapRingBuf<T>>;

            /// Creates a new unbuffered channel.
            ///
            /// Refer to [`generic_channel`] for details.
            pub fn unbuffered_channel<T>() -> (Sender<T>, Receiver<T>)
            where
                T: Send,
            {
                generic_channel::<parking_lot::RawMutex, T, HeapRingBuf<T>>(0)
            }

            /// A [`GenericSender`] implementation backed by [`parking_lot`].
            ///
            /// Uses a `GrowingRingBuf` whose capacity grows dynamically up to
            /// the given limit. Refer to [`GrowingRingBuf`] for more information.
            ///
            /// [`GrowingRingBuf`]: ../../buffer/struct.GrowingRingBuf.html
            pub type GrowingSender<T> =
                GenericSender<parking_lot::RawMutex, T, GrowingRingBuf<T>>;
            /// A [`GenericReceiver`] implementation backed by [`parking_lot`].
            ///
            /// Uses a `GrowingRingBuf` whose capacity grows dynamically up to
            /// the given limit. Refer to [`GrowingRingBuf`] for more information.
            ///
            /// [`GrowingRingBuf`]: ../../buffer/struct.GrowingRingBuf.html
            pub type GrowingReceiver<T> =
                GenericReceiver<parking_lot::RawMutex, T, GrowingRingBuf<T>>;

            /// Creates a new growing channel.
            ///
            /// Refer to [`generic_channel`] for details.
            pub fn growing_channel<T>(
                capacity: usize,
            ) -> (GrowingSender<T>, GrowingReceiver<T>)
            where
                T: Send,
            {
                generic_channel::<parking_lot::RawMutex, T, GrowingRingBuf<T>>(
                    capacity,
                )
            }
        }

        #[cfg(feature = "std")]
        pub use self::if_std::*;
    }
}

#[cfg(feature = "std")]
pub use self::if_alloc::*;
