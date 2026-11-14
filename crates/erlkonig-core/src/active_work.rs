//! Ephemeral work owned by this app, not telemetry or network throughput.

use anyhow::Result;
use faculties::mcp::{Bytes, Faculty, Tool};
use faculties::out::Out;
use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Default)]
pub(crate) struct ActiveWork(Arc<AtomicUsize>);

impl ActiveWork {
    pub(crate) fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn enter(&self) -> WorkGuard {
        self.0.fetch_add(1, Ordering::Relaxed);
        WorkGuard(self.clone())
    }

    /// Enter only when polled; completion, error and cancellation all drop the
    /// same owned scope. Never surround a service loop or its idle sleep.
    pub(crate) async fn during<F: Future>(&self, future: F) -> F::Output {
        let _work = self.enter();
        future.await
    }

    pub(crate) fn faculty<'a>(&'a self, inner: &'a dyn Faculty) -> WorkingFaculty<'a> {
        WorkingFaculty {
            inner,
            activity: self,
        }
    }
}

pub(crate) struct WorkGuard(ActiveWork);

impl Drop for WorkGuard {
    fn drop(&mut self) {
        // Each guard removes only its own contribution. An old worker can
        // never clear another worker's activity, including during replacement.
        self.0.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) struct WorkingFaculty<'a> {
    inner: &'a dyn Faculty,
    activity: &'a ActiveWork,
}

impl Faculty for WorkingFaculty<'_> {
    fn tools(&self) -> &[Tool] {
        self.inner.tools()
    }

    fn input_schema<'a>(&'a self, tool: &'a Tool) -> &'a str {
        self.inner.input_schema(tool)
    }

    fn call(&self, name: &str, arguments: Bytes, out: &mut Out<'_>) -> Result<()> {
        let _work = self.activity.enter();
        self.inner.call(name, arguments, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    #[test]
    fn blocked_pass_is_active_before_any_result_and_cancellation_clears_it() {
        let work = ActiveWork::default();
        let mut pass = Box::pin(work.during(std::future::pending::<()>()));
        assert_eq!(work.count(), 0, "unpolled work is not active");
        let mut context = Context::from_waker(Waker::noop());
        for _ in 0..3 {
            assert!(pass.as_mut().poll(&mut context).is_pending());
            assert_eq!(work.count(), 1, "waiting inside a pass is work");
        }
        drop(pass);
        assert_eq!(work.count(), 0);
    }

    #[test]
    fn success_error_and_unwind_release_only_their_owned_scope() {
        let work = ActiveWork::default();
        for result in [Ok(()), Err("synthetic pass failure")] {
            let mut pass = Box::pin(work.during(async {
                assert_eq!(work.count(), 1);
                result
            }));
            assert_eq!(
                pass.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(result)
            );
            assert_eq!(work.count(), 0);
        }
        let old = work.enter();
        let newer = work.enter();
        drop(old);
        assert_eq!(work.count(), 1);
        assert!(
            std::panic::catch_unwind(|| {
                let _panicking = work.enter();
                panic!("synthetic unwind");
            })
            .is_err()
        );
        assert_eq!(work.count(), 1);
        drop(newer);
        assert_eq!(work.count(), 0);
    }

    struct Probe<'a>(&'a ActiveWork, bool);

    impl Faculty for Probe<'_> {
        fn tools(&self) -> &[Tool] {
            assert_eq!(self.0.count(), 0, "metadata does not animate");
            &[]
        }

        fn call(&self, _: &str, _: Bytes, _: &mut Out<'_>) -> Result<()> {
            assert_eq!(self.0.count(), 3, "faculty overlaps sync and maintenance");
            if self.1 {
                anyhow::bail!("synthetic faculty failure");
            }
            Ok(())
        }
    }

    #[test]
    fn faculty_execution_shares_count_but_metadata_is_inert() {
        let work = ActiveWork::default();
        for fails in [false, true] {
            let probe = Probe(&work, fails);
            let faculty = work.faculty(&probe);
            assert!(faculty.tools().is_empty());
            let reconcile = work.enter();
            let maintenance = work.enter();
            let result = faculty.call(
                "probe",
                Bytes::from(b"{}".to_vec()),
                &mut Out::new(&mut |_| Ok(())),
            );
            assert_eq!(result.is_err(), fails);
            assert_eq!(work.count(), 2);
            drop(reconcile);
            assert_eq!(work.count(), 1);
            drop(maintenance);
            assert_eq!(work.count(), 0);
        }
    }

    #[test]
    fn disconnected_request_and_shutdown_do_not_hide_admitted_native_work() {
        use faculties::mcp::http;
        use serde_json::{Value, json};
        use std::{
            net::{Ipv4Addr, SocketAddr},
            sync::mpsc,
            thread,
            time::Duration,
        };

        struct BlockingFaculty {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Faculty for BlockingFaculty {
            fn tools(&self) -> &[Tool] {
                &[Tool {
                    name: "test_block",
                    description: "Test-only admitted native work",
                    input_schema: r#"{"type":"object","properties":{}}"#,
                }]
            }
            fn call(&self, _: &str, _: Bytes, _: &mut Out<'_>) -> Result<()> {
                self.entered.send(())?;
                self.release.recv_timeout(Duration::from_secs(10))?;
                Ok(())
            }
        }

        let work = ActiveWork::default();
        let token = "synthetic-activity-test-token-not-a-real-secret";
        let endpoint = http::Endpoint::bind(http::Config::new(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            http::BearerToken::new(token).unwrap(),
        ))
        .unwrap();
        let url = format!("http://{}/", endpoint.local_addr().unwrap());
        let (entered, admitted) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let native_work = work.clone();
        let worker = thread::spawn(move || {
            let faculty = BlockingFaculty {
                entered,
                release: wait,
            };
            endpoint.serve_until(&[&native_work.faculty(&faculty)], async move {
                let _ = stopping.changed().await;
            })
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let initialized = client
            .post(&url)
            .bearer_auth(token)
            .header("Accept", "application/json, text/event-stream")
            .json(
                &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"active-work-test","version":"1"}}}),
            )
            .send()
            .unwrap();
        let session = initialized.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let _: Value = initialized.json().unwrap();
        let request = |message: Value| {
            client
                .post(&url)
                .bearer_auth(token)
                .header("Accept", "application/json, text/event-stream")
                .header("mcp-session-id", &session)
                .header("mcp-protocol-version", "2025-06-18")
                .json(&message)
        };
        request(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .unwrap();
        assert_eq!(work.count(), 0, "transport setup is not work");
        let disconnected = request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"test_block","arguments":{}}}))
        .timeout(Duration::from_millis(200))
        .send();
        assert!(disconnected.unwrap_err().is_timeout());
        admitted.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            work.count(),
            1,
            "disconnected HTTP is still executing natively"
        );
        stop.send_replace(true);
        assert_eq!(work.count(), 1, "shutdown drains the admitted call");
        assert!(!worker.is_finished());
        release.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(work.count(), 0);
    }
}
