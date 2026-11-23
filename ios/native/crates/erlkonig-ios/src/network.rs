//! Presentation of a peer's immutable host observations. Reading never drives networking.
use std::sync::Arc;
use triblespace_net::{
    clock::{Mono, mono_now},
    health::{HealthReader, HealthSnapshot},
};

/// The worker publishes this binding once when it acquires a peer. It carries
/// no transport, store or authority to write health.
pub(crate) struct Session {
    pub serial: u64,
    pub reader: HealthReader,
}

pub(crate) struct Observation {
    pub(crate) sampled_at: Mono,
    rows: Rows,
}

enum Rows {
    Native {
        current: Arc<HealthSnapshot>,
        baseline: Option<Arc<HealthSnapshot>>,
    },
    #[cfg(feature = "synthetic-proof")]
    Visual(Vec<Connection>),
}

#[derive(Clone)]
pub(crate) struct Connection {
    pub peer: String,
    pub receive_bytes_per_second: Option<f64>,
    pub send_bytes_per_second: Option<f64>,
    pub rtt_ms: Option<f64>,
}

impl Observation {
    pub fn age_ms(&self) -> u64 {
        mono_now().duration_since(self.sampled_at).as_millis() as u64
    }

    pub fn len(&self) -> usize {
        match &self.rows {
            Rows::Native { current, .. } => current.connections.len(),
            #[cfg(feature = "synthetic-proof")]
            Rows::Visual(rows) => rows.len(),
        }
    }

    pub fn row(&self, index: usize) -> Option<Connection> {
        match &self.rows {
            Rows::Native { current, baseline } => {
                let row = current.connections.iter().nth(index)?;
                let rates = baseline.as_deref().and_then(|old| {
                    let before = old.connections.get(&row.peer)?;
                    if before.id != row.id {
                        return None;
                    }
                    let (before, after) = (before.traffic?, row.traffic?);
                    let seconds = self
                        .sampled_at
                        .duration_since(old.observed_at?)
                        .as_secs_f64();
                    if seconds == 0.0 {
                        return None;
                    }
                    let rx = after.received_bytes.checked_sub(before.received_bytes)?;
                    let tx = after.sent_bytes.checked_sub(before.sent_bytes)?;
                    Some((rx as f64 / seconds, tx as f64 / seconds))
                });
                Some(Connection {
                    peer: hex::encode(row.peer),
                    receive_bytes_per_second: rates.map(|v| v.0),
                    send_bytes_per_second: rates.map(|v| v.1),
                    rtt_ms: row
                        .traffic
                        .and_then(|t| t.rtt)
                        .map(|v| v.as_secs_f64() * 1000.),
                })
            }
            #[cfg(feature = "synthetic-proof")]
            Rows::Visual(rows) => rows.get(index).cloned(),
        }
    }

    /// Keep only two observed cuts. Calculating a rate or formatting a peer ID
    /// is deferred until UIKit asks for that row. The return value invalidates
    /// table content; age/freshness is tracked separately by its rendering key.
    pub(crate) fn update(view: &mut Option<Self>, snapshot: Option<Arc<HealthSnapshot>>) -> bool {
        let Some((current, stamp)) = snapshot.and_then(|s| {
            let stamp = s.started_at.and(s.observed_at)?;
            Some((s, stamp))
        }) else {
            return view.take().is_some();
        };
        let mut changed = true;
        let mut baseline = None;
        if let Some(Self {
            sampled_at,
            rows:
                Rows::Native {
                    current: old,
                    baseline: previous,
                },
        }) = view.as_ref()
        {
            if Arc::ptr_eq(old, &current) {
                return false;
            }
            if old.started_at == current.started_at {
                let same_rows = old.connections.same_root(&current.connections);
                if stamp < *sampled_at || (same_rows && *sampled_at == stamp) {
                    return false;
                }
                if stamp > *sampled_at {
                    // The first equal counter pair can establish zero rates. Once
                    // both ends already have the same rows, even that change is done.
                    changed = !same_rows
                        || (!old.connections.is_empty()
                            && !previous
                                .as_ref()
                                .is_some_and(|p| p.connections.same_root(&old.connections)));
                    baseline = Some(old.clone());
                }
            }
        }
        *view = Some(Self {
            sampled_at: stamp,
            rows: Rows::Native { current, baseline },
        });
        changed
    }

    #[cfg(feature = "synthetic-proof")]
    pub(crate) fn visual(rows: Vec<Connection>) -> Self {
        Self {
            sampled_at: mono_now(),
            rows: Rows::Visual(rows),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use triblespace_net::{health::ConnectionHealth, transport::TrafficStats};

    fn health() -> HealthSnapshot {
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
            store: Default::default(),
            collections: Default::default(),
            publication: Default::default(),
            blob_serving: Default::default(),
            peerings: Default::default(),
            grants: Default::default(),
            named_unselected: Default::default(),
        }
    }
    fn row(id: u64, rx: u64, tx: u64) -> ConnectionHealth {
        ConnectionHealth {
            id,
            peer: [42; 32],
            traffic: Some(TrafficStats {
                received_bytes: rx,
                sent_bytes: tx,
                rtt: Some(Duration::from_millis(12)),
            }),
        }
    }
    #[test]
    fn rates_follow_only_matching_advancing_observations() {
        let mut h = health();
        let mut frame = None;
        let first = |frame: &Option<Observation>| frame.as_ref().unwrap().row(0).unwrap();
        h.connections = [row(7, 100, 200)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(h.clone())));
        assert!(first(&frame).receive_bytes_per_second.is_none());
        h.observed_at = h.observed_at.map(|t| t + Duration::from_secs(2));
        h.connections = [row(7, 1100, 700)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(h.clone())));
        assert_eq!(first(&frame).receive_bytes_per_second, Some(500.));
        assert_eq!(first(&frame).send_bytes_per_second, Some(250.));
        assert!(!Observation::update(&mut frame, Some(Arc::new(h.clone()))));
        assert_eq!(first(&frame).receive_bytes_per_second, Some(500.));
        h.connections = [row(7, 1101, 700)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(h.clone())));
        assert!(first(&frame).receive_bytes_per_second.is_none());
        h.observed_at = h.observed_at.map(|t| t + Duration::from_secs(1));
        h.connections = [row(8, 1200, 710)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(h.clone())));
        assert!(first(&frame).receive_bytes_per_second.is_none());
        h.started_at = h.started_at.map(|t| t + Duration::from_nanos(1));
        h.observed_at = h.observed_at.map(|t| t + Duration::from_secs(1));
        Observation::update(&mut frame, Some(Arc::new(h)));
        assert!(first(&frame).receive_bytes_per_second.is_none());
    }

    #[test]
    fn reconnect_keeps_one_row_for_the_peer_without_carrying_its_rate() {
        let mut health = health();
        let mut frame = None;
        let stable = |rx, tx| ConnectionHealth {
            peer: [1; 32],
            ..row(90, rx, tx)
        };
        health.connections = [row(7, 20, 25), stable(100, 80)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(health.clone())));
        health.observed_at = health.observed_at.map(|at| at + Duration::from_secs(1));
        // The new connection already has larger counters than its predecessor.
        health.connections = [row(8, 400, 300), stable(150, 90)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(health.clone())));
        let view = frame.as_ref().unwrap();
        assert_eq!(view.len(), 2);
        let other = view.row(0).unwrap();
        assert_eq!(other.peer, "01".repeat(32)); // Peer order, independent of local ids.
        assert_eq!(other.receive_bytes_per_second, Some(50.));
        assert_eq!(other.send_bytes_per_second, Some(10.));
        let reconnected = view.row(1).unwrap();
        assert_eq!(reconnected.peer, "2a".repeat(32));
        assert!(reconnected.receive_bytes_per_second.is_none());
        assert!(reconnected.send_bytes_per_second.is_none());
        assert_eq!(reconnected.rtt_ms, Some(12.));
        health.observed_at = health.observed_at.map(|at| at + Duration::from_secs(2));
        health.connections = [row(8, 500, 330), stable(250, 110)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(health)));
        let reconnected = frame.as_ref().unwrap().row(1).unwrap();
        assert_eq!(reconnected.receive_bytes_per_second, Some(50.));
        assert_eq!(reconnected.send_bytes_per_second, Some(15.));
    }

    #[test]
    fn counter_reset_and_closed_rows_do_not_become_traffic() {
        let mut h = health();
        let mut frame = None;
        h.connections = [row(7, 100, 200)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(h.clone())));
        h.observed_at = h.observed_at.map(|t| t + Duration::from_secs(1));
        h.connections = [row(7, 0, 210)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(h.clone())));
        let reset = frame.as_ref().unwrap().row(0).unwrap();
        assert!(reset.receive_bytes_per_second.is_none() && reset.send_bytes_per_second.is_none());
        h.connections = Default::default();
        Observation::update(&mut frame, Some(Arc::new(h.clone())));
        assert_eq!(frame.as_ref().unwrap().len(), 0);
        h.started_at = None;
        Observation::update(&mut frame, Some(Arc::new(h)));
        assert!(frame.is_none());
    }
    #[test]
    fn new_evidence_outside_the_connection_rows_keeps_identical_rendering() {
        let mut h = health();
        h.connections = [row(1, 12, 34)].into_iter().collect();
        let mut view = None;
        assert!(Observation::update(&mut view, Some(Arc::new(h.clone()))));
        h.observed_at = h.observed_at.map(|t| t + Duration::from_secs(1));
        assert!(Observation::update(&mut view, Some(Arc::new(h.clone())))); // Missing → measured zero.
        assert_eq!(
            view.as_ref()
                .unwrap()
                .row(0)
                .unwrap()
                .receive_bytes_per_second,
            Some(0.)
        );
        h.store.serving_snapshot = true;
        assert!(!Observation::update(&mut view, Some(Arc::new(h.clone()))));
        h.observed_at = h.observed_at.map(|t| t + Duration::from_secs(1));
        assert!(!Observation::update(&mut view, Some(Arc::new(h.clone()))));
        let latest = view.as_ref().unwrap().sampled_at;
        h.observed_at = h.started_at;
        assert!(!Observation::update(&mut view, Some(Arc::new(h))));
        assert_eq!(view.unwrap().sampled_at, latest);
    }

    #[test]
    fn a_pinned_rate_and_rtt_outlive_replacement_without_refreshing_the_clock() {
        let mut h = health();
        let mut frame = None;
        h.connections = [row(7, 100, 200)].into_iter().collect();
        Observation::update(&mut frame, Some(Arc::new(h.clone())));
        h.observed_at = h.observed_at.map(|t| t + Duration::from_secs(1));
        h.connections = [row(7, 4196, 200)].into_iter().collect();
        let pinned = Arc::new(h.clone());
        Observation::update(&mut frame, Some(pinned.clone()));
        let net = frame.unwrap();
        let at = net.sampled_at;
        h.connections = Default::default();
        assert_eq!(pinned.connections.len(), 1);
        assert_eq!(net.row(0).unwrap().receive_bytes_per_second, Some(4096.));
        assert_eq!(net.row(0).unwrap().rtt_ms, Some(12.));
        assert_eq!(net.sampled_at, at);
    }
}
