use futures_intrusive::sync::{Mutex as IntrusiveMutex, Semaphore};

use std::sync::Arc;

async fn do_iteration() {
    // Changing this to a higher value will prevent it from getting stuck
    const NR_ITERATIONS: usize = 10;
    let m = Arc::new(IntrusiveMutex::new((), true));
    let wait_sem = Arc::new(Semaphore::new(false, 0));

    let m = m.clone();
    let s = wait_sem.clone();
    tokio::spawn(async move {
        for _ in 0..NR_ITERATIONS {
            let _ = m.lock().await;
        }
        s.release(1);
    });

    wait_sem.acquire(1).await;
}

fn main() {
    const NR_ITERATIONS: usize = 10000;
    const BLOCK_ON_PER_RT: usize = 3000;

    for i in 0 .. NR_ITERATIONS {
        {
            println!("Starting runtime {}", i);
            let mut builder = tokio::runtime::Builder::new();
            builder.thread_pool().num_threads(1);
            let mut rt = builder.build().unwrap();

            for _ in 0 .. BLOCK_ON_PER_RT {
                rt.block_on(async {
                    do_iteration().await;
                });
            }

            println!("Stopping runtime");
        }
        println!("Stopped runtime");
    }
}