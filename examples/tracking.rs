use std::time::Duration;

use parking_lot::RawMutex;
use rand::{thread_rng, Rng};
use running_average::{RealTimeRunningAverage, RealTimeSource};
use tokio::time::delay_for;

use futures_intrusive::buffer::GrowingHeapBuf;
use futures_intrusive::channel::shared::generic_channel_with_tracker;
use futures_intrusive::tracking::Tracker;
use histogram::Histogram;

pub struct RunningAverageTracker {
    incoming: RealTimeRunningAverage<f32, RealTimeSource>,
    outgoing: RealTimeRunningAverage<f32, RealTimeSource>,
    vals_histogram: Histogram,
}

impl Default for RunningAverageTracker {
    fn default() -> Self {
        RunningAverageTracker {
            incoming: Default::default(),
            outgoing: Default::default(),
            vals_histogram: Default::default(),
        }
    }
}

#[derive(Debug)]
pub struct RunningAverageReport {
    incoming_rate: f64,
    outgoing_rate: f64,
    vals_median: f64,
}

impl<T: Into<u64> + Copy> Tracker<T> for RunningAverageTracker {
    type Report = RunningAverageReport;

    fn track_incoming(&mut self, item: &T) {
        self.incoming.insert(1f32);
        self.vals_histogram.increment((*item).into()).unwrap();
    }

    fn track_outgoing(&mut self, _item: &T) {
        self.outgoing.insert(1f32);
    }

    fn report(&mut self) -> Self::Report {
        RunningAverageReport {
            incoming_rate: self.incoming.measurement().to_rate(),
            outgoing_rate: self.outgoing.measurement().to_rate(),
            vals_median: self.vals_histogram.percentile(50.0).unwrap_or(0)
                as f64,
        }
    }
}

#[tokio::main]
async fn main() {
    let tracker = RunningAverageTracker {
        incoming: RealTimeRunningAverage::new(Duration::from_secs(15)),
        outgoing: RealTimeRunningAverage::new(Duration::from_secs(15)),
        vals_histogram: Default::default(),
    };

    let (tx, rx) =
        generic_channel_with_tracker::<RawMutex, u64, GrowingHeapBuf<_>, _>(
            16, tracker,
        );

    tokio::spawn({
        let tx = tx.clone();

        async move {
            loop {
                let val = thread_rng().gen_range(0, 1000);
                tx.send(val).await.unwrap();
                let delay = thread_rng().gen_range(0, 50);
                delay_for(Duration::from_millis(delay)).await;
            }
        }
    });

    tokio::spawn(async move {
        while let Some(_item) = rx.receive().await {
            let delay = thread_rng().gen_range(0, 100);
            delay_for(Duration::from_millis(delay)).await;
        }
    });

    loop {
        println!("{:?}", tx.tracker_report());
        delay_for(Duration::from_millis(1000)).await;
    }
}
