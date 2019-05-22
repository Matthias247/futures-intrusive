#![feature(async_await)]

use futures::{join, FutureExt};
use futures::{
    channel::mpsc,
    executor::block_on,
    future::join_all,
    sink::SinkExt,
    stream::StreamExt};
use futures_intrusive::channel::{
    LocalChannel,
    shared::channel,
    shared::unbuffered_channel,
};
use criterion::{Criterion, criterion_group, criterion_main, ParameterizedBenchmark};
use std::time::Duration;

/// variable producers, single consumer
fn futchan_bounded_variable_tx(producers: usize) {
    const ELEMS_TO_SEND: usize = 3000;
    let elems_per_producer = ELEMS_TO_SEND/producers;
    let (tx, mut rx) = mpsc::channel::<i32>(20);

    for _i in 0..producers {
        let mut tx = tx.clone();
        std::thread::spawn(move || {
            block_on(async {
                for _i in 0..elems_per_producer {
                    tx.send(4).await.unwrap();
                }
            });
        });
    }

    drop(tx);

    block_on(async {
        loop {
            let res = rx.next().await;
            if res.is_none() {
                break;
            }
        }
    });
}

/// variable producers, single consumer
fn intrusivechan_bounded_variable_tx(producers: usize) {
    const ELEMS_TO_SEND: usize = 3000;
    let elems_per_producer = ELEMS_TO_SEND/producers;
    let (tx, rx) = channel::<i32>(20);

    for _i in 0..producers {
        let tx = tx.clone();

        std::thread::spawn(move || {
            block_on(async {
                for _i in 0..elems_per_producer {
                    let r = tx.send(4).await;
                    assert!(r.is_ok());
                }
            });
        });
    }

    drop(tx);

    block_on(async {
        loop {
            let res = rx.receive().await;
            if res.is_none() {
                break;
            }
        }
    });
}

/// variable producers, single consumer
fn intrusivechan_unbuffered_variable_tx(producers: usize) {
    const ELEMS_TO_SEND: usize = 3000;
    let elems_per_producer = ELEMS_TO_SEND/producers;
    let (tx, rx) = unbuffered_channel::<i32>();

    for _i in 0..producers {
        let tx = tx.clone();
        std::thread::spawn(move || {
            block_on(async {
                for _i in 0..elems_per_producer {
                    let r = tx.send(4).await;
                    assert!(r.is_ok());
                }
            });
        });
    }

    drop(tx);

    block_on(async {
        loop {
            let res = rx.receive().await;
            if res.is_none() {
                break;
            }
        }
    });
}

/// variable producers, single consumer
fn futchan_bounded_variable_tx_single_thread(producers: usize) {
    const ELEMS_TO_SEND: usize = 3000;
    let elems_per_producer = ELEMS_TO_SEND/producers;

    block_on(async {
        let (tx, mut rx) = mpsc::channel::<i32>(20);
        let produce_done = join_all((0..producers).into_iter().map(|_|{
            let mut tx = tx.clone();
            async move {
                for _i in 0..elems_per_producer {
                    tx.send(4).await.unwrap();
                }
            }.boxed()}));

        drop(tx);

        let consume_done = async {
            loop {
                let res = rx.next().await;
                if res.is_none() {
                    break;
                }
            }
        };

        join!(produce_done, consume_done);
    });
}

/// variable producers, single consumer
fn intrusivechan_bounded_variable_tx_single_thread(producers: usize) {
    const ELEMS_TO_SEND: usize = 3000;
    let elems_per_producer = ELEMS_TO_SEND/producers;

    block_on(async {
        let (tx, rx) = channel::<i32>(20);
        let produce_done = join_all((0..producers).into_iter().map(|_|{
            let tx = tx.clone();
            Box::pin(async move {
                for _i in 0..elems_per_producer {
                    let r = tx.send(4).await;
                    assert!(r.is_ok());
                }
            })}));

        drop(tx);

        let consume_done = async {
            loop {
                let res = rx.receive().await;
                if res.is_none() {
                    break;
                }
            }
        };

        join!(produce_done, consume_done);
    });
}

/// variable producers, single consumer
fn intrusivechan_unbuffered_variable_tx_single_thread(producers: usize) {
    const ELEMS_TO_SEND: usize = 3000;
    let elems_per_producer = ELEMS_TO_SEND/producers;

    block_on(async {
        let (tx, rx) = unbuffered_channel::<i32>();
        let produce_done = join_all((0..producers).into_iter().map(|_|{
            let tx = tx.clone();
            Box::pin(async move {
                for _i in 0..elems_per_producer {
                    let r = tx.send(4).await;
                    assert!(r.is_ok());
                }
            })}));

        drop(tx);

        let consume_done = async {
            loop {
                let res = rx.receive().await;
                if res.is_none() {
                    break;
                }
            }
        };

        join!(produce_done, consume_done);
    });
}

/// variable producers, single consumer
fn intrusive_local_chan_bounded_variable_tx_single_thread(producers: usize) {
    const ELEMS_TO_SEND: usize = 3000;
    let elems_per_producer = ELEMS_TO_SEND/producers;

    block_on(async {
        let rx = LocalChannel::<i32, [i32; 20]>::new();
        let produce_done = join_all((0..producers).into_iter().map(|_|{
            Box::pin(async {
                for _i in 0..elems_per_producer {
                    let r = rx.send(4).await;
                    assert!(r.is_ok());
                }
            })}));

        let consume_done = async {
            let mut count = 0;
            let needed = elems_per_producer * producers;
            loop {
                let _ = rx.receive().await.unwrap();
                // The channel doesn't automatically get closed when producers are
                // gone since producer and consumer are the same object type.
                // Therefore we need to count receives.
                count += 1;
                if count == needed {
                    break;
                }
            }
        };

        join!(produce_done, consume_done);
    });
}

fn criterion_benchmark(c: &mut Criterion) {
    // Producer and consumer are running on the same thread
    c.bench(
        "Channels (Single Threaded)",
        ParameterizedBenchmark::new(
            "intrusive local channel with producers",
            |b, &&producers| b.iter(|| intrusive_local_chan_bounded_variable_tx_single_thread(producers)),
            &[5, 20, 100])
        .with_function(
            "intrusive channel with producers",
            |b, &&producers| b.iter(|| intrusivechan_bounded_variable_tx_single_thread(producers)))
        .with_function(
            "intrusive unbuffered channel with producers",
            |b, &&producers| b.iter(|| intrusivechan_unbuffered_variable_tx_single_thread(producers)))
        .with_function(
            "futures::channel::mpsc with producers",
            |b, &&producers| b.iter(|| futchan_bounded_variable_tx_single_thread(producers)))
        );

    // Producer and consume run on a different thread
    c.bench(
        "Channels (Thread per producer)",
        ParameterizedBenchmark::new(
            "intrusive channel with producers",
            |b, &&producers| b.iter(|| intrusivechan_bounded_variable_tx(producers)),
            &[5, 20, 100])
        .with_function(
            "intrusive unbuffered channel with producers",
            |b, &&producers| b.iter(|| intrusivechan_unbuffered_variable_tx(producers)))
        .with_function(
            "futures::channel::mpsc with producers",
            |b, &&producers| b.iter(|| futchan_bounded_variable_tx(producers)))
        );
}

criterion_group!{
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(10)).nresamples(50);
    targets = criterion_benchmark
}
criterion_main!(benches);