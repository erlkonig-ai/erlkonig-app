use super::*;
use crate::{
    config::Bootstrap,
    init,
    node_store::{NodeSettings, NodeStore},
    runtime::Runtime,
};
use std::{thread, time::Instant};
use triblespace::core::{collection::CollectionStoreExt, metadata, signing_key_file};
use triblespace::prelude::*;

#[path = "../../../../scripts/accept/old_framing.rs"]
mod old_framing;

struct Fixture {
    _dir: tempfile::TempDir,
    layout: Layout,
    signer: SigningKey,
    binding: NodeBinding,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        fs::create_dir(&store).unwrap();
        let bootstrap = Bootstrap::new(&store);
        let signer = init::prepare(&bootstrap).unwrap();
        let layout = Layout::application(dir.path().join("app")).unwrap();
        fs::create_dir(&layout.dir).unwrap();
        let binding = NodeBinding {
            pile: bootstrap.pile,
            key: bootstrap.key,
            signer: signer.verifying_key().to_bytes(),
        };
        let locator = NodeLocator {
            pile: binding.pile.clone(),
            key: binding.key.clone(),
            expected_signer: hex::encode(binding.signer),
        };
        fs::write(
            layout.dir.join("node.json"),
            serde_json::to_vec(&locator).unwrap(),
        )
        .unwrap();
        Self {
            _dir: dir,
            layout,
            signer,
            binding,
        }
    }

    fn configured(&self) -> crate::workspace::Workspace {
        let mut node = NodeStore::open(&self.binding.pile, self.signer.clone()).unwrap();
        let view = node.config().unwrap();
        node.put_settings(
            &view,
            &NodeSettings {
                mcp_port: 0,
                sync_enabled: false,
                maintenance_enabled: false,
            },
        )
        .unwrap();
        let view = node.config().unwrap();
        let id = node
            .create_workspace(&view, "Retained configured workspace")
            .unwrap();
        let workspace = node.config().unwrap().workspace(id).unwrap().unwrap();
        // Facts this application does not use remain harmless data.
        node.pile
            .commit(
                node.config,
                &self.signer,
                entity! { metadata::description: "unknown-to-app annotation" },
            )
            .unwrap();
        node.close().unwrap();
        workspace
    }

    fn locator(&self) -> Vec<u8> {
        fs::read(self.layout.dir.join("node.json")).unwrap()
    }

    fn legacy(&self) {
        fs::copy(old_framing::released_fixture(), &self.binding.pile).unwrap();
    }
}

fn prompt(control: &StartupControl) -> StartupPrompt {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(prompt) = control.take_prompt() {
            return prompt;
        }
        assert!(
            Instant::now() < deadline,
            "startup never offered its branch choice"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn framing_upgrade_retains_source_identity_config_and_restart_selection() {
    let f = Fixture::new();
    let workspace = f.configured();
    old_framing::append_released_blob(&f.binding.pile);
    let source = fs::read(&f.binding.pile).unwrap();
    let key = fs::read(&f.binding.key).unwrap();
    let control = StartupControl::new(true);
    let worker_control = control.clone();
    let layout = f.layout.clone();
    let worker = thread::spawn(move || Runtime::start_controlled(&layout, &worker_control));
    let consent = prompt(&control);
    assert_eq!(consent.title, "Update local data?");
    assert_eq!(consent.approve_label, "Migrate");
    assert_eq!(consent.decline_label, "Not now");
    assert_eq!(
        fs::read_dir(f.binding.pile.parent().unwrap())
            .unwrap()
            .count(),
        2,
        "no candidate exists before consent"
    );
    consent.answer(true);
    let mut runtime = worker.join().unwrap().unwrap();
    let selected = runtime.binding().clone();
    assert_ne!(selected.pile, f.binding.pile);
    assert_eq!(selected.pile.parent(), f.binding.pile.parent());
    assert_eq!(selected.key, f.binding.key);
    assert_eq!(selected.signer, f.binding.signer);
    let view = runtime.config().unwrap();
    assert_eq!(
        view.settings().unwrap(),
        NodeSettings {
            mcp_port: 0,
            sync_enabled: false,
            maintenance_enabled: false
        }
    );
    let after = view.workspace(workspace.id).unwrap().unwrap();
    assert_eq!(after.label, workspace.label);
    assert_eq!(after.routes, workspace.routes);
    runtime.finish().unwrap();
    assert_eq!(fs::read(&f.binding.pile).unwrap(), source);
    assert_eq!(fs::read(&f.binding.key).unwrap(), key);
    let locator = f.locator();
    let mut restarted = Runtime::start(&f.layout).unwrap();
    assert_eq!(restarted.binding(), &selected);
    assert_eq!(
        restarted
            .config()
            .unwrap()
            .workspace(workspace.id)
            .unwrap()
            .unwrap()
            .routes,
        workspace.routes
    );
    restarted.finish().unwrap();
    assert_eq!(f.locator(), locator);
    assert_eq!(fs::read(&f.binding.pile).unwrap(), source);
}

#[test]
fn declining_migration_and_noninteractive_start_never_create_a_candidate() {
    let f = Fixture::new();
    f.configured();
    old_framing::append_released_blob(&f.binding.pile);
    let source = fs::read(&f.binding.pile).unwrap();
    let locator = f.locator();
    let control = StartupControl::new(true);
    let worker_control = control.clone();
    let layout = f.layout.clone();
    let worker = thread::spawn(move || Runtime::start_controlled(&layout, &worker_control));
    let consent = prompt(&control);
    assert_eq!(consent.title, "Update local data?");
    consent.answer(false);
    let error = worker.join().unwrap().err().unwrap();
    assert!(error.to_string().contains("Local services are not running"));
    let error = Runtime::start(&f.layout).err().unwrap();
    assert!(error.to_string().contains("needs your approval"));
    assert_eq!(
        fs::read_dir(f.binding.pile.parent().unwrap())
            .unwrap()
            .count(),
        2
    );
    assert_eq!(fs::read(&f.binding.pile).unwrap(), source);
    assert_eq!(f.locator(), locator);
    assert!(!f.layout.dir.join("mcp.token").exists());
}

#[test]
fn current_pile_never_rewrites_even_when_it_retains_adopted_branch_pins() {
    let f = Fixture::new();
    f.legacy();
    let branch = migration::inspect(&f.binding.pile).unwrap().legacy_branches[0];
    let converted = f.binding.pile.with_file_name("already-converted.pile");
    let decisions = [BranchDecision {
        branch,
        collection_name: "Already imported",
        signer: &f.signer,
        authority: f.signer.verifying_key(),
    }];
    migration::prepare(&f.binding.pile, &converted, &decisions).unwrap();
    let binding = NodeBinding {
        pile: converted,
        ..f.binding.clone()
    };
    let locator = NodeLocator {
        pile: binding.pile.clone(),
        key: binding.key.clone(),
        expected_signer: hex::encode(binding.signer),
    };
    fs::write(
        f.layout.dir.join("node.json"),
        serde_json::to_vec(&locator).unwrap(),
    )
    .unwrap();
    let before = fs::read(&binding.pile).unwrap();
    let before_locator = f.locator();
    let control = StartupControl::new(false);
    assert_eq!(
        prepare(&f.layout, binding.clone(), &f.signer, &control).unwrap(),
        binding
    );
    assert_eq!(fs::read(&binding.pile).unwrap(), before);
    assert_eq!(f.locator(), before_locator);
    assert!(
        control.notice().is_none(),
        "retained pins are not a recurring warning"
    );
}

#[test]
fn branch_choice_is_explicit_and_locator_changes_are_not_overwritten() {
    let f = Fixture::new();
    f.legacy();
    let source = fs::read(&f.binding.pile).unwrap();
    let control = StartupControl::new(true);
    let worker_control = control.clone();
    let layout = f.layout.clone();
    let binding = f.binding.clone();
    let signer = f.signer.clone();
    let worker = thread::spawn(move || prepare(&layout, binding, &signer, &worker_control));
    let consent = prompt(&control);
    assert_eq!(consent.title, "Update local data?");
    consent.answer(true);
    let choice = prompt(&control);
    assert_eq!(choice.title, "Import legacy?");
    assert!(choice.details.contains(&hex::encode(f.binding.signer)));
    let changed = b"a concurrent locator change";
    fs::write(f.layout.dir.join("node.json"), changed).unwrap();
    choice.answer(true);
    let error = worker.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("locator changed"), "{error:#}");
    assert_eq!(f.locator(), changed);
    assert_eq!(fs::read(&f.binding.pile).unwrap(), source);
}

#[test]
fn cancellation_drains_a_pending_choice_without_publishing() {
    let f = Fixture::new();
    f.legacy();
    let source = fs::read(&f.binding.pile).unwrap();
    let locator = f.locator();
    let control = StartupControl::new(true);
    let worker_control = control.clone();
    let layout = f.layout.clone();
    let worker = thread::spawn(move || Runtime::start_controlled(&layout, &worker_control));
    let consent = prompt(&control);
    consent.answer(true);
    let choice = prompt(&control);
    assert!(
        Runtime::start(&f.layout).is_err(),
        "startup retains node.lock while waiting"
    );
    control.cancel();
    choice.answer(true); // A stale native response cannot reactivate cancelled work.
    assert!(worker.join().unwrap().is_err());
    assert_eq!(f.locator(), locator);
    assert_eq!(fs::read(&f.binding.pile).unwrap(), source);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.layout.lock())
        .unwrap();
    assert!(lock.try_lock().is_ok(), "joined startup released its lock");
}

#[test]
fn unresolved_settings_and_concurrent_workspace_revisions_are_preserved() {
    let f = Fixture::new();
    let workspace = f.configured();
    let mut node = NodeStore::open(&f.binding.pile, f.signer.clone()).unwrap();
    node.pile
        .commit(
            node.config,
            &f.signer,
            entity! {
                faculties::schemas::config::config::anchor: crate::node_schema::NODE_SETTINGS,
                crate::node_schema::mcp_port: 0u16,
            },
        )
        .unwrap();
    node.pile
        .commit(
            node.config,
            &f.signer,
            entity! {
                metadata::tag: crate::node_schema::WORKSPACE,
                faculties::schemas::config::config::anchor: workspace.anchor,
                metadata::name: "Concurrent retained workspace",
                crate::node_schema::enrolled: true,
            },
        )
        .unwrap();
    let before = node.config().unwrap();
    let settings_error = format!("{:#}", before.settings().unwrap_err());
    let workspace_error = format!("{:#}", before.workspace(workspace.id).unwrap_err());
    node.close().unwrap();
    old_framing::append_released_blob(&f.binding.pile);
    let source = fs::read(&f.binding.pile).unwrap();
    let control = StartupControl::new(true);
    let worker_control = control.clone();
    let layout = f.layout.clone();
    let binding = f.binding.clone();
    let signer = f.signer.clone();
    let worker = thread::spawn(move || prepare(&layout, binding, &signer, &worker_control));
    let consent = prompt(&control);
    assert_eq!(consent.title, "Update local data?");
    consent.answer(true);
    let selected = worker.join().unwrap().unwrap();
    let after = config_view(&selected.pile, &f.signer).unwrap().unwrap();
    assert_eq!(
        format!("{:#}", after.settings().unwrap_err()),
        settings_error
    );
    assert_eq!(
        format!("{:#}", after.workspace(workspace.id).unwrap_err()),
        workspace_error
    );
    assert_eq!(fs::read(&f.binding.pile).unwrap(), source);
}

#[test]
fn missing_candidate_config_is_rejected_instead_of_becoming_empty_settings() {
    let f = Fixture::new();
    f.configured();
    let empty = f
        .binding
        .pile
        .with_file_name("candidate-missing-config.pile");
    fs::write(&empty, []).unwrap();
    let locator = f.locator();
    let error = validate(&f.binding.pile, &empty, &f.signer, false).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("lost the previously resident application configuration")
    );
    assert_eq!(f.locator(), locator);
}

#[test]
fn missing_identity_never_generates_a_replacement() {
    let f = Fixture::new();
    f.configured();
    let locator = f.locator();
    fs::remove_file(&f.binding.key).unwrap();
    assert!(Runtime::start(&f.layout).is_err());
    assert!(!f.binding.key.exists(), "never invent a replacement key");
    assert_eq!(f.locator(), locator);
}

#[test]
fn malformed_storage_fails_without_opening_services_or_changing_the_locator() {
    let f = Fixture::new();
    let source = b"unsupported synthetic storage framing";
    fs::write(&f.binding.pile, source).unwrap();
    let locator = f.locator();
    let key = signing_key_file::load_existing(&f.binding.key)
        .unwrap()
        .verifying_key();
    assert!(Runtime::start(&f.layout).is_err());
    assert_eq!(f.locator(), locator);
    assert_eq!(fs::read(&f.binding.pile).unwrap(), source);
    assert_eq!(
        signing_key_file::load_existing(&f.binding.key)
            .unwrap()
            .verifying_key(),
        key
    );
    assert!(!f.layout.dir.join("mcp.token").exists());
}
