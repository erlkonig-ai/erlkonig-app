//! A disposable presentation of passive host observations. No network work.
use serde::Serialize;
#[cfg(test)]
use serde::{Serializer, ser::SerializeStruct};
use triblespace_net::health::HealthSnapshot;

#[derive(Clone, PartialEq)]
pub struct Observation {
    pub host_epoch: u64,
    pub sampled_at: u64,
    pub connections: Vec<Connection>,
}

#[derive(Serialize, Clone, PartialEq)]
pub struct Connection {
    pub id: u64,
    pub peer: String,
    pub received_bytes: Option<u64>,
    pub sent_bytes: Option<u64>,
    pub receive_bytes_per_second: Option<f64>,
    pub send_bytes_per_second: Option<f64>,
    pub rtt_ms: Option<f64>,
}

// Synthetic reports retain their printable representation.
#[cfg(test)]
impl Serialize for Observation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let age_ms = self.age_ms();
        let mut value = serializer.serialize_struct("NetworkObservation", 4)?;
        value.serialize_field("host_epoch", &self.host_epoch)?;
        value.serialize_field("sampled_at", &self.sampled_at)?;
        value.serialize_field("age_ms", &age_ms)?;
        value.serialize_field("connections", &self.connections)?;
        value.end()
    }
}

impl Observation {
    pub fn age_ms(&self) -> u64 {
        triblespace_net::clock::mono_now().as_nanos().saturating_sub(self.sampled_at) / 1_000_000
    }

    pub(crate) fn observe(health: &HealthSnapshot, previous: Option<&Self>) -> Option<Self> {
        let host_epoch = health.started_at?.as_nanos();
        let sampled_at = health.observed_at?.as_nanos();
        let previous = previous.filter(|old| old.host_epoch == host_epoch && old.sampled_at <= sampled_at);
        let seconds = previous.map(|old| (sampled_at - old.sampled_at) as f64 / 1e9);
        let connections = health.connections.iter().map(|row| {
            let peer = hex::encode(row.peer);
            let before = previous.and_then(|old| old.connections.iter().find(|old| old.id == row.id && old.peer == peer));
            let received_bytes = row.traffic.map(|stats| stats.received_bytes);
            let sent_bytes = row.traffic.map(|stats| stats.sent_bytes);
            // Both counters must advance monotonically. A reset is a new baseline,
            // never a wrapped delta or a zero-rate claim.
            let rates = before.and_then(|old| {
                let received = received_bytes?.checked_sub(old.received_bytes?)?;
                let sent = sent_bytes?.checked_sub(old.sent_bytes?)?;
                let seconds = seconds?;
                if seconds > 0.0 {
                    Some((received as f64 / seconds, sent as f64 / seconds))
                } else if received == 0 && sent == 0 {
                    Some((old.receive_bytes_per_second?, old.send_bytes_per_second?))
                } else {
                    None
                }
            });
            Connection {
                id: row.id, peer, received_bytes, sent_bytes,
                receive_bytes_per_second: rates.map(|rate| rate.0),
                send_bytes_per_second: rates.map(|rate| rate.1),
                rtt_ms: row.traffic.and_then(|stats| stats.rtt).map(|rtt| rtt.as_secs_f64() * 1000.0),
            }
        }).collect();
        Some(Self { host_epoch, sampled_at, connections })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use triblespace_net::{clock::mono_now, health::ConnectionHealth, transport::TrafficStats};

    fn health() -> HealthSnapshot {
        let now = mono_now();
        HealthSnapshot {
            node: hex::encode(ed25519_dalek::SigningKey::from_bytes(&[41; 32]).verifying_key().as_bytes()).parse().unwrap(),
            started_at: Some(now), observed_at: Some(now),
            connections: vec![ConnectionHealth { id: 7, peer: [42; 32],
                traffic: Some(TrafficStats { received_bytes: 100, sent_bytes: 200, rtt: Some(Duration::from_millis(12)) }) }],
            store: Default::default(), collections: vec![], publication: Default::default(),
            blob_serving: Default::default(), peerings: vec![], grants: vec![], named_unselected: vec![],
        }
    }

    #[test]
    fn rates_follow_only_matching_advancing_observations() {
        let mut health = health();
        let first = Observation::observe(&health, None).unwrap();
        assert!(first.connections[0].receive_bytes_per_second.is_none());
        health.observed_at = health.observed_at.map(|at| at + Duration::from_secs(2));
        let traffic = health.connections[0].traffic.as_mut().unwrap();
        traffic.received_bytes += 1000; traffic.sent_bytes += 500;
        let next = Observation::observe(&health, Some(&first)).unwrap();
        assert_eq!(next.connections[0].receive_bytes_per_second, Some(500.0));
        assert_eq!(next.connections[0].send_bytes_per_second, Some(250.0));
        let repeated = Observation::observe(&health, Some(&next)).unwrap();
        assert_eq!(repeated.sampled_at, next.sampled_at);
        assert_eq!(repeated.connections[0].receive_bytes_per_second, Some(500.0));
        assert_eq!(repeated.connections[0].send_bytes_per_second, Some(250.0));
        health.connections[0].traffic.as_mut().unwrap().received_bytes += 1;
        let ambiguous = Observation::observe(&health, Some(&repeated)).unwrap();
        assert!(ambiguous.connections[0].receive_bytes_per_second.is_none());
        assert!(ambiguous.connections[0].send_bytes_per_second.is_none());
        health.observed_at = health.observed_at.map(|at| at + Duration::from_secs(1));
        health.connections[0].id += 1;
        assert!(Observation::observe(&health, Some(&next)).unwrap().connections[0].receive_bytes_per_second.is_none());
        health.connections[0].id -= 1;
        health.started_at = health.started_at.map(|at| at + Duration::from_nanos(1));
        assert!(Observation::observe(&health, Some(&next)).unwrap().connections[0].receive_bytes_per_second.is_none());
        health.started_at = Some(mono_now()); // Distinct host, no carried rate.
        health.connections[0].traffic = None;
        let absent = Observation::observe(&health, None).unwrap();
        assert!(absent.connections[0].rtt_ms.is_none());
        assert!(absent.connections[0].received_bytes.is_none());
    }

    #[test]
    fn counter_reset_and_closed_rows_do_not_become_traffic() {
        let mut health = health();
        let first = Observation::observe(&health, None).unwrap();
        health.observed_at = health.observed_at.map(|at| at + Duration::from_secs(1));
        health.connections[0].traffic.as_mut().unwrap().received_bytes = 0;
        let reset = Observation::observe(&health, Some(&first)).unwrap();
        assert!(reset.connections[0].receive_bytes_per_second.is_none());
        assert!(reset.connections[0].send_bytes_per_second.is_none());
        health.connections.clear();
        assert!(Observation::observe(&health, Some(&first)).unwrap().connections.is_empty());
        health.started_at = None;
        assert!(Observation::observe(&health, Some(&first)).is_none());
    }

    #[test]
    fn network_rate_and_age_are_available_from_a_pinned_typed_read() {
        let mut health = health();
        let before = Observation::observe(&health, None).unwrap();
        health.observed_at = health.observed_at.map(|at| at + Duration::from_secs(1));
        health.connections[0].traffic.as_mut().unwrap().received_bytes += 4096;
        let mut observation = Observation::observe(&health, Some(&before)).unwrap();
        // Counter fixture uses synthetic intervals; age uses the actual clock.
        observation.sampled_at = mono_now().as_nanos();
        let (command, _receiver) = std::sync::mpsc::channel();
        let replica = crate::Replica {
            command, activity_serial: std::sync::atomic::AtomicU64::new(9),
            status: std::sync::Arc::new(std::sync::Mutex::new(crate::Status {
                ready: true, running: true, replay_complete: true, activity_serial: 9,
                network_started: true, network: Some(std::sync::Arc::new(observation)),
                ..Default::default()
            })),
        };
        let view = replica.read_after(None).unwrap();
        let net = view.network.as_ref().unwrap();
        assert_eq!(net.connections[0].receive_bytes_per_second, Some(4096.0));
        assert_eq!(net.connections[0].rtt_ms, Some(12.0));
        assert_eq!(view.activity_serial, 9);
        assert!(net.age_ms() <= 5000);
        assert!(replica.read_after(Some(view.revision)).is_none());
        std::thread::sleep(Duration::from_millis(5100));
        assert!(net.age_ms() > 5000);
        assert!(replica.read_after(Some(view.revision)).is_none());
    }
}
