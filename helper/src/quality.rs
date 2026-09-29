//! Turns the last 30 seconds of measurements into the numbers the app judges.

use cakevpn_proto::Quality;
use std::collections::VecDeque;

/// Samples are taken every 2 seconds, so 15 of them cover 30 seconds.
const WINDOW: usize = 15;

#[derive(Default)]
pub struct Tracker {
    gateway: VecDeque<Option<f64>>,
    /// Some routers never answer pings; they must not look like a broken Wi-Fi.
    gateway_ever_answered: bool,
    tunnel_delay_ms: Option<u32>,
    tunnel_failures: u32,
}

impl Tracker {
    pub fn gateway_sample(&mut self, rtt_ms: Option<f64>) {
        self.gateway_ever_answered |= rtt_ms.is_some();
        if self.gateway.len() == WINDOW {
            self.gateway.pop_front();
        }
        self.gateway.push_back(rtt_ms);
    }

    pub fn tunnel_sample(&mut self, delay_ms: Option<u32>) {
        match delay_ms {
            Some(ms) => {
                self.tunnel_delay_ms = Some(ms);
                self.tunnel_failures = 0;
            }
            None => self.tunnel_failures += 1,
        }
    }

    pub fn reset(&mut self) {
        *self = Tracker::default();
    }

    pub fn quality(&self) -> Quality {
        let mut q = Quality {
            tunnel_delay_ms: self.tunnel_delay_ms,
            tunnel_failures: self.tunnel_failures,
            ..Default::default()
        };
        // A few samples are not enough to call a network unstable.
        if self.gateway.len() < 5 || !self.gateway_ever_answered {
            return q;
        }
        let answered: Vec<f64> = self.gateway.iter().flatten().copied().collect();
        q.gateway_loss = Some((self.gateway.len() - answered.len()) as f64 / self.gateway.len() as f64);
        if !answered.is_empty() {
            q.gateway_rtt_ms = Some(answered.iter().sum::<f64>() / answered.len() as f64);
        }
        if answered.len() >= 2 {
            let jumps: f64 = answered.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
            q.gateway_jitter_ms = Some(jumps / (answered.len() - 1) as f64);
        }
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_wifi() {
        let mut t = Tracker::default();
        for _ in 0..15 {
            t.gateway_sample(Some(3.0));
        }
        let q = t.quality();
        assert_eq!(q.gateway_loss, Some(0.0));
        assert_eq!(q.gateway_rtt_ms, Some(3.0));
        assert_eq!(q.gateway_jitter_ms, Some(0.0));
    }

    #[test]
    fn lossy_jumpy_wifi() {
        let mut t = Tracker::default();
        for i in 0..20 {
            t.gateway_sample(if i % 4 == 0 { None } else if i % 2 == 0 { Some(80.0) } else { Some(5.0) });
        }
        let q = t.quality();
        assert!(q.gateway_loss.unwrap() >= 0.2, "{q:?}"); // 3 of the last 15 lost
        assert!(q.gateway_jitter_ms.unwrap() > 30.0, "{q:?}");
    }

    #[test]
    fn router_that_ignores_pings_is_not_unstable() {
        let mut t = Tracker::default();
        for _ in 0..15 {
            t.gateway_sample(None);
        }
        assert_eq!(t.quality().gateway_loss, None);
    }

    #[test]
    fn too_few_samples_say_nothing() {
        let mut t = Tracker::default();
        t.gateway_sample(None);
        t.gateway_sample(None);
        assert_eq!(t.quality().gateway_loss, None);
    }

    #[test]
    fn tunnel_failures_count_until_success() {
        let mut t = Tracker::default();
        t.tunnel_sample(None);
        t.tunnel_sample(None);
        assert_eq!(t.quality().tunnel_failures, 2);
        t.tunnel_sample(Some(40));
        assert_eq!(t.quality().tunnel_failures, 0);
        assert_eq!(t.quality().tunnel_delay_ms, Some(40));
    }
}
