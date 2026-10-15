//! Server/client clock offset estimation from ping/pong (PROTOCOL.md §7.4).
//!
//! `offset = server_time - client_time`; the sample with the smallest RTT in a
//! sliding window is trusted most, because its one-way delays are the most
//! symmetric.

use std::collections::VecDeque;

const WINDOW_US: u64 = 30_000_000;

#[derive(Debug, Clone, Copy)]
struct Sample {
    at_us: u64,
    rtt_us: u64,
    offset_us: i64,
}

#[derive(Debug, Default)]
pub struct ClockSync {
    samples: VecDeque<Sample>,
}

impl ClockSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// `c0` = client send time, `s1` = server time in pong, `c2` = client receive time.
    pub fn add(&mut self, c0: u64, s1: u64, c2: u64) {
        if c2 < c0 {
            return;
        }
        let rtt_us = c2 - c0;
        let mid = (c0 as i128 + c2 as i128) / 2;
        let offset_us = (s1 as i128 - mid) as i64;
        self.samples.push_back(Sample { at_us: c2, rtt_us, offset_us });
        while let Some(front) = self.samples.front() {
            if c2.saturating_sub(front.at_us) > WINDOW_US {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    fn best(&self) -> Option<&Sample> {
        self.samples.iter().min_by_key(|s| s.rtt_us)
    }

    pub fn offset_us(&self) -> Option<i64> {
        self.best().map(|s| s.offset_us)
    }

    pub fn min_rtt_us(&self) -> Option<u64> {
        self.best().map(|s| s.rtt_us)
    }

    pub fn last_rtt_us(&self) -> Option<u64> {
        self.samples.back().map(|s| s.rtt_us)
    }

    /// Converts a server timestamp to the client clock.
    pub fn to_client(&self, server_us: u64) -> Option<u64> {
        self.offset_us().map(|o| (server_us as i64 - o).max(0) as u64)
    }

    pub fn reset(&mut self) {
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_min_rtt_sample() {
        let mut c = ClockSync::new();
        // True offset 1_000_000. Asymmetric slow sample first, then a clean one.
        c.add(100, 1_000_000 + 100 + 900, 1_100); // rtt 1000, skewed
        c.add(2_000, 1_000_000 + 2_010, 2_020); // rtt 20, near-symmetric
        assert_eq!(c.min_rtt_us(), Some(20));
        assert_eq!(c.offset_us(), Some(1_000_000));
        assert_eq!(c.to_client(1_000_000 + 5_000), Some(5_000));
    }

    #[test]
    fn window_expires_old_samples() {
        let mut c = ClockSync::new();
        c.add(0, 500, 10);
        c.add(40_000_000, 40_000_000 + 700, 40_000_100);
        assert_eq!(c.min_rtt_us(), Some(100));
    }
}
