use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hybrid Logical Clock.
///
/// Total order: `(ms, counter, device)`. `device` (short id) breaks ties
/// between two devices that happen to produce equal (ms, counter).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hlc {
    pub ms: u64,
    pub counter: u32,
    pub device: String,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Hlc {
    pub fn now(device: impl Into<String>) -> Self {
        Self {
            ms: now_ms(),
            counter: 0,
            device: device.into(),
        }
    }

    pub fn from_parts(ms: u64, counter: u32, device: String) -> Self {
        Self {
            ms,
            counter,
            device,
        }
    }

    /// Generate a timestamp for a local event, keeping our own device id.
    pub fn tick(&mut self) -> &Self {
        let pt = now_ms();
        if pt > self.ms {
            self.ms = pt;
            self.counter = 0;
        } else {
            self.counter += 1;
        }
        self
    }

    /// Merge a remote timestamp into the clock (classic HLC receive),
    /// keeping our own device id. The result is always strictly greater
    /// than both the previous local clock and the remote timestamp, so a
    /// local event after the receive (via `tick`) can never be ordered
    /// before the received event.
    pub fn receive(&mut self, remote: &Hlc) -> &Self {
        let pt = now_ms();
        let l = (self.ms, self.counter);
        let r = (remote.ms, remote.counter);
        let p = (pt, 0u32);
        if r > l && r > p {
            self.ms = r.0;
            self.counter = r.1 + 1;
        } else if p > l && p > r {
            self.ms = p.0;
            self.counter = 0;
        } else {
            self.ms = l.0.max(r.0);
            self.counter = l.1.max(r.1) + 1;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_by_ms_then_counter_then_device() {
        let a = Hlc::from_parts(100, 0, "a".into());
        let b = Hlc::from_parts(100, 1, "a".into());
        let c = Hlc::from_parts(101, 0, "a".into());
        let d = Hlc::from_parts(100, 0, "b".into());
        assert!(a < b);
        assert!(b < c);
        assert!(a < d);
    }

    #[test]
    fn tick_advances() {
        let mut h = Hlc::from_parts(1000, 0, "a".into());
        // now_ms() is far in the future, so tick should jump to physical time
        h.tick();
        assert!(h.ms >= 1_000_000_000_000);
        let prev = h.clone();
        h.tick();
        assert!(h > prev);
    }

    #[test]
    fn receive_stays_strictly_above_both() {
        let mut local = Hlc::now("a");
        local.counter = 3;
        let before = local.clone();
        let mut remote = Hlc::now("b");
        remote.counter = 5;
        local.receive(&remote);
        // HLC invariant: strictly above both inputs, own device kept
        assert!(local > before);
        assert!(local > remote);
        assert_eq!(local.device, "a");
        assert!(local.ms >= remote.ms);
    }

    #[test]
    fn receive_stays_above_newer_local() {
        let mut local = Hlc::from_parts(1000, 5, "a".into());
        let remote = Hlc::from_parts(100, 9, "b".into());
        local.receive(&remote);
        assert!(local > remote);
        assert!(local.ms >= 1000);
    }

    #[test]
    fn tick_after_receive_beats_remote() {
        // regression: a remote ts at the current physical ms used to leave
        // the clock stale; the next tick reset the counter to 0 and produced
        // a value OLDER than the remote, losing LWW on the next sync
        let mut local = Hlc::from_parts(now_ms().saturating_sub(50), 0, "a".into());
        let remote = Hlc::from_parts(now_ms(), 1, "b".into());
        local.receive(&remote);
        local.tick();
        assert!(local > remote);
    }
}
