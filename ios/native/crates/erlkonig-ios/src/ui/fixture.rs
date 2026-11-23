//! Explicit synthetic UI modes. The caller selects these before opening any replica.
use crate::{
    ReplayProgress, Status, SyncProgress,
    collections::{PAGE_SIZE, Page, Row},
    network::{Connection, Observation},
    storage::{self, Counts},
};
use std::sync::Arc;
pub(super) struct Fixture {
    pub rows: Vec<Row>,
    pub reduced: bool,
    pub peer_observation: Option<Observation>,
    prefix: bool,
}
impl Fixture {
    pub fn new(args: &[String], status: &mut Status) -> Self {
        let has = |arg: &str| args.iter().any(|s| s == arg);
        let mode = args
            .iter()
            .find_map(|s| s.strip_prefix("--crown-fixture="))
            .unwrap_or("working");
        let many = has("--collections-fixture-many");
        let prefix = has("--collections-fixture-prefix");
        let collections = args.iter().any(|s| s.starts_with("--collections-fixture"));
        status.ready = true;
        status.replay_complete = !prefix;
        status.running = !prefix && !matches!(mode, "idle" | "error");
        status.selected = usize::from(mode != "idle");
        status.listening = collections && !prefix;
        if mode == "error" {
            status.error = Some("Synthetic fixture error".into());
        }
        let n = if !collections {
            0
        } else if many {
            70
        } else {
            3
        };
        let rows: Vec<_> = (1..=n)
            .map(|i| {
                let root = format!("{i:064x}");
                let selected = i == 1;
                let conflicted = i == 3;
                Row {
                    handle: crate::parse_handle(&root).expect("32 byte fixture root"),
                    root,
                    name: match i {
                        1 => "Atlas (synthetic)".into(),
                        2 => "Later grant (synthetic)".into(),
                        3 => "Conflicting choice (synthetic)".into(),
                        _ => format!("Collection {i} with a longer synthetic name"),
                    },
                    state: if selected {
                        "selected"
                    } else if conflicted {
                        "conflicted:visual"
                    } else if prefix && i == 2 {
                        "unknown"
                    } else {
                        "unset"
                    }
                    .into(),
                    selected,
                    conflicted,
                    readable: true,
                    counts: Some(Counts {
                        records: if i == 2 { 0 } else { 12 },
                        direct_present: if i == 2 {
                            0
                        } else if i == 3 {
                            12
                        } else {
                            9
                        },
                    }),
                    count_error: None,
                    counts_replay_complete: Some(!prefix),
                }
            })
            .collect();
        if collections && !prefix {
            let segments: Vec<_> = rows
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != 1)
                .map(|(i, r)| storage::Segment {
                    root: r.root.clone(),
                    name: r.name.clone(),
                    bytes: match i {
                        0 => 12_000_000_000,
                        2 => 8_000_000_000,
                        _ => 100_000_000,
                    },
                })
                .collect();
            status.composition = Some(Arc::new(storage::Composition {
                shared_bytes: 4_000_000_000,
                known_bytes: 4_000_000_000 + segments.iter().map(|s| s.bytes).sum::<u64>(),
                known_inventories: segments.len(),
                unknown_inventories: 1,
                error: None,
                segments,
            }));
        }
        if prefix {
            status.replay_progress = Some(ReplayProgress {
                prefix_bytes: if has("--progress-fixture-small") {
                    2
                } else {
                    40
                },
                observed_bytes: 100,
                activity_serial: 0,
                records_per_second: None,
            });
        }
        if has("--collections-fixture-network") {
            status.replay_progress = Some(ReplayProgress {
                prefix_bytes: 100,
                observed_bytes: 100,
                activity_serial: 0,
                records_per_second: None,
            });
            status.sync_progress = Some(SyncProgress {
                records: 12,
                direct_present: 9,
                unknown_roots: 0,
            });
        }
        let peer_observation = has("--sync-fixture").then(|| {
            Observation::visual(if has("--sync-fixture-empty") {
                Vec::new()
            } else {
                vec![
                    Connection {
                        peer: "2a".repeat(32),
                        receive_bytes_per_second: Some(1_250_000.),
                        send_bytes_per_second: Some(32_000.),
                        rtt_ms: Some(12.),
                    },
                    Connection {
                        peer: "3b".repeat(32),
                        receive_bytes_per_second: None,
                        send_bytes_per_second: None,
                        rtt_ms: None,
                    },
                ]
            })
        });
        Self {
            rows,
            reduced: mode == "reduced",
            peer_observation,
            prefix,
        }
    }
    pub fn page(&self, offset: usize) -> Page {
        let total = self.rows.len();
        let start = offset.min(total);
        let end = (start + PAGE_SIZE).min(total);
        Page {
            rows: self.rows[start..end].to_vec(),
            offset: start,
            total,
            has_more: end < total,
            ..Default::default()
        }
    }
    pub fn choose(&mut self, root: &str, expected: &str, enabled: bool) -> Option<String> {
        let Some(row) = self
            .rows
            .iter_mut()
            .find(|r| r.root == root && r.state == expected)
        else {
            return Some("Collection choice changed".into());
        };
        row.selected = enabled;
        row.state = if enabled { "selected" } else { "unselected" }.into();
        row.conflicted = false;
        None
    }
    pub fn storage(&self, roots: &[String]) -> Vec<storage::ByteObservation> {
        if self.prefix {
            return Vec::new();
        }
        roots
            .iter()
            .filter_map(|root| {
                self.rows
                    .iter()
                    .position(|r| r.root == *root)
                    .map(|i| storage::ByteObservation {
                        root: root.clone(),
                        resident_bytes: match i {
                            0 => Some(16_000_000_000),
                            1 => None,
                            2 => Some(12_000_000_000),
                            _ => Some(100_000_000),
                        },
                        resident_error: None,
                        pending: false,
                    })
            })
            .collect()
    }
}
