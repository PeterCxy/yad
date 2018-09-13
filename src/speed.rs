use std::collections::VecDeque;
use std::time::{Instant, Duration};
use util::*;

// A simple and naïve speed meter implementation
// It works by first dividing time into small slices
// and save the increment in bytes during each small slice
// into a queue of length N. When the queue is full,
// The first elements will be discarded, thus simulating
// a sliding window. The number read from the SpeedMeter
// is the average speed within the sliding window.
#[derive(Debug)]
pub struct SpeedMeter {
    last_instant: Instant,
    currrent_increment: u64,
    time_slices: VecDeque<(u64, f64)>,
    slice_window_size: usize, // maximum length of the slices queue (sliding window)
    min_slice: Duration // the slice size
}

impl SpeedMeter {
    pub fn new(min_slice: Duration, slice_window_size: usize) -> SpeedMeter {
        SpeedMeter {
            last_instant: Instant::now(),
            currrent_increment: 0,
            time_slices: VecDeque::with_capacity(slice_window_size * 2),
            slice_window_size,
            min_slice
        }
    }

    pub fn add(&mut self, delta: u64) -> bool {
        self.currrent_increment += delta;
        let now = Instant::now();
        let since_last = now.duration_since(self.last_instant);

        if since_last >= self.min_slice {
            // A minimum slice has passed, we can now save it into the queue
            // First check if we need to discard anything
            // i.e. if the window needs sliding
            if self.time_slices.len() >= self.slice_window_size {
                self.time_slices.pop_front();
            }

            // The durations won't be exactly min_slice, thus we store the
            // actual durations instead of just assuming each of them is
            // min_slice
            self.time_slices.push_back(
                (self.currrent_increment, duration_to_secs_float(since_last)));
            self.last_instant = now;
            self.currrent_increment = 0;
            return true;
        } else {
            return false;
        }
    }

    pub fn get_speed_per_sec(&self) -> f64 {
        // Calculate the sum of the increments and durations of each slice
        let (sum_incr, sum_dur) = self.time_slices.iter().fold((0, 0f64), |sum, current| {
            (sum.0 + current.0, sum.1 + current.1)
        });

        sum_incr as f64 / sum_dur
    }
}