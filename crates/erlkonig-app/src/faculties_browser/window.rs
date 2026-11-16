//! Workspace choice and the shared Faculties viewer over native observations.
use GORBIE::{prelude::CardCtx, state::StateId};
use erlkonig_core::{runtime::Runtime, faculties_browser::{ViewerNode, ViewerWorker}, workspace::Workspace};
use faculties::widgets::storage::WorkspaceObservation;
use faculties::viewer::{self, Host, HostAction, Target};
use faculties::widgets::{FilesViewer, SourceKey, WidgetContext, WikiViewer};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::time::Duration;

#[derive(Clone, Default)]
struct Observation {
    generation: u64,
    ready: bool,
    choices: Vec<Workspace>, // transient choice labels/routes, never faculty rows
    unavailable: usize,
    requested: Option<Workspace>,
    data: Option<Arc<WorkspaceObservation>>,
    file_status: Option<String>,
    error: Option<String>,
    invalidated: bool,
}

// Request generations identify transient UI intent, not pile identities.
enum Request {
    Observe {
        generation: u64,
        selected: Option<Workspace>,
    },
    File {
        generation: u64,
        selected: Workspace,
        files: Arc<WorkspaceObservation>,
        action: HostAction,
    },
    Stop,
}

/// This source cell owns selection, cancellation and immutable observations.
/// Runtime owns the worker's join; the window retains its completion receipt.
/// Each shared card owns its navigation state independently.
struct Source {
    selected: Option<Workspace>,
    notice: Option<String>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    output: Arc<Mutex<Observation>>,
    sender: mpsc::Sender<Request>,
    worker: Option<ViewerWorker>,
}
impl Source {
    fn new(runtime: &mut Runtime, stop: Arc<AtomicBool>) -> Self {
        let output = Arc::new(Mutex::new(Observation::default()));
        let generation = Arc::new(AtomicU64::new(0));
        let (sender, receiver) = mpsc::channel();
        let shared = output.clone();
        let desired = generation.clone();
        let worker_stop = stop.clone();
        let worker = runtime.spawn_faculties(stop.clone(), move |node| {
            work(node, worker_stop, desired, receiver, shared);
        });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(_) => {
                output.lock().unwrap().error =
                    Some("Cannot start the local Faculties reader.".into());
                None
            }
        };
        Self {
            selected: None,
            notice: None,
            generation,
            stop,
            output,
            sender,
            worker,
        }
    }
    fn snapshot(&self) -> Observation {
        self.output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn select(&mut self, workspace: Option<Workspace>) {
        self.selected = workspace.clone();
        self.notice = None;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mut output = self.output.lock().unwrap_or_else(|e| e.into_inner());
        // Clear immediately, and distinguish Refresh/A→B→A from the old A.
        *output = Observation {
            generation,
            ready: output.ready,
            choices: output.choices.clone(),
            unavailable: output.unavailable,
            requested: workspace.clone(),
            ..Default::default()
        };
        drop(output);
        if self
            .sender
            .send(Request::Observe {
                generation,
                selected: workspace,
            })
            .is_err()
        {
            let mut output = self.output.lock().unwrap_or_else(|e| e.into_inner());
            output.ready = true;
            output.error = Some(
                "The Faculties reader stopped. Close this window and choose Browse Faculties again.".into(),
            );
        }
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.sender.send(Request::Stop);
        // Runtime owns the join handle and settles it before NodeStore closes.
    }
}

fn publish(output: &Mutex<Observation>, desired: &AtomicU64, result: Observation) {
    let mut output = output.lock().unwrap_or_else(|e| e.into_inner());
    if desired.load(Ordering::Acquire) == result.generation {
        *output = result;
    }
}

fn observe(
    node: &ViewerNode,
    stop: &AtomicBool,
    desired: &AtomicU64,
    generation: u64,
    selected: Option<Workspace>,
) -> Observation {
    let mut result = Observation {
        generation, ready: true, requested: selected.clone(), ..Default::default()
    };
    let cancelled = || stop.load(Ordering::Acquire) || generation != desired.load(Ordering::Acquire);
    let read = (|| -> Result<(), String> {
        let config = node.config(cancelled).map_err(|_| "Workspaces are unavailable.".to_owned())?;
        for id in config.workspace_ids().map_err(|_| "Workspace choices are unavailable.".to_owned())? {
            match config.workspace(id) {
                Ok(Some(workspace)) => result.choices.push(workspace),
                _ => result.unavailable += 1,
            }
        }
        result.choices.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
        let Some(choice) = selected.as_ref() else { return Ok(()); };
        if config.workspace(choice.id).ok().flatten().as_ref() != Some(choice) {
            result.invalidated = true;
            return Err("Workspace changed; choose it again.".into());
        }
        let data = node.with_workspace(choice, cancelled, |pile, signer| {
            pollster::block_on(WorkspaceObservation::observe(pile, signer, &choice.routes))
                .map_err(|error| std::io::Error::other(error).into())
        }).map_err(|_| "These views are unavailable on this device. Wait for sync, then Refresh.".to_owned())?;
        node.revalidate(choice, cancelled).map_err(|_| {
            result.invalidated = true;
            "Workspace changed; choose it again.".to_owned()
        })?;
        result.data = Some(Arc::new(data));
        Ok(())
    })();
    if let Err(error) = read { result.error = Some(error); result.data = None; }
    result
}

// Both file entry points use the same cancellation and exact-binding gate.
fn admit_file_action(
    stop: &AtomicBool,
    desired: &AtomicU64,
    generation: u64,
    revalidate: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let cancelled =
        || stop.load(Ordering::Acquire) || generation != desired.load(Ordering::Acquire);
    if cancelled() {
        return Err("The file request was cancelled.".into());
    }
    revalidate()?;
    if cancelled() {
        return Err("The file request was cancelled.".into());
    }
    Ok(())
}

fn work(
    node: ViewerNode,
    stop: Arc<AtomicBool>,
    desired: Arc<AtomicU64>,
    requests: mpsc::Receiver<Request>,
    output: Arc<Mutex<Observation>>,
) {
    // All storage observations and writes borrow the existing app owner.
    let mut initial = Some(Request::Observe {
        generation: 0,
        selected: None,
    });
    'work: loop {
        if stop.load(Ordering::Acquire) || node.is_stopping() {
            break;
        }
        let request = if let Some(initial) = initial.take() {
            initial
        } else {
            loop {
                if stop.load(Ordering::Acquire) || node.is_stopping() {
                    break 'work;
                }
                match requests.recv_timeout(Duration::from_millis(250)) {
                    Ok(request) => break request,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break 'work,
                    Err(mpsc::RecvTimeoutError::Timeout) => {} // Lifetime only; no idle storage polling.
                }
            }
        };
        match request {
            Request::Stop => break,
            Request::Observe {
                generation,
                selected,
            } => {
                if generation != desired.load(Ordering::Acquire) {
                    continue;
                }
                let result = observe(&node, &stop, &desired, generation, selected);
                if !stop.load(Ordering::Acquire) {
                    publish(&output, &desired, result);
                }
            }
            Request::File {
                generation,
                selected,
                files,
                action,
            } => {
                if generation != desired.load(Ordering::Acquire) {
                    continue;
                }
                // Resolve only against the immutable observation clicked by
                // the person, not the result of a later Refresh/selection.
                let sources = files.loaded_context();
                let mut binding_invalidated = false;
                let wrote = matches!(action, HostAction::Prioritize { .. });
                let result = (|| {
                    let mut before_export = || {
                        admit_file_action(&stop, &desired, generation, || {
                            node.revalidate(&selected, || stop.load(Ordering::Acquire) || generation != desired.load(Ordering::Acquire))
                                .map_err(|e| {
                                    binding_invalidated = true;
                                    e.to_string()
                                })
                        })
                    };
                    let files_view = || {
                        sources
                            .dataset(SourceKey::Files)
                            .ok_or_else(|| "Files are unavailable in this workspace.".to_owned())
                    };
                    match action {
                        HostAction::WikiLink(selector) => {
                            WikiViewer::open_file_with(files_view()?, &selector, &mut before_export)
                        }
                        HostAction::Entity(entity) => {
                            FilesViewer::open_entity_with(files_view()?, entity, &mut before_export)
                        }
                        // A priority answer is an ordinary Compass write into
                        // the same exact, revalidated workspace binding.
                        HostAction::Prioritize { higher, lower } => {
                            before_export()?;
                            let cancelled = || stop.load(Ordering::Acquire)
                                || node.is_stopping() || generation != desired.load(Ordering::Acquire);
                            node.with_workspace(&selected, cancelled, |pile, signer| {
                                let route = selected.routes.get(&faculties::schemas::compass::DEFAULT_SCOPE_ID)
                                    .ok_or_else(|| std::io::Error::other("Compass is unavailable in this workspace."))?;
                                pollster::block_on(faculties::compass::operations::prioritize_in(
                                    pile, signer, *route, higher, lower, || {
                                        if cancelled() { return Err(std::io::Error::other("The priority request was cancelled.").into()); }
                                        Ok(())
                                    },
                                )).map(drop)
                            }).map_err(|e| format!("{e:#}"))
                        }
                    }
                })();
                let output_handle = output.clone();
                let mut output = output.lock().unwrap_or_else(|e| e.into_inner());
                if !stop.load(Ordering::Acquire)
                    && generation == desired.load(Ordering::Acquire)
                    && output.generation == generation
                {
                    let succeeded = result.is_ok();
                    output.file_status = Some(match result {
                        Ok(()) if wrote => "Priority recorded.".into(),
                        Ok(()) => "Sent to the system viewer.".into(),
                        Err(error) => error,
                    });
                    if binding_invalidated {
                        output.invalidated = true;
                        output.error = output.file_status.clone();
                        output.data = None;
                    }
                    if wrote && succeeded && !binding_invalidated {
                        // Observe again so the next frame reads the new edge.
                        drop(output);
                        let result =
                            observe(&node, &stop, &desired, generation, Some(selected));
                        if !stop.load(Ordering::Acquire) {
                            publish(&output_handle, &desired, result);
                            // The fresh observation starts without a status;
                            // keep the answer's confirmation on screen.
                            let mut output =
                                output_handle.lock().unwrap_or_else(|e| e.into_inner());
                            if output.generation == generation {
                                output.file_status = Some("Priority recorded.".into());
                            }
                        }
                    }
                }
            }
        }
    }
}

fn label(workspace: &Workspace, choices: &[Workspace]) -> String {
    let name: String = workspace
        .label
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    if choices
        .iter()
        .filter(|other| other.label == workspace.label)
        .count()
        > 1
    {
        format!(
            "{} · {}",
            name,
            format!("{:X}", workspace.id)
                .chars()
                .take(8)
                .collect::<String>()
        )
    } else {
        name
    }
}

#[derive(Clone, Copy)]
struct WorkspaceHost(StateId<Source>);

impl Host for WorkspaceHost {
    fn with_context(
        &self,
        ctx: &mut CardCtx<'_>,
        render: impl FnOnce(
            &mut CardCtx<'_>,
            WidgetContext<'_>,
            u64,
            &mut dyn FnMut(HostAction) -> Result<(), String>,
        ),
    ) {
        let (selected, observed, generation, sender, output) = {
            let source = self.0.read(ctx);
            (
                source.selected.clone(),
                source.snapshot(),
                source.generation.load(Ordering::Acquire),
                source.sender.clone(),
                source.output.clone(),
            )
        };
        let Some(selected) = selected else {
            return;
        };
        if observed.generation != generation || observed.requested.as_ref() != Some(&selected) {
            return;
        }
        let Some(data) = observed.data else {
            return;
        };
        let mut open = |action: HostAction| {
            let status = match action {
                HostAction::Prioritize { .. } => "Recording priority…",
                HostAction::WikiLink(_) | HostAction::Entity(_) => {
                    if !data.loaded_context().contains(SourceKey::Files) {
                        return Err("Files are unavailable in this workspace.".into());
                    }
                    "Opening file…"
                }
            };
            output.lock().unwrap_or_else(|e| e.into_inner()).file_status = Some(status.into());
            sender
                .send(Request::File {
                    generation,
                    selected: selected.clone(),
                    files: data.clone(),
                    action,
                })
                .map_err(|_| "The file reader is unavailable.".to_owned())
        };
        render(ctx, data.loaded_context(), generation, &mut open);
    }
}

pub(super) fn notebook(
    runtime: &mut Runtime,
    context: &egui::Context,
    stop: Arc<AtomicBool>,
) -> (GORBIE::Notebook, Option<ViewerWorker>) {
    let source = Source::new(runtime, stop);
    let worker = source.worker.clone();
    let mut source = Some(source);
    let notebook = GORBIE::NotebookConfig::new("Erlkonig · Faculties").without_editor().embedded(context, move |nb| {
        let source = nb.state("workspace", || source.take().expect("source initialized once"), move |ctx, source| {
            let observed = source.snapshot();
            let current = source.generation.load(Ordering::Acquire);
            if observed.invalidated && observed.generation == current {
                source.select(None);
                source.notice = observed.error.clone();
            }
            ctx.heading("Faculties");
            ctx.set_default_section_open(false);
            ctx.label("Local snapshot · choose Refresh to see new synced content.");
            if let Some(selected) = source.selected.clone() {
                ctx.label(format!("Workspace: {}", label(&selected, &observed.choices)));
                ctx.horizontal(|ctx| {
                    if ctx.button("Choose workspace").clicked() { source.select(None); }
                    if ctx.button("Refresh").clicked() { source.select(source.selected.clone()); }
                });
            } else {
                ctx.horizontal(|ctx| {
                    ctx.label("Choose a workspace");
                    if ctx.button("Refresh").clicked() { source.select(None); }
                });
                if !observed.ready { ctx.label("Loading workspaces…"); }
                else if observed.choices.is_empty() && observed.error.is_none() {
                    ctx.label("No workspaces are available. Create or enroll one through Erlkonig's connected tools.");
                }
                for workspace in &observed.choices {
                    if ctx.button(label(workspace, &observed.choices)).clicked() { source.select(Some(workspace.clone())); }
                }
            }
            if let Some(notice) = &source.notice { ctx.label(egui::RichText::new(notice).color(egui::Color32::LIGHT_RED)); }
            if observed.unavailable > 0 { ctx.label(format!("{} workspace revision(s) are unavailable.", observed.unavailable)); }
            if let Some(error) = &observed.error { ctx.label(egui::RichText::new(error).color(egui::Color32::LIGHT_RED)); }
            if let Some(data) = &observed.data {
                let unavailable: Vec<_> = data.unavailable_labels().collect();
                if !unavailable.is_empty() {
                    ctx.label(format!("Unavailable in this workspace: {}.", unavailable.join(", ")));
                }
                if !SourceKey::ALL.into_iter().any(|key| data.loaded_context().contains(key)) {
                    ctx.label("No views are available in this workspace.");
                }
            } else if source.selected.is_some() && observed.error.is_none() {
                ctx.label("Loading views…");
            }
            if let Some(status) = &observed.file_status { ctx.label(status); }
            ctx.ctx().request_repaint_after(Duration::from_millis(250));
        });
        viewer::compose_hosted(nb, Target::Dashboard, WorkspaceHost(source));
    });
    (notebook, worker)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> (Source, mpsc::Receiver<Request>) {
        let (sender, receiver) = mpsc::channel();
        (
            Source {
                selected: None,
                notice: None,
                generation: Arc::new(AtomicU64::new(0)),
                stop: Arc::new(AtomicBool::new(false)),
                output: Arc::new(Mutex::new(Observation::default())),
                sender,
                worker: None,
            },
            receiver,
        )
    }
    #[test]
    fn stale_file_action_never_revalidates_or_crosses_the_export_gate() {
        let stop = AtomicBool::new(false);
        let desired = AtomicU64::new(3);
        assert!(admit_file_action(&stop, &desired, 1, || panic!("stale action")).is_err());
        stop.store(true, Ordering::Release);
        assert!(admit_file_action(&stop, &desired, 3, || panic!("stopped action")).is_err());
    }

    #[test]
    fn selection_or_stop_during_revalidation_cancels_export() {
        let stop = AtomicBool::new(false);
        let desired = AtomicU64::new(1);
        assert!(
            admit_file_action(&stop, &desired, 1, || {
                desired.store(2, Ordering::Release);
                Ok(())
            })
            .is_err()
        );
        assert!(
            admit_file_action(&stop, &desired, 2, || {
                stop.store(true, Ordering::Release);
                Ok(())
            })
            .is_err()
        );
    }

    #[test]
    fn duplicate_labels_have_distinct_choice_text() {
        let first = Workspace::new("Design");
        let second = Workspace::new("Design");
        let choices = vec![first.clone(), second.clone()];
        assert_ne!(label(&first, &choices), label(&second, &choices));
    }
    #[test]
    fn old_completion_is_rejected_after_same_selection_refresh_and_return() {
        let (mut source, _requests) = source();
        let a = Workspace::new("A");
        let b = Workspace::new("B");
        source.select(Some(a.clone()));
        let old = Observation {
            generation: source.generation.load(Ordering::Acquire),
            requested: Some(a.clone()),
            error: Some("old completion".into()),
            ..Default::default()
        };
        source.select(Some(a.clone())); // Refresh, same exact binding.
        publish(&source.output, &source.generation, old.clone());
        assert!(source.snapshot().error.is_none());
        source.select(Some(b));
        source.select(Some(a)); // A → B → A.
        publish(&source.output, &source.generation, old);
        assert!(source.snapshot().error.is_none());
        let current = source.generation.load(Ordering::Acquire);
        publish(
            &source.output,
            &source.generation,
            Observation {
                generation: current,
                error: Some("new completion".into()),
                ..Default::default()
            },
        );
        assert_eq!(source.snapshot().error.as_deref(), Some("new completion"));
    }
    #[test]
    fn stopped_worker_is_visible_instead_of_loading_forever() {
        let (mut source, requests) = source();
        drop(requests);
        source.select(Some(Workspace::new("Design")));
        assert!(
            source
                .snapshot()
                .error
                .as_deref()
                .unwrap()
                .contains("reader stopped")
        );
        assert!(source.snapshot().data.is_none());
    }
}
