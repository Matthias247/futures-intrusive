#![feature(collapse_debuginfo)]

use futures::future::{FusedFuture, Future};
use futures::task::{Context, Poll};
use futures_intrusive::sync::LocalRwLock;
use futures_test::task::{new_count_waker, panic_waker};
use pin_utils::pin_mut;

// Allows backtrace to work properly inside macro expension.
#[collapse_debuginfo]
macro_rules! assert_eq_ {
    ($left:expr, $right:expr $(,)?) => {
        assert_eq!($left, $right);
    };
}

#[collapse_debuginfo]
macro_rules! assert_ {
    ($val:expr) => {
        assert!($val)
    };
}

#[collapse_debuginfo]
macro_rules! panic_ {
    ($val:expr) => {
        panic!($val)
    };
}

#[collapse_debuginfo]
macro_rules! gen_rwlock_tests {
    ($mod_name:ident, $rwlock_type:ident) => {
        mod $mod_name {
            use super::*;

            #[test]
            fn uncontended_read() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    {
                        let mut guards = Vec::with_capacity(3);
                        for _ in 0..3 {
                            let fut = lock.read();
                            pin_mut!(fut);
                            match fut.as_mut().poll(cx) {
                                Poll::Pending => {
                                    panic_!("Expect lock to get locked")
                                }
                                Poll::Ready(mut guard) => {
                                    assert_eq_!(false, lock.is_exclusive());
                                    assert_eq_!(5, *guard);
                                    guards.push(guard);
                                    assert_eq_!(
                                        guards.len(),
                                        lock.nb_readers()
                                    );
                                }
                            };
                            assert_!(fut.as_mut().is_terminated());
                        }

                        assert_eq_!(3, lock.nb_readers());

                        drop(guards.pop().unwrap());
                        assert_eq_!(2, lock.nb_readers());

                        drop(guards.pop().unwrap());
                        assert_eq_!(1, lock.nb_readers());

                        drop(guards.pop().unwrap());
                        assert_eq_!(0, lock.nb_readers());
                    }

                    {
                        let fut = lock.read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic_!("Expect lock to get locked")
                            }
                            Poll::Ready(guard) => {
                                assert_eq_!(false, lock.is_exclusive());
                                assert_eq_!(1, lock.nb_readers());
                                assert_eq_!(5, *guard);
                            }
                        };
                    }

                    assert_eq_!(0, lock.nb_readers());
                }
            }

            #[test]
            fn uncontended_write() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    {
                        let fut = lock.write();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic_!("Expect lock to get locked")
                            }
                            Poll::Ready(mut guard) => {
                                assert_eq_!(true, lock.is_exclusive());
                                assert_eq_!(5, *guard);
                                *guard = 12;
                                assert_eq_!(12, *guard);
                            }
                        };
                        assert_!(fut.as_mut().is_terminated());
                    }
                    assert_eq_!(false, lock.is_exclusive());

                    {
                        let fut = lock.write();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic_!("Expect lock to get locked")
                            }
                            Poll::Ready(guard) => {
                                assert_eq_!(true, lock.is_exclusive());
                                assert_eq_!(12, *guard);
                            }
                        };
                    }

                    assert_eq_!(false, lock.is_exclusive());
                }
            }

            #[test]
            fn uncontended_upgradable_read() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    {
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        let guard = match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic_!("Expect lock to get locked")
                            }
                            Poll::Ready(guard) => {
                                assert_eq_!(false, lock.is_exclusive());
                                assert_eq_!(5, *guard);
                                guard
                            }
                        };
                        assert_!(fut.as_mut().is_terminated());

                        let fut = guard.upgrade();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic_!("Expect lock to get locked")
                            }
                            Poll::Ready(mut guard) => {
                                assert_eq_!(true, lock.is_exclusive());
                                assert_eq_!(5, *guard);
                                *guard = 12;
                                assert_eq_!(12, *guard);
                            }
                        };
                    }

                    assert_eq_!(false, lock.is_exclusive());

                    {
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic_!("Expect lock to get locked")
                            }
                            Poll::Ready(guard) => {
                                assert_eq_!(false, lock.is_exclusive());
                                assert_eq_!(12, *guard);
                            }
                        };
                    }

                    assert_eq_!(false, lock.is_exclusive());
                }
            }

            #[test]
            fn contended_read() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    let guard = lock.try_write().unwrap();

                    {
                        assert_!(lock.try_read().is_none());
                        let fut = lock.read();
                        pin_mut!(fut);
                        assert_!(fut.as_mut().poll(cx).is_pending());
                        assert_!(!fut.as_mut().is_terminated());
                    }

                    {
                        assert_!(lock.try_read().is_none());
                        let fut = lock.read();
                        pin_mut!(fut);
                        assert_!(fut.as_mut().poll(cx).is_pending());
                        assert_!(!fut.as_mut().is_terminated());
                    }
                }
            }

            #[test]
            fn contended_write() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    let guard = lock.try_read().unwrap();

                    {
                        assert_!(lock.try_write().is_none());
                        let fut = lock.write();
                        pin_mut!(fut);
                        assert_!(fut.as_mut().poll(cx).is_pending());
                        assert_!(!fut.as_mut().is_terminated());
                    }
                }
            }

            #[test]
            fn contended_upgrade_read() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    let guard = lock.try_upgradable_read().unwrap();
                    {
                        assert_!(lock.try_upgradable_read().is_none());
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        assert_!(fut.as_mut().poll(cx).is_pending());
                        assert_!(!fut.as_mut().is_terminated());
                    }
                    drop(guard);

                    let guard = lock.try_write().unwrap();
                    {
                        assert_!(lock.try_upgradable_read().is_none());
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        assert_!(fut.as_mut().poll(cx).is_pending());
                        assert_!(!fut.as_mut().is_terminated());
                    }
                }
            }

            #[test]
            fn dropping_reader_wakes_up_writer() {
                for is_fair in &[true, false] {
                    let (waker, count) = new_count_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    let guard = lock.try_read().unwrap();

                    let mut fut = lock.write();
                    pin_mut!(fut);
                    assert_!(fut.as_mut().poll(cx).is_pending());
                    assert_!(!fut.as_mut().is_terminated());

                    assert_eq_!(count, 0);

                    drop(guard);
                    assert_eq_!(count, 1);

                    assert_!(fut.as_mut().poll(cx).is_ready());
                    assert_!(fut.as_mut().is_terminated());
                }
            }

            #[test]
            fn dropping_writer_wakes_up_readers() {
                for is_fair in &[true, false] {
                    let (waker, count) = new_count_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    let guard = lock.try_write().unwrap();

                    let fut1 = lock.read();
                    pin_mut!(fut1);
                    assert_!(fut1.as_mut().poll(cx).is_pending());
                    assert_!(!fut1.as_mut().is_terminated());

                    let fut2 = lock.upgradable_read();
                    pin_mut!(fut2);
                    assert_!(fut2.as_mut().poll(cx).is_pending());
                    assert_!(!fut2.as_mut().is_terminated());

                    assert_eq_!(count, 0);

                    drop(guard);
                    assert_eq_!(count, 2);

                    assert_!(fut1.as_mut().poll(cx).is_ready());
                    assert_!(fut1.as_mut().is_terminated());

                    assert_!(fut2.as_mut().poll(cx).is_ready());
                    assert_!(fut2.as_mut().is_terminated());
                }
            }

            #[test]
            #[should_panic]
            fn poll_read_after_completion_should_panic() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    {
                        let fut = lock.read();
                        pin_mut!(fut);
                        assert_!(fut.as_mut().poll(cx).is_ready());
                        assert_!(fut.as_mut().is_terminated());

                        let _ = fut.as_mut().poll(cx);
                    }
                }
            }

            #[test]
            #[should_panic]
            fn poll_write_after_completion_should_panic() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    {
                        let fut = lock.write();
                        pin_mut!(fut);
                        assert_!(fut.as_mut().poll(cx).is_ready());
                        assert_!(fut.as_mut().is_terminated());

                        let _ = fut.as_mut().poll(cx);
                    }
                }
            }

            #[test]
            #[should_panic]
            fn poll_upgradable_read_after_completion_should_panic() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    {
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        assert_!(fut.as_mut().poll(cx).is_ready());
                        assert_!(fut.as_mut().is_terminated());

                        let _ = fut.as_mut().poll(cx);
                    }
                }
            }

            #[test]
            fn dropping_guard_follows_queue_order_for_wakeup() {
                for is_fair in &[true, false] {
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq_!(false, lock.is_exclusive());

                    let (waker, count) = new_count_waker();
                    let cx = &mut Context::from_waker(&waker);

                    let writer = lock.try_write().unwrap();

                    let read_fut1 = lock.read();
                    pin_mut!(read_fut1);
                    assert_!(read_fut1.as_mut().poll(cx).is_pending());

                    let read_fut2 = lock.read();
                    pin_mut!(read_fut2);
                    assert_!(read_fut2.as_mut().poll(cx).is_pending());

                    let write_fut1 = lock.write();
                    pin_mut!(write_fut1);
                    assert_!(write_fut1.as_mut().poll(cx).is_pending());

                    let upgrade_read_fut1 = lock.upgradable_read();
                    pin_mut!(upgrade_read_fut1);
                    assert_!(upgrade_read_fut1.as_mut().poll(cx).is_pending());

                    let write_fut2 = lock.write();
                    pin_mut!(write_fut2);
                    assert_!(write_fut2.as_mut().poll(cx).is_pending());

                    let read_fut3 = lock.read();
                    pin_mut!(read_fut3);
                    assert_!(read_fut3.as_mut().poll(cx).is_pending());

                    let upgrade_read_fut2 = lock.upgradable_read();
                    pin_mut!(upgrade_read_fut2);
                    assert_!(upgrade_read_fut2.as_mut().poll(cx).is_pending());

                    assert_eq_!(count, 0);

                    drop(writer);

                    // Wakeup the three readers and 1 upgradable_read.
                    assert_eq_!(count, 4);

                    let guard1 = match read_fut1.as_mut().poll(cx) {
                        Poll::Pending => panic_!("busy rwlock"),
                        Poll::Ready(guard) => guard,
                    };
                    let guard2 = match read_fut2.as_mut().poll(cx) {
                        Poll::Pending => panic_!("busy rwlock"),
                        Poll::Ready(guard) => guard,
                    };
                    let guard3 = match upgrade_read_fut1.as_mut().poll(cx) {
                        Poll::Pending => panic_!("busy rwlock"),
                        Poll::Ready(guard) => guard,
                    };
                    let guard4 = match read_fut3.as_mut().poll(cx) {
                        Poll::Pending => panic_!("busy rwlock"),
                        Poll::Ready(guard) => guard,
                    };
                    assert_!(upgrade_read_fut2.as_mut().poll(cx).is_pending());
                    assert_!(write_fut1.as_mut().poll(cx).is_pending());
                    assert_!(write_fut2.as_mut().poll(cx).is_pending());

                    assert_eq_!(count, 4);

                    drop(guard1);
                    assert_eq_!(count, 4);

                    // Wakeup the other upgradable_read
                    drop(guard3);
                    assert_eq_!(count, 5);

                    let guard5 = match upgrade_read_fut2.as_mut().poll(cx) {
                        Poll::Pending => panic_!("busy rwlock"),
                        Poll::Ready(guard) => guard,
                    };
                    assert_!(write_fut1.as_mut().poll(cx).is_pending());
                    assert_!(write_fut2.as_mut().poll(cx).is_pending());

                    drop(guard2);
                    assert_eq_!(count, 5);
                    drop(guard5);
                    assert_eq_!(count, 5);
                    // Now that we dropped all readers, wakeup one writer.
                    drop(guard4);
                    assert_eq_!(count, 6);

                    let guard6 = match write_fut1.as_mut().poll(cx) {
                        Poll::Pending => panic_!("busy rwlock"),
                        Poll::Ready(guard) => guard,
                    };
                    assert_!(write_fut2.as_mut().poll(cx).is_pending());

                    // Wakeup the other writer.
                    drop(guard6);
                    assert_eq_!(count, 7);

                    assert_!(write_fut2.as_mut().poll(cx).is_ready());
                    assert_eq_!(count, 7);
                }
            }
        }

        #[test]
        fn cancel_wait_for_lock() {
            for is_fair in &[true, false] {
                let (waker, count) = new_count_waker();
                let cx = &mut Context::from_waker(&waker);
                let lock = $rwlock_type::new(5, *is_fair);

                let mut guard1 = lock.try_write().unwrap();

                // The second and third lock attempt must fail
                let mut fut1 = Box::pin(lock.write());
                let mut fut2 = Box::pin(lock.read());
                let mut fut3 = Box::pin(lock.upgradable_read());
                let mut fut4 = Box::pin(lock.write());

                assert!(fut1.as_mut().poll(cx).is_pending());
                assert!(fut2.as_mut().poll(cx).is_pending());
                assert!(fut3.as_mut().poll(cx).is_pending());
                assert!(fut4.as_mut().poll(cx).is_pending());

                // Before the lock gets available, cancel a bunch of futures.
                drop(fut1);
                drop(fut2);
                drop(fut3);

                assert_eq!(count, 0);

                // The reader should have been notified.
                drop(guard1);
                assert_eq!(count, 1);

                // Unlock - mutex should be available again
                assert_!(fut4.as_mut().poll(cx).is_ready());
            }
        }

        #[test]
        fn unlock_next_when_notification_is_not_used() {
            for is_fair in &[true, false] {
                let (waker, count) = new_count_waker();
                let cx = &mut Context::from_waker(&waker);
                let lock = $rwlock_type::new(5, *is_fair);

                // Lock the mutex
                let mut guard1 = lock.try_write().unwrap();

                // The second and third lock attempt must fail
                let mut fut2 = Box::pin(lock.write());
                let mut fut3 = Box::pin(lock.write());
                let mut fut4 = Box::pin(lock.upgradable_read());
                let mut fut5 = Box::pin(lock.upgradable_read());

                assert_!(fut2.as_mut().poll(cx).is_pending());
                assert_!(fut3.as_mut().poll(cx).is_pending());
                assert_!(fut4.as_mut().poll(cx).is_pending());
                assert_!(fut5.as_mut().poll(cx).is_pending());

                assert_eq_!(count, 0);

                // Unlock, notifying the next waiter in line.
                drop(guard1);
                assert_eq_!(count, 1);

                // Ignore the notification.
                drop(fut2);
                assert_eq_!(count, 2);

                // Unlock - mutex should be available again
                let guard3 = match fut3.as_mut().poll(cx) {
                    Poll::Pending => panic!("lock busy"),
                    Poll::Ready(guard) => guard,
                };

                drop(guard3);
                assert_eq_!(count, 3);
                drop(fut4);
                assert_eq_!(count, 4);

                match fut5.as_mut().poll(cx) {
                    Poll::Pending => panic!("lock busy"),
                    Poll::Ready(guard) => (),
                };

                // We also test cancelling read locks.
                let guard = lock.try_write().unwrap();

                let mut fut6 = Box::pin(lock.upgradable_read());
                let mut fut7 = Box::pin(lock.upgradable_read());

                assert_!(fut6.as_mut().poll(cx).is_pending());
                assert_!(fut7.as_mut().poll(cx).is_pending());

                assert_eq_!(count, 4);
                drop(guard);
                assert_eq_!(count, 5);
                drop(fut6);
                assert_eq_!(count, 6);

                assert_!(fut7.as_mut().poll(cx).is_ready());

            }
        }

        #[test]
        fn new_waiters_on_unfair_lock_can_acquire_future_while_another_task_is_notified() {
            let (waker, count) = new_count_waker();
            let cx = &mut Context::from_waker(&waker);
            let lock = $rwlock_type::new(5, false);

            let mut guard1 = lock.try_write().unwrap();

            // The second and third lock attempt must fail
            let mut fut2 = Box::pin(lock.write());
            let mut fut3 = Box::pin(lock.upgradable_read());
            let mut fut4 = Box::pin(lock.read());
            let mut fut5 = Box::pin(lock.write());

            assert_!(fut2.as_mut().poll(cx).is_pending());
            assert_!(fut3.as_mut().poll(cx).is_pending());
            assert_!(fut4.as_mut().poll(cx).is_pending());

            // Notify fut2.
            drop(guard1);
            assert_eq_!(count, 1);

            // Steal the lock.
            let guard5 = match fut5.as_mut().poll(cx) {
                Poll::Pending => panic_!("Expect mutex to get locked"),
                Poll::Ready(guard) => guard,
            };
            // This future must requeue.
            assert_!(fut2.as_mut().poll(cx).is_pending());

            // ...but the other one preserve their old position in the queue.
            assert_!(fut3.as_mut().poll(cx).is_pending());
            assert_!(fut4.as_mut().poll(cx).is_pending());

            // When we drop fut3, the mutex should signal that it's available for
            // fut2 and fut3 since they have priority in the queue.
            assert_eq_!(count, 1);
            drop(guard5);
            assert_eq_!(count, 3);

            // We streal the lock again.
            let guard2 = match fut2.as_mut().poll(cx) {
                Poll::Pending => panic_!("Expect mutex to get locked"),
                Poll::Ready(guard) => guard,
            };

            // The two notified futures must register again.
            assert_!(fut3.as_mut().poll(cx).is_pending());
            assert_!(fut4.as_mut().poll(cx).is_pending());

            // Now we notify the two futures.
            drop(guard2);
            assert_eq_!(count, 5);

            assert_!(fut3.as_mut().poll(cx).is_ready());
            assert_!(fut4.as_mut().poll(cx).is_ready());
        }

        #[test]
        fn new_waiters_on_fair_mutex_cant_acquire_future_while_another_task_is_notified() {
            let (waker, count) = new_count_waker();
            let cx = &mut Context::from_waker(&waker);
            let lock = $rwlock_type::new(5, true);

            // Lock the mutex
            let mut guard1 = lock.try_write().unwrap();

            let mut fut2 = Box::pin(lock.write());
            let mut fut3 = Box::pin(lock.upgradable_read());
            let mut fut4 = Box::pin(lock.read());
            let mut fut5 = Box::pin(lock.write());

            assert_!(fut2.as_mut().poll(cx).is_pending());

            // Notify fut2.
            drop(guard1);
            assert_eq_!(count, 1);

            // Try to steal the lock.
            assert_!(fut3.as_mut().poll(cx).is_pending());
            assert_!(fut4.as_mut().poll(cx).is_pending());
            assert_!(fut5.as_mut().poll(cx).is_pending());

            // Take the lock.
            assert_!(fut2.as_mut().poll(cx).is_ready());

            // Now fut3 & fut4 should have been signaled and be lockable
            assert_eq_!(count, 3);

            // Try to steal the lock.
            assert_!(fut5.as_mut().poll(cx).is_pending());

            // Take the lock.
            assert_!(fut3.as_mut().poll(cx).is_ready());
            assert_!(fut4.as_mut().poll(cx).is_ready());

            // Now fut5 should have been signaled and be lockable
            assert_eq_!(count, 4);

            assert_!(fut5.as_mut().poll(cx).is_ready());
        }

        #[test]
        fn waiters_on_fair_mutex_cant_acquire_future_while_another_task_is_notified() {
            let (waker, count) = new_count_waker();
            let cx = &mut Context::from_waker(&waker);
            let lock = $rwlock_type::new(5, true);

            // Lock the mutex
            let mut guard1 = lock.try_write().unwrap();

            let mut fut2 = Box::pin(lock.write());
            let mut fut3 = Box::pin(lock.upgradable_read());
            let mut fut4 = Box::pin(lock.read());
            let mut fut5 = Box::pin(lock.write());

            assert_!(fut2.as_mut().poll(cx).is_pending());
            assert_!(fut3.as_mut().poll(cx).is_pending());
            assert_!(fut4.as_mut().poll(cx).is_pending());

            // Notify fut2.
            drop(guard1);
            assert_eq_!(count, 1);

            // Try to steal the lock.
            assert_!(fut3.as_mut().poll(cx).is_pending());
            assert_!(fut4.as_mut().poll(cx).is_pending());
            assert_!(fut5.as_mut().poll(cx).is_pending());

            // Take the lock.
            assert_!(fut2.as_mut().poll(cx).is_ready());

            // Now fut3 & fut4 should have been signaled and be lockable
            assert_eq_!(count, 3);

            // Try to steal the lock.
            assert_!(fut5.as_mut().poll(cx).is_pending());

            // Take the lock.
            assert_!(fut3.as_mut().poll(cx).is_ready());
            assert_!(fut4.as_mut().poll(cx).is_ready());

            // Now fut5 should have been signaled and be lockable
            assert_eq_!(count, 4);

            // Take the lock.
            assert_!(fut5.as_mut().poll(cx).is_ready());
        }

        #[test]
        fn poll_from_multiple_executors() {
            for is_fair in &[true, false] {
                let (waker_1, count_1) = new_count_waker();
                let (waker_2, count_2) = new_count_waker();
                let lock = $rwlock_type::new(5, *is_fair);

                // Lock the mutex
                let mut guard1 = lock.try_write().unwrap();
                *guard1 = 27;

                let cx_1 = &mut Context::from_waker(&waker_1);
                let cx_2 = &mut Context::from_waker(&waker_2);

                // Wait the read lock and poll using 2 different contexts.
                let fut1 = lock.read();
                pin_mut!(fut1);

                assert_!(fut1.as_mut().poll(cx_1).is_pending());
                assert_!(fut1.as_mut().poll(cx_2).is_pending());

                // Make sure the context was updated properly.
                drop(guard1);
                assert_eq_!(count_1, 0);
                assert_eq_!(count_2, 1);

                let guard2 = match fut1.as_mut().poll(cx_2) {
                    Poll::Pending => panic_!("busy lock"),
                    Poll::Ready(guard) => guard,
                };
                assert_!(fut1.as_mut().is_terminated());

                // Wait for the write lock and poll using 2 different contexts.
                let fut2 = lock.write();
                pin_mut!(fut2);
                assert_!(fut2.as_mut().poll(cx_1).is_pending());
                assert_!(fut2.as_mut().poll(cx_2).is_pending());

                // Make sure the context was updated properly.
                drop(guard2);
                assert_eq_!(count_1, 0);
                assert_eq_!(count_2, 2);

                let guard3 = match fut2.as_mut().poll(cx_2) {
                    Poll::Pending => panic_!("busy lock"),
                    Poll::Ready(guard) => guard,
                };

                // Wait for the upgradable_read lock and poll using 2 different contexts.
                let fut3 = lock.upgradable_read();
                pin_mut!(fut3);
                assert_!(fut3.as_mut().poll(cx_1).is_pending());
                assert_!(fut3.as_mut().poll(cx_2).is_pending());

                // Make sure the context was updated properly.
                drop(guard3);
                assert_eq_!(count_1, 0);
                assert_eq_!(count_2, 3);

                assert_!(fut3.as_mut().poll(cx_2).is_ready());
            }
        }

        #[test]
        fn upgrade_guard_has_priority_over_writer() {
            for is_fair in &[true, false] {
                let (waker, count) = new_count_waker();
                let lock = $rwlock_type::new(5, *is_fair);

                // Acquire the upgradable lock.
                let mut guard1 = lock.try_read().unwrap();
                let mut guard2 = lock.try_upgradable_read().unwrap();

                let cx = &mut Context::from_waker(&waker);

                // Wait for the write lock.
                let fut3 = lock.write();
                pin_mut!(fut3);
                assert_!(fut3.as_mut().poll(cx).is_pending());

                // We can't upgrade because of the other reader.
                let guard2 = guard2.try_upgrade().unwrap_err();

                // Add contention for the write lock.
                let fut4 = guard2.upgrade();
                pin_mut!(fut4);
                assert_!(fut4.as_mut().poll(cx).is_pending());

                assert_eq_!(count, 0);
                // This should wakeup the upgrading upgradable_read because
                // it has priority.
                drop(guard1);
                assert_eq_!(count, 1);

                // We got the write lock first because of priority.
                assert_!(fut4.as_mut().poll(cx).is_ready());
                assert_eq_!(count, 2);

                // Make sure the lock is released properly.
                assert_!(fut3.as_mut().poll(cx).is_ready());
            }
        }

        #[test]
        fn upgrade_guard_sync_contention() {
            for is_fair in &[true, false] {
                let (waker, count) = new_count_waker();
                let lock = $rwlock_type::new(5, *is_fair);

                // Acquire the upgradable lock.
                let mut guard1 = lock.try_read().unwrap();
                let mut guard2 = lock.try_upgradable_read().unwrap();

                // We can't upgrade because of the oother reader.
                let guard2 = guard2.try_upgrade().unwrap_err();
                drop(guard1);

                // Can't acquire a write lock because of the upgradable_read.
                assert_!(lock.try_write().is_none());

                // Now we can upgrade.
                let _guard3 = guard2.try_upgrade().unwrap();
            }
        }
    };
}

gen_rwlock_tests!(local_rwlock_tests, LocalRwLock);

#[cfg(feature = "std")]
mod if_std {
    use super::*;
    use futures::FutureExt;
    use futures_intrusive::sync::RwLock;

    gen_rwlock_tests!(rwlock_tests, RwLock);

    fn is_send<T: Send>(_: &T) {}

    fn is_send_value<T: Send>(_: T) {}

    fn is_sync<T: Sync>(_: &T) {}

    macro_rules! gen_future_send_test {
        ($mod_name:ident, $fut_fn:ident) => {
            mod $mod_name {
                use super::*;

                #[test]
                fn futures_are_send() {
                    let lock = RwLock::new(true, true);
                    is_sync(&lock);
                    {
                        let fut = lock.$fut_fn();
                        is_send(&fut);
                        pin_mut!(fut);
                        is_send(&fut);

                        let waker = &panic_waker();
                        let cx = &mut Context::from_waker(&waker);
                        pin_mut!(fut);
                        let res = fut.poll_unpin(cx);
                        let guard = match res {
                            Poll::Ready(v) => v,
                            Poll::Pending => panic_!("Expected to be ready"),
                        };
                        is_send(&guard);
                        is_send_value(guard);
                    }
                    is_send_value(lock);
                }
            }
        };
    }

    gen_future_send_test!(rwlock_read, read);
    gen_future_send_test!(rwlock_write, write);
    gen_future_send_test!(rwlock_upgradable_read, upgradable_read);
}
