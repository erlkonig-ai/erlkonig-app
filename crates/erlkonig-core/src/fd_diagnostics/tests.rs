use super::*;

fn snapshot(count: u64, time: u128) -> Snapshot {
    Snapshot {
        monotonic_ns: Some(time),
        census: Some(Census {
            count,
            complete: true,
            ..Census::default()
        }),
        ..Snapshot::default()
    }
}

#[test]
fn census_distinguishes_unknown_unavailable_and_capacity() {
    let entries = [
        FdInfo { fd: 0, kind: 1 },
        FdInfo { fd: 7, kind: 2 },
        FdInfo { fd: 17, kind: 6 },
        FdInfo { fd: 42, kind: 99 },
        FdInfo::default(),
    ];
    let census = census_from_result(&entries, 32).unwrap();
    assert_eq!(census.count, 4);
    assert_eq!(census.types[1], 1);
    assert_eq!(census.types[2], 1);
    assert_eq!(census.types[6], 1);
    assert_eq!(census.types[11], 1);
    assert!(census.complete);
    assert!(!census_from_result(&entries, 40).unwrap().complete);
    for bytes in [-1, 0, 3, 48] {
        assert_eq!(census_from_result(&entries, bytes), None);
    }
}

#[test]
fn full_and_contended_capture_drops_without_probing_or_blocking() {
    let diagnostics = Diagnostics::new();
    let session = genid().id;
    {
        let _guard = diagnostics.queue.lock().unwrap();
        diagnostics.capture_with(session, Exhaustion::Process, || {
            panic!("contended capture probed")
        });
    }
    for _ in 0..CAPACITY {
        diagnostics.capture_with(session, Exhaustion::Process, Snapshot::default);
    }
    diagnostics.capture_with(session, Exhaustion::Process, || {
        panic!("full capture probed")
    });
    diagnostics
        .publish_pending(|batch| {
            assert_eq!(batch.events.iter().flatten().count(), CAPACITY);
            assert_eq!(batch.dropped, 2);
            Ok::<_, ()>(())
        })
        .unwrap();
    diagnostics
        .publish_pending::<()>(|_| panic!("acknowledged batch republished"))
        .unwrap();
}

#[test]
fn failure_keeps_occurrence_and_does_not_generate_recursive_events() {
    let diagnostics = Diagnostics::new();
    diagnostics.capture_with(genid().id, Exhaustion::Process, || snapshot(41, 100));
    let mut first = None;
    let result = diagnostics.publish_pending(|batch| {
        first = batch.events[0];
        Err("synthetic metrics commit/flush failed: Too many open files (os error 24)")
    });
    assert!(result.is_err());
    diagnostics
        .publish_pending(|batch| {
            assert_eq!(batch.events[0], first);
            assert_eq!(batch.events.iter().flatten().count(), 1);
            assert_eq!(batch.dropped, 0);
            // The writer holds no capture mutex, even during a failed publication.
            assert!(diagnostics.queue.try_lock().is_ok());
            Ok::<_, ()>(())
        })
        .unwrap();
    diagnostics
        .publish_pending::<()>(|_| panic!("duplicate after successful ack"))
        .unwrap();
}

#[test]
fn captures_and_drops_during_publication_survive_prefix_acknowledgement() {
    let diagnostics = Diagnostics::new();
    let session = genid().id;
    diagnostics.capture_with(session, Exhaustion::Process, Snapshot::default);
    diagnostics
        .publish_pending(|batch| {
            assert_eq!(batch.events.iter().flatten().count(), 1);
            for _ in 1..CAPACITY {
                diagnostics.capture_with(session, Exhaustion::System, Snapshot::default);
            }
            diagnostics.capture_with(session, Exhaustion::System, || panic!("full"));
            Ok::<_, ()>(())
        })
        .unwrap();
    diagnostics
        .publish_pending(|batch| {
            assert_eq!(batch.events.iter().flatten().count(), CAPACITY - 1);
            assert_eq!(batch.events[0].unwrap().sequence, 2);
            assert_eq!(batch.dropped, 1);
            Ok::<_, ()>(())
        })
        .unwrap();
    diagnostics
        .publish_pending::<()>(|_| panic!("acknowledged loss republished"))
        .unwrap();
}

#[test]
fn contended_success_acknowledgement_retries_same_batch_without_loss() {
    let diagnostics = Diagnostics::new();
    diagnostics.capture_with(genid().id, Exhaustion::Process, Snapshot::default);
    let mut guard = None;
    let mut first = None;
    diagnostics
        .publish_pending(|batch| {
            first = batch.events[0];
            guard = Some(diagnostics.queue.lock().unwrap());
            Ok::<_, ()>(())
        })
        .unwrap();
    drop(guard);
    diagnostics
        .publish_pending(|batch| {
            assert_eq!(batch.events[0], first);
            Ok::<_, ()>(())
        })
        .unwrap();
    diagnostics
        .publish_pending::<()>(|_| panic!("already acknowledged"))
        .unwrap();
}

#[test]
fn sampler_is_sparse_high_water_not_handler_or_syscall_peak() {
    let diagnostics = Diagnostics::new();
    diagnostics.sample_with(snapshot(5, 10));
    diagnostics.sample_with(Snapshot::default()); // unavailable is not zero
    diagnostics.sample_with(snapshot(9, 30));
    diagnostics.sample_with(snapshot(4, 1_000_000)); // arbitrary cadence gap
    diagnostics.capture_with(genid().id, Exhaustion::System, || snapshot(40, 2_000_000));
    diagnostics
        .publish_pending(|batch| {
            let event = batch.events[0].unwrap();
            assert_eq!(event.samples, 3);
            assert_eq!(event.sampled_high_water, Some(snapshot(9, 30)));
            assert_eq!(event.handler, snapshot(40, 2_000_000));
            Ok::<_, ()>(())
        })
        .unwrap();
}

#[test]
fn stale_partial_future_and_unknown_clock_samples_do_not_claim_recent_peak() {
    let diagnostics = Diagnostics::new();
    diagnostics.sample_with(snapshot(100, 1));
    diagnostics.sample_with(snapshot(99, 70_000_000_001));
    let mut partial = snapshot(8192, 69_000_000_000);
    partial.census.as_mut().unwrap().complete = false;
    diagnostics.sample_with(partial);
    let mut unknown_clock = snapshot(200, 0);
    unknown_clock.monotonic_ns = None;
    diagnostics.sample_with(unknown_clock);
    diagnostics.capture_with(genid().id, Exhaustion::Process, || {
        snapshot(3, 70_000_000_000)
    });
    diagnostics
        .publish_pending(|batch| {
            let event = batch.events[0].unwrap();
            assert_eq!(event.sampled_high_water, None);
            assert_eq!(event.samples, 0);
            Ok::<_, ()>(())
        })
        .unwrap();
}

#[test]
fn sample_storage_is_bounded_and_poison_does_not_hide_evidence() {
    let diagnostics = Diagnostics::new();
    diagnostics.sample_with(snapshot(999, 1));
    for _ in 0..SAMPLE_CAPACITY {
        diagnostics.sample_with(snapshot(2, 2));
    }
    let _ = std::panic::catch_unwind(|| {
        let _queue = diagnostics.queue.lock().unwrap();
        panic!("synthetic reader unwound while holding queue");
    });
    diagnostics.capture_with(genid().id, Exhaustion::Process, || snapshot(3, 3));
    diagnostics
        .publish_pending(|batch| {
            let event = batch.events[0].unwrap();
            assert_eq!(event.samples, SAMPLE_CAPACITY as u64);
            assert_eq!(event.sampled_high_water, Some(snapshot(2, 2)));
            Ok::<_, ()>(())
        })
        .unwrap();
}

#[test]
fn classification_handles_wrapped_errors_without_retaining_text() {
    let wrapped = std::io::Error::other(std::io::Error::from_raw_os_error(libc::EMFILE));
    assert_eq!(wrapped.raw_os_error(), None);
    assert_eq!(exhaustion(&wrapped.to_string()), Some(Exhaustion::Process));
    assert_eq!(
        exhaustion("maintain: Too many open files in system (os error 23)"),
        Some(Exhaustion::System)
    );
    assert_eq!(exhaustion("failed (os error 240)"), None);
    assert_eq!(exhaustion("permission denied"), None);
    let long = format!("{}: Too many open files (os error 24)", "x".repeat(10000));
    assert_eq!(exhaustion(&long), Some(Exhaustion::Process));
}

#[test]
fn handler_attribution_is_not_inferred_from_error_text_or_socket_counts() {
    let diagnostics = Diagnostics::new();
    diagnostics.maintenance_failure(genid().id,
        "http secret-endpoint-canary: Too many open files (os error 24)");
    diagnostics.publish_pending(|batch| {
        let event = batch.events[0].unwrap();
        assert_eq!(event.error_site, ErrorSite::Maintenance);
        assert_eq!(event.handler.mcp_http_accepted_streams, None);
        assert!(!format!("{event:?}").contains("secret-endpoint-canary"));
        Ok::<_, ()>(())
    }).unwrap();
}

#[test]
fn instrumented_endpoint_zero_is_distinct_from_unknown_and_survives_endpoint_drop() {
    use faculties::mcp::http::{BearerToken, Config, Endpoint};
    let endpoint = Endpoint::bind(Config::new("127.0.0.1:0".parse().unwrap(),
        BearerToken::new("test-only-token-0123456789abcdef0123456789").unwrap())).unwrap();
    let diagnostics = Diagnostics::new().with_http(endpoint.connection_metrics());
    drop(endpoint);
    assert_eq!(diagnostics.snapshot().mcp_http_accepted_streams, Some(0));
    diagnostics.maintenance_failure(genid().id, "Too many open files (os error 24)");
    diagnostics.publish_pending(|batch| {
        assert_eq!(batch.events[0].unwrap().handler.mcp_http_accepted_streams, Some(0));
        Ok::<_, ()>(())
    }).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn own_process_reports_limits_and_known_descriptor_types() {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open("/dev/null").unwrap();
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let captured = Snapshot::capture();
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    assert!(file.as_raw_fd() >= 0 && a.as_raw_fd() >= 0);
    let census = captured.census.unwrap();
    assert!(census.complete);
    assert!(census.types[1] >= 1 && census.types[2] >= 2 && census.types[6] >= 2);
    assert_eq!(census.types.iter().sum::<u64>(), census.count);
    assert!(captured.wall_unix_ns.is_some() && captured.monotonic_ns.is_some());
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) },
        0
    );
    assert_eq!(captured.limits, Some((limits.rlim_cur, limits.rlim_max)));
}

#[cfg(target_os = "macos")]
#[test]
fn child_only_low_rlimit_capture_survives_exhaustion() {
    const CHILD: &str = "ERLKONIG_FD_DIAGNOSTIC_CHILD";
    if std::env::var_os(CHILD).is_some() {
        // This branch runs only in a new throwaway test process. No stores,
        // identities, application launch, or parent limit changes are involved.
        let diagnostics = Diagnostics::new();
        let session = genid().id;
        let mut limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) },
            0
        );
        let original_limits = limits;
        limits.rlim_cur = limits.rlim_max.min(64);
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) }, 0);
        let mut fds = [-1; 128];
        let mut used = 0;
        loop {
            let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
            if fd < 0 {
                break;
            }
            assert!(used < fds.len());
            fds[used] = fd;
            used += 1;
        }
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EMFILE)
        );
        diagnostics.maintenance_failure(
            session,
            "synthetic maintenance: Too many open files (os error 24)",
        );
        let captured = diagnostics.queue.lock().unwrap().slots[0].unwrap();
        for fd in &fds[..used] {
            unsafe {
                libc::close(*fd);
            }
        }
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original_limits) },
            0
        );
        assert_eq!(
            captured.handler.limits,
            Some((limits.rlim_cur, limits.rlim_max))
        );
        assert_eq!(captured.handler.census.unwrap().count, limits.rlim_cur);
        assert!(captured.handler.census.unwrap().complete);
        // Recovery publishes that same real low-limit snapshot to a throwaway
        // node, not the test environment's (cleared) PILE or signing identity.
        use crate::{node_metrics::fd, node_store::NodeStore};
        use triblespace::core::collection::CollectionSnapshotExt;
        use triblespace::core::repo::SnapshotSource;
        use triblespace::prelude::*;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.pile");
        std::fs::File::create(&path).unwrap();
        let mut node =
            NodeStore::open(&path, ed25519_dalek::SigningKey::from_bytes(&[93; 32])).unwrap();
        diagnostics
            .publish_pending(|batch| node.append_fd_events(batch))
            .unwrap();
        let snapshot = node.pile.snapshot().unwrap();
        let observed = snapshot.collection_acquiring(node.metrics).unwrap();
        let facts: TribleSet = observed.view().unwrap();
        assert!(exists!(pattern!(&facts, [
            { triblespace::core::metadata::tag: fd::KIND_FD_EXHAUSTION,
                fd::handler_snapshot: _?snapshot },
            { _?snapshot @ fd::soft_limit: limits.rlim_cur as u128,
                fd::fd_count: limits.rlim_cur as u128,
                fd::monotonic_ns: captured.handler.monotonic_ns.unwrap() },
        ])));
        drop(observed);
        drop(snapshot);
        node.close().unwrap();
        println!("child captured EMFILE with actual low limit and full census");
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .env(CHILD, "1")
        .args([
            "--exact",
            "fd_diagnostics::tests::child_only_low_rlimit_capture_survives_exhaustion",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child exit {:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("child captured EMFILE"));
}
