//! Workspace choice and the shared Faculties viewer over native observations.
use GORBIE::{prelude::CardCtx, state::StateId};
use erlkonig_core::{config::NodeBinding, faculties_browser::ConfigReader, workspace::Workspace};
use faculties::storage::Storage;
use faculties::viewer::{self, FileAction, Host, Target};
use faculties::widgets::{FilesViewer, SourceKey, StorageState, WidgetContext, WikiViewer};
use std::sync::{
    Arc, Mutex, OnceLock,
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
    data: Option<Arc<StorageState>>,
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
        files: Arc<StorageState>,
        action: FileAction,
    },
    Stop,
}

/// This source cell owns selection, its worker and immutable observations.
/// Each shared card owns its navigation state independently.
struct Source {
    selected: Option<Workspace>,
    notice: Option<String>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    output: Arc<Mutex<Observation>>,
    sender: mpsc::Sender<Request>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Source {
    fn new(node: NodeBinding, stop: Arc<AtomicBool>) -> Self {
        let output = Arc::new(Mutex::new(Observation::default()));
        let generation = Arc::new(AtomicU64::new(0));
        let (sender, receiver) = mpsc::channel();
        let shared = output.clone();
        let desired = generation.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("faculties-observation".into())
            .spawn(move || work(node, worker_stop, desired, receiver, shared));
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
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn publish(output: &Mutex<Observation>, desired: &AtomicU64, result: Observation) {
    let mut output = output.lock().unwrap_or_else(|e| e.into_inner());
    if desired.load(Ordering::Acquire) == result.generation {
        *output = result;
    }
}

fn retire_owner(owner: &mut Option<(Workspace, Storage)>) -> Result<(), String> {
    if let Some((_, storage)) = owner.take() {
        // close takes the shared Session before closing it. UI-held Storage
        // clones then contain no owner to close on their eventual drop;
        // native frozen snapshots keep their resident bytes readable.
        storage.close().map_err(|error| {
            eprintln!("Faculties reader close failed: {error:#}");
            "The previous workspace could not close cleanly. Close this browser and reopen it from Erlkonig.".to_owned()
        })?;
    }
    Ok(())
}

fn observe(
    node: &NodeBinding,
    reader: &mut Option<ConfigReader>,
    owner: &mut Option<(Workspace, Storage)>,
    generation: u64,
    selected: Option<Workspace>,
) -> Observation {
    let mut result = Observation {
        generation,
        ready: true,
        requested: selected.clone(),
        ..Default::default()
    };
    let read = (|| -> Result<(), String> {
        if reader.is_none() {
            *reader = Some(ConfigReader::open(node).map_err(|e| e.to_string())?);
        }
        let reader = reader.as_mut().unwrap();
        let config = reader.config().map_err(|e| e.to_string())?;
        for id in config
            .workspace_ids()
            .map_err(|_| "Workspace choices are unavailable.".to_owned())?
        {
            match config.workspace(id) {
                Ok(Some(workspace)) => result.choices.push(workspace),
                _ => result.unavailable += 1,
            }
        }
        result
            .choices
            .sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
        let Some(choice) = selected.as_ref() else {
            retire_owner(owner)?;
            return Ok(());
        };
        if config.workspace(choice.id).ok().flatten().as_ref() != Some(choice) {
            result.invalidated = true;
            return Err("Workspace changed; choose it again.".into());
        }
        // Create and retain the actual store on this worker. Nothing acquires
        // a serving endpoint or peer data; the app remains the sync owner.
        if owner.as_ref().is_none_or(|(binding, _)| binding != choice) {
            retire_owner(owner)?;
            let public = ed25519_dalek::VerifyingKey::from_bytes(&node.signer)
                .map_err(|_| "Erlkonig’s local data cannot be opened.".to_owned())?;
            let storage = Storage::shared_local(node.pile.clone(), Some(node.key.clone()))
                .with_collections(choice.routes.clone())
                .with_expected_signer(public);
            *owner = Some((choice.clone(), storage));
        }
        let storage = owner.as_ref().unwrap().1.clone();
        let mut data = StorageState::for_workspace(storage)?;
        data.refresh();
        if data.error().is_some() {
            return Err(
                "These views are unavailable on this device. Wait for sync, then Refresh.".into(),
            );
        }
        result.data = Some(Arc::new(data));
        reader.revalidate(choice).map_err(|e| {
            result.invalidated = true;
            e.to_string()
        })?;
        Ok(())
    })();
    if let Err(error) = read {
        result.error = Some(error);
        result.data = None;
    }
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
    node: NodeBinding,
    stop: Arc<AtomicBool>,
    desired: Arc<AtomicU64>,
    requests: mpsc::Receiver<Request>,
    output: Arc<Mutex<Observation>>,
) {
    // Construct/retain the native reader and store on their actual worker.
    let mut reader = None;
    let mut owner = None;
    let mut initial = Some(Request::Observe {
        generation: 0,
        selected: None,
    });
    'work: loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let request = if let Some(initial) = initial.take() {
            initial
        } else {
            loop {
                if stop.load(Ordering::Acquire) {
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
                let result = observe(&node, &mut reader, &mut owner, generation, selected);
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
                let result = (|| {
                    let view = sources
                        .dataset(SourceKey::Files)
                        .ok_or_else(|| "Files are unavailable in this workspace.".to_owned())?;
                    let mut before_export = || {
                        admit_file_action(&stop, &desired, generation, || {
                            reader
                                .as_mut()
                                .ok_or_else(|| "Workspaces are unavailable.".to_owned())?
                                .revalidate(&selected)
                                .map_err(|e| {
                                    binding_invalidated = true;
                                    e.to_string()
                                })
                        })
                    };
                    match action {
                        FileAction::WikiLink(selector) => {
                            WikiViewer::open_file_with(view, &selector, &mut before_export)
                        }
                        FileAction::Entity(entity) => {
                            FilesViewer::open_entity_with(view, entity, &mut before_export)
                        }
                    }
                })();
                let mut output = output.lock().unwrap_or_else(|e| e.into_inner());
                if !stop.load(Ordering::Acquire)
                    && generation == desired.load(Ordering::Acquire)
                    && output.generation == generation
                {
                    output.file_status = Some(match result {
                        Ok(()) => "Sent to the system viewer.".into(),
                        Err(error) => error,
                    });
                    if binding_invalidated {
                        output.invalidated = true;
                        output.error = output.file_status.clone();
                        output.data = None;
                    }
                }
            }
        }
    }
    let _ = retire_owner(&mut owner); // Detailed failure is reported on the worker.
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
            &mut dyn FnMut(FileAction) -> Result<(), String>,
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
        let mut open = |action| {
            if !data.loaded_context().contains(SourceKey::Files) {
                return Err("Files are unavailable in this workspace.".into());
            }
            output.lock().unwrap_or_else(|e| e.into_inner()).file_status =
                Some("Opening file…".into());
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

pub(super) fn run(
    node: NodeBinding,
    stop: Arc<AtomicBool>,
    window: Arc<OnceLock<egui::Context>>,
) -> Result<(), String> {
    GORBIE::NotebookConfig::new("Erlkonig · Faculties").run(move |nb| {
        let closing = stop.clone();
        let window = window.clone();
        let source = nb.state("workspace", || Source::new(node.clone(), stop.clone()), move |ctx, source| {
            super::attach_window(&closing, &window, ctx.ctx());
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
    }).map_err(|_| "The Faculties window could not be opened.".to_owned())
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
