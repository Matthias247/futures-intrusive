#![feature(collapse_debuginfo)]

use futures::future::{FusedFuture, Future};
use futures::task::{Context, Poll};
use futures_intrusive::sync::LocalRwLock;
use futures_test::task::{new_count_waker, panic_waker};
use pin_utils::pin_mut;

// Allows backtrace to work properly.
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
                    assert_eq!(false, lock.is_exclusive());

                    {
                        let mut guards = Vec::with_capacity(3);
                        for _ in 0..3 {
                            let fut = lock.read();
                            pin_mut!(fut);
                            match fut.as_mut().poll(cx) {
                                Poll::Pending => {
                                    panic!("Expect lock to get locked")
                                }
                                Poll::Ready(mut guard) => {
                                    assert_eq!(false, lock.is_exclusive());
                                    assert_eq!(5, *guard);
                                    guards.push(guard);
                                    assert_eq!(guards.len(), lock.nb_readers());
                                }
                            };
                            assert!(fut.as_mut().is_terminated());
                        }

                        assert_eq!(3, lock.nb_readers());

                        drop(guards.pop().unwrap());
                        assert_eq!(2, lock.nb_readers());

                        drop(guards.pop().unwrap());
                        assert_eq!(1, lock.nb_readers());

                        drop(guards.pop().unwrap());
                        assert_eq!(0, lock.nb_readers());
                    }

                    {
                        let fut = lock.read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic!("Expect lock to get locked")
                            }
                            Poll::Ready(guard) => {
                                assert_eq!(false, lock.is_exclusive());
                                assert_eq!(1, lock.nb_readers());
                                assert_eq!(5, *guard);
                            }
                        };
                    }

                    assert_eq!(0, lock.nb_readers());
                }
            }

            #[test]
            fn uncontended_write() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    {
                        let fut = lock.write();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic!("Expect lock to get locked")
                            }
                            Poll::Ready(mut guard) => {
                                assert_eq!(true, lock.is_exclusive());
                                assert_eq!(5, *guard);
                                *guard = 12;
                                assert_eq!(12, *guard);
                            }
                        };
                        assert!(fut.as_mut().is_terminated());
                    }
                    assert_eq!(false, lock.is_exclusive());

                    {
                        let fut = lock.write();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic!("Expect lock to get locked")
                            }
                            Poll::Ready(guard) => {
                                assert_eq!(true, lock.is_exclusive());
                                assert_eq!(12, *guard);
                            }
                        };
                    }

                    assert_eq!(false, lock.is_exclusive());
                }
            }

            #[test]
            fn uncontended_upgradable_read() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    {
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        let guard = match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic!("Expect lock to get locked")
                            }
                            Poll::Ready(mut guard) => {
                                assert_eq!(false, lock.is_exclusive());
                                assert_eq!(5, *guard);
                                guard
                            }
                        };
                        assert!(fut.as_mut().is_terminated());

                        let fut = guard.upgrade();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic!("Expect lock to get locked")
                            }
                            Poll::Ready(mut guard) => {
                                assert_eq!(true, lock.is_exclusive());
                                assert_eq!(5, *guard);
                                *guard = 12;
                                assert_eq!(12, *guard);
                            }
                        };
                    }

                    assert_eq!(false, lock.is_exclusive());

                    {
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => {
                                panic!("Expect lock to get locked")
                            }
                            Poll::Ready(guard) => {
                                assert_eq!(false, lock.is_exclusive());
                                assert_eq!(12, *guard);
                            }
                        };
                    }

                    assert_eq!(false, lock.is_exclusive());
                }
            }

            #[test]
            fn contended_read() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    let guard = lock.try_write().unwrap();

                    {
                        assert!(lock.try_read().is_none());
                        let fut = lock.read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => (),
                            Poll::Ready(mut guard) => panic!("rwlock ready"),
                        };
                        assert!(!fut.as_mut().is_terminated());
                    }

                    {
                        assert!(lock.try_read().is_none());
                        let fut = lock.read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => (),
                            Poll::Ready(mut guard) => panic!("rwlock ready"),
                        };
                        assert!(!fut.as_mut().is_terminated());
                    }
                }
            }

            #[test]
            fn contended_write() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    let guard = lock.try_read().unwrap();

                    {
                        assert!(lock.try_write().is_none());
                        let fut = lock.write();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => (),
                            Poll::Ready(mut guard) => panic!("rwlock ready"),
                        };
                        assert!(!fut.as_mut().is_terminated());
                    }
                }
            }

            #[test]
            fn contended_upgrade_read() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    let guard = lock.try_upgradable_read().unwrap();
                    {
                        assert!(lock.try_upgradable_read().is_none());
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => (),
                            Poll::Ready(mut guard) => panic!("rwlock ready"),
                        };
                        assert!(!fut.as_mut().is_terminated());
                    }
                    drop(guard);

                    let guard = lock.try_write().unwrap();
                    {
                        assert!(lock.try_upgradable_read().is_none());
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => (),
                            Poll::Ready(mut guard) => panic!("rwlock ready"),
                        };
                        assert!(!fut.as_mut().is_terminated());
                    }
                }
            }

            #[test]
            fn dropping_reader_wakes_up_writer() {
                for is_fair in &[true, false] {
                    let (waker, count) = new_count_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    let guard = lock.try_read().unwrap();

                    let mut fut = lock.write();
                    pin_mut!(fut);
                    match fut.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };
                    assert!(!fut.as_mut().is_terminated());

                    assert_eq!(count, 0);

                    drop(guard);
                    assert_eq!(count, 1);

                    match fut.as_mut().poll(cx) {
                        Poll::Pending => panic!("rwlock busy"),
                        Poll::Ready(mut guard) => (),
                    };
                    assert!(fut.as_mut().is_terminated());
                }
            }

            #[test]
            fn dropping_writer_wakes_up_readers() {
                for is_fair in &[true, false] {
                    let (waker, count) = new_count_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    let guard = lock.try_write().unwrap();

                    let fut1 = lock.read();
                    pin_mut!(fut1);
                    match fut1.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };
                    assert!(!fut1.as_mut().is_terminated());

                    let fut2 = lock.upgradable_read();
                    pin_mut!(fut2);
                    match fut2.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };
                    assert!(!fut2.as_mut().is_terminated());

                    assert_eq!(count, 0);

                    drop(guard);
                    assert_eq!(count, 2);

                    match fut1.as_mut().poll(cx) {
                        Poll::Pending => panic!("rwlock busy"),
                        Poll::Ready(mut guard) => (),
                    };
                    assert!(fut1.as_mut().is_terminated());

                    match fut2.as_mut().poll(cx) {
                        Poll::Pending => panic!("rwlock busy"),
                        Poll::Ready(mut guard) => (),
                    };
                    assert!(fut2.as_mut().is_terminated());
                }
            }

            #[test]
            #[should_panic]
            fn poll_read_after_completion_should_panic() {
                for is_fair in &[true, false] {
                    let waker = &panic_waker();
                    let cx = &mut Context::from_waker(&waker);
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    {
                        let fut = lock.read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => panic!("rwlock busy"),
                            Poll::Ready(mut guard) => (),
                        };
                        assert!(fut.as_mut().is_terminated());

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
                    assert_eq!(false, lock.is_exclusive());

                    {
                        let fut = lock.write();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => panic!("rwlock busy"),
                            Poll::Ready(mut guard) => (),
                        };
                        assert!(fut.as_mut().is_terminated());

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
                    assert_eq!(false, lock.is_exclusive());

                    {
                        let fut = lock.upgradable_read();
                        pin_mut!(fut);
                        match fut.as_mut().poll(cx) {
                            Poll::Pending => panic!("rwlock busy"),
                            Poll::Ready(mut guard) => (),
                        };
                        assert!(fut.as_mut().is_terminated());

                        let _ = fut.as_mut().poll(cx);
                    }
                }
            }

            #[test]
            fn dropping_writer_follow_queue_order_for_wakeup() {
                for is_fair in &[true, false] {
                    let lock = $rwlock_type::new(5, *is_fair);
                    assert_eq!(false, lock.is_exclusive());

                    let (waker, count) = new_count_waker();
                    let cx = &mut Context::from_waker(&waker);

                    let writer = lock.try_write().unwrap();

                    let read_fut1 = lock.read();
                    pin_mut!(read_fut1);
                    match read_fut1.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };

                    let read_fut2 = lock.read();
                    pin_mut!(read_fut2);
                    match read_fut2.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };

                    let upgrade_read_fut1 = lock.upgradable_read();
                    pin_mut!(upgrade_read_fut1);
                    match upgrade_read_fut1.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };

                    let upgrade_read_fut2 = lock.upgradable_read();
                    pin_mut!(upgrade_read_fut2);
                    match upgrade_read_fut2.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };

                    let write_fut1 = lock.write();
                    pin_mut!(write_fut1);
                    match write_fut1.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };

                    let write_fut2 = lock.write();
                    pin_mut!(write_fut2);
                    match write_fut2.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };

                    let read_fut3 = lock.read();
                    pin_mut!(read_fut3);
                    match read_fut3.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("rwlock ready"),
                    };

                    assert_eq!(count, 0);

                    drop(writer);

                    // Wakeup the three readers and 1 upgradable_read.
                    assert_eq!(count, 4);

                    let guard1 = match read_fut1.as_mut().poll(cx) {
                        Poll::Pending => panic!("busy rwlock"),
                        Poll::Ready(mut guard) => guard,
                    };
                    let guard2 = match read_fut2.as_mut().poll(cx) {
                        Poll::Pending => panic!("busy rwlock"),
                        Poll::Ready(mut guard) => guard,
                    };
                    let guard3 = match upgrade_read_fut1.as_mut().poll(cx) {
                        Poll::Pending => panic!("busy rwlock"),
                        Poll::Ready(mut guard) => guard,
                    };
                    let guard4 = match read_fut3.as_mut().poll(cx) {
                        Poll::Pending => panic!("busy rwlock"),
                        Poll::Ready(mut guard) => guard,
                    };
                    match upgrade_read_fut2.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("busy rwlock"),
                    };
                    match write_fut1.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("busy rwlock"),
                    };
                    match write_fut2.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("busy rwlock"),
                    };

                    assert_eq!(count, 4);

                    drop(guard1);
                    assert_eq!(count, 4);

                    drop(guard3);
                    assert_eq!(count, 5);

                    let guard5 = match upgrade_read_fut2.as_mut().poll(cx) {
                        Poll::Pending => panic!("busy rwlock"),
                        Poll::Ready(mut guard) => guard,
                    };
                    match write_fut1.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("busy rwlock"),
                    };
                    match write_fut2.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("busy rwlock"),
                    };

                    drop(guard2);
                    assert_eq!(count, 5);
                    drop(guard5);
                    assert_eq!(count, 5);
                    drop(guard4);
                    assert_eq!(count, 6);

                    let guard6 = match write_fut1.as_mut().poll(cx) {
                        Poll::Pending => panic!("busy rwlock"),
                        Poll::Ready(mut guard) => guard,
                    };
                    match write_fut2.as_mut().poll(cx) {
                        Poll::Pending => (),
                        Poll::Ready(mut guard) => panic!("busy rwlock"),
                    };

                    drop(guard6);
                    assert_eq!(count, 7);

                    match write_fut2.as_mut().poll(cx) {
                        Poll::Pending => panic!("busy rwlock"),
                        Poll::Ready(mut guard) => (),
                    };
                    assert_eq!(count, 7);
                }
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
                            Poll::Pending => panic!("Expected to be ready"),
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
