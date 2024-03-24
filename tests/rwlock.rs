use futures::future::{FusedFuture, Future};
use futures::task::{Context, Poll};
use futures_intrusive::sync::LocalRwLock;
use futures_test::task::{new_count_waker, panic_waker};
use pin_utils::pin_mut;

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
                                    panic!("Expect mutex to get locked")
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
                                panic!("Expect mutex to get locked")
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
        }
    };
}

gen_rwlock_tests!(local_rwlock_tests, LocalRwLock);

#[cfg(feature = "std")]
mod if_std {
    use super::*;
    use futures::FutureExt;
    use futures_intrusive::sync::RwLock;

    gen_rwlock_tests!(mutex_tests, RwLock);

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
