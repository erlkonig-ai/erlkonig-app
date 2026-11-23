//! Presentation of a peer's immutable host observations. Reading never drives networking.
use std::sync::Arc;
use triblespace_net::{
    clock::{Mono, mono_now},
    health::{HealthReader, HealthSnapshot},
};

/// The worker publishes this binding once when it acquires a peer.
pub(crate) struct Session {
    pub serial: u64,
    pub reader: HealthReader,
}

pub(crate) struct Observation {
    pub(crate) sampled_at: Mono,
    current: Arc<HealthSnapshot>,
}

pub(crate) struct Connection {
    pub peer: String,
    pub connected: bool,
    pub receive_bytes_per_second: Option<f64>,
    pub send_bytes_per_second: Option<f64>,
    pub rtt_ms: Option<f64>,
}

impl Observation {
    pub fn age_ms(&self) -> u64 {
        mono_now().duration_since(self.sampled_at).as_millis() as u64
    }

    pub fn connected_count(&self) -> usize {
        self.current.connections.len()
    }

    pub fn len(&self) -> usize {
        self.current.traffic_rates.len()
    }

    pub fn row(&self, index: usize) -> Option<Connection> {
        let rate = self.current.traffic_rates.iter().nth(index)?;
        let connection = self.current.connections.get(&rate.peer);
        Some(Connection {
            peer: hex::encode(rate.peer),
            connected: connection.is_some(),
            receive_bytes_per_second: rate.receive_bytes_per_second(),
            send_bytes_per_second: rate.send_bytes_per_second(),
            rtt_ms: connection
                .and_then(|c| c.traffic)
                .and_then(|t| t.rtt)
                .map(|rtt| rtt.as_secs_f64() * 1000.),
        })
    }

    /// Keep one native cut. A newer heartbeat can update freshness without
    /// invalidating the table. Formatting happens only for requested rows.
    pub(crate) fn update(view: &mut Option<Self>, snapshot: Option<Arc<HealthSnapshot>>) -> bool {
        let Some((current, sampled_at)) = snapshot.and_then(|s| {
            let at = s.started_at.and(s.observed_at)?;
            Some((s, at))
        }) else {
            return view.take().is_some();
        };
        let mut changed = true;
        if let Some(old) = view.as_ref() {
            if Arc::ptr_eq(&old.current, &current) {
                return false;
            }
            if old.current.started_at == current.started_at {
                if sampled_at < old.sampled_at {
                    return false;
                }
                changed = !old.current.traffic_rates.same_root(&current.traffic_rates)
                    || !old.current.connections.same_root(&current.connections);
            }
        }
        *view = Some(Self {
            current,
            sampled_at,
        });
        changed
    }
}

#[cfg(any(test, feature = "synthetic-proof"))]
pub(crate) fn synthetic_health() -> HealthSnapshot {
    let now = mono_now();
    HealthSnapshot {
        node: hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[41; 32])
                .verifying_key()
                .as_bytes(),
        )
        .parse()
        .unwrap(),
        started_at: Some(now),
        observed_at: Some(now),
        connections: Default::default(),
        traffic_rates: Default::default(),
        store: Default::default(),
        collections: Default::default(),
        publication: Default::default(),
        blob_serving: Default::default(),
        peerings: Default::default(),
        grants: Default::default(),
        named_unselected: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use triblespace_net::{
        health::{ConnectionHealth, PeerRate},
        transport::TrafficStats,
    };

    fn rate(peer: u8, received_bytes: Option<u128>, sent_bytes: Option<u128>) -> PeerRate {
        PeerRate {
            peer: [peer; 32],
            received_bytes,
            sent_bytes,
            window_ns: 10_000_000_000,
        }
    }
    fn socket(id: u64) -> ConnectionHealth {
        ConnectionHealth {
            peer: [42; 32],
            id,
            traffic: Some(TrafficStats {
                received_bytes: id * 1000,
                sent_bytes: id * 1000,
                rtt: Some(Duration::from_millis(12)),
            }),
        }
    }

    #[test]
    fn first_screen_read_shows_the_full_native_window_in_peer_order() {
        let mut h = synthetic_health();
        h.connections = [socket(7)].into_iter().collect();
        h.traffic_rates = [rate(42, Some(50_000), Some(0)), rate(1, None, Some(500))]
            .into_iter()
            .collect();
        let mut view = None;
        assert!(Observation::update(&mut view, Some(Arc::new(h))));
        let view = view.unwrap();
        assert_eq!(view.connected_count(), 1);
        assert_eq!(view.len(), 2);
        let recent = view.row(0).unwrap();
        assert_eq!(recent.peer, "01".repeat(32));
        assert!(!recent.connected);
        assert_eq!(recent.rtt_ms, None);
        assert_eq!(recent.receive_bytes_per_second, None);
        assert_eq!(recent.send_bytes_per_second, Some(50.));
        let active = view.row(1).unwrap();
        assert!(active.connected);
        assert_eq!(active.receive_bytes_per_second, Some(5000.));
        assert_eq!(active.send_bytes_per_second, Some(0.));
        assert_eq!(active.rtt_ms, Some(12.));
    }

    #[test]
    fn disconnect_and_reconnect_read_the_published_peer_window_without_a_local_baseline() {
        let mut h = synthetic_health();
        h.connections = [socket(1)].into_iter().collect();
        h.traffic_rates = [rate(42, Some(40_000), Some(100))].into_iter().collect();
        let pinned = Arc::new(h.clone());
        let mut view = None;
        Observation::update(&mut view, Some(pinned.clone()));
        h.connections = Default::default();
        h.observed_at = h.observed_at.map(|at| at + Duration::from_secs(2));
        assert!(Observation::update(&mut view, Some(Arc::new(h.clone()))));
        let recent = view.as_ref().unwrap();
        assert_eq!(recent.connected_count(), 0);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent.row(0).unwrap().receive_bytes_per_second, Some(4000.));
        assert_eq!(recent.row(0).unwrap().rtt_ms, None);
        h.observed_at = h.observed_at.map(|at| at + Duration::from_secs(1));
        h.connections = [socket(99)].into_iter().collect();
        h.traffic_rates = [rate(42, Some(30_000), None)].into_iter().collect();
        let new = Arc::new(h);
        assert!(Observation::update(&mut view, Some(new.clone())));
        assert_eq!(view.as_ref().unwrap().connected_count(), 1);
        let row = view.as_ref().unwrap().row(0).unwrap();
        assert_eq!(row.receive_bytes_per_second, Some(3000.));
        assert_eq!(row.send_bytes_per_second, None);
        assert_eq!(row.rtt_ms, Some(12.));
        assert!(!Observation::update(&mut view, Some(new)));
        assert_eq!(pinned.connections.get(&[42; 32]).unwrap().id, 1);
        assert_eq!(
            pinned
                .traffic_rates
                .get(&[42; 32])
                .unwrap()
                .receive_bytes_per_second(),
            Some(4000.)
        );
    }

    #[test]
    fn only_displayed_roots_invalidate_the_table_and_only_host_time_advances_freshness() {
        let mut h = synthetic_health();
        let mut view = None;
        h.traffic_rates = [rate(42, Some(100), Some(0))].into_iter().collect();
        assert!(Observation::update(&mut view, Some(Arc::new(h.clone()))));
        h.store.serving_snapshot = true;
        h.observed_at = h.observed_at.map(|at| at + Duration::from_secs(1));
        assert!(!Observation::update(&mut view, Some(Arc::new(h.clone()))));
        let at = view.as_ref().unwrap().sampled_at;
        h.observed_at = h.started_at;
        assert!(!Observation::update(&mut view, Some(Arc::new(h.clone()))));
        assert_eq!(view.as_ref().unwrap().sampled_at, at);
        h.observed_at = Some(at + Duration::from_secs(10));
        h.traffic_rates = Default::default();
        assert!(Observation::update(&mut view, Some(Arc::new(h.clone()))));
        assert_eq!(view.as_ref().unwrap().len(), 0);
        h.observed_at = None;
        assert!(Observation::update(&mut view, Some(Arc::new(h))));
        assert!(view.is_none());
    }
}
