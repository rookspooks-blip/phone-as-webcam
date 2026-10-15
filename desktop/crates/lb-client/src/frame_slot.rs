//! "Latest frame wins" slot shared by producers (decoder) and several
//! consumers (preview, virtual camera). Slow consumers simply skip frames,
//! they never build up latency.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::decode::VideoFrame;

#[derive(Default)]
pub struct LatestFrame {
    inner: Mutex<(u64, Option<Arc<VideoFrame>>)>,
    cv: Condvar,
}

impl LatestFrame {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn publish(&self, frame: VideoFrame) {
        let mut g = self.inner.lock().unwrap();
        g.0 += 1;
        g.1 = Some(Arc::new(frame));
        self.cv.notify_all();
    }

    /// Returns the newest frame if its generation is newer than `seen`.
    pub fn get_newer(&self, seen: u64) -> Option<(u64, Arc<VideoFrame>)> {
        let g = self.inner.lock().unwrap();
        match &g.1 {
            Some(f) if g.0 > seen => Some((g.0, f.clone())),
            _ => None,
        }
    }

    /// Blocks until a frame newer than `seen` arrives or `timeout` elapses.
    pub fn wait_newer(&self, seen: u64, timeout: Duration) -> Option<(u64, Arc<VideoFrame>)> {
        let g = self.inner.lock().unwrap();
        let (g, _) = self.cv.wait_timeout_while(g, timeout, |g| g.0 <= seen).unwrap();
        match &g.1 {
            Some(f) if g.0 > seen => Some((g.0, f.clone())),
            _ => None,
        }
    }

    pub fn generation(&self) -> u64 {
        self.inner.lock().unwrap().0
    }
}
