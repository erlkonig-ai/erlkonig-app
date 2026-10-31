use super::*;
use crate::{
    config::Bootstrap,
    init,
    node_store::{NodeSettings, NodeStore},
    runtime::Runtime,
};
use std::{thread, time::Instant};
use triblespace::core::{
    collection::{CollectionCommit, CollectionRecord, CollectionStore, CollectionStoreExt},
    metadata, signing_key_file,
};
use triblespace::prelude::*;

#[path = "../../../../scripts/accept/legacy32_input.rs"]
mod legacy32_input;
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
    let after = config_view(
        config_snapshot(&selected.pile, &f.signer).unwrap(),
        &f.signer,
    )
    .unwrap();
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

fn add_missing_config_payload(path: &Path, signer: &SigningKey) {
    let mut node = NodeStore::open(path, signer.clone()).unwrap();
    let resident = node
        .pile
        .commit(
            node.config,
            signer,
            entity! { metadata::description: "synthetic missing-payload fixture" },
        )
        .unwrap();
    let missing = Inline::new([0x52; 32]); // Intentionally absent synthetic blob, not a schema ID.
    let commit = CollectionCommit::sign(signer, node.config.handle(), missing, resident.metadata());
    commit.verify_strict().unwrap();
    node.pile.insert(CollectionRecord::Commit(commit)).unwrap();
    node.close().unwrap();
}

#[test]
fn unavailable_config_payload_survives_framing_upgrade_for_normal_reconciliation() {
    let f = Fixture::new();
    f.configured();
    add_missing_config_payload(&f.binding.pile, &f.signer);
    old_framing::append_released_blob(&f.binding.pile);
    let source = fs::read(&f.binding.pile).unwrap();
    let before = config_view(
        config_snapshot(&f.binding.pile, &f.signer).unwrap(),
        &f.signer,
    )
    .unwrap();
    let unavailable = format!("{:#}", before.facts().unwrap_err());
    let control = StartupControl::new(true);
    let worker_control = control.clone();
    let layout = f.layout.clone();
    let binding = f.binding.clone();
    let signer = f.signer.clone();
    let worker = thread::spawn(move || prepare(&layout, binding, &signer, &worker_control));
    prompt(&control).answer(true);
    let selected = worker.join().unwrap().unwrap();
    let after = config_view(
        config_snapshot(&selected.pile, &f.signer).unwrap(),
        &f.signer,
    )
    .unwrap();
    assert_eq!(format!("{:#}", after.facts().unwrap_err()), unavailable);
    assert_eq!(fs::read(&f.binding.pile).unwrap(), source);
    assert_ne!(selected.pile, f.binding.pile);
    assert!(
        !f.layout.dir.join("mcp.token").exists(),
        "validation starts no services"
    );
    // The canonical boundary must not demand recovery of a missing payload
    // either; signed-record preservation is its separate native audit.
    validate(&f.binding.pile, &selected.pile, &f.signer, true).unwrap();
}

#[test]
fn readable_config_must_not_become_unavailable_in_the_candidate() {
    let f = Fixture::new();
    f.configured();
    let candidate = f.binding.pile.with_file_name("unavailable-candidate.pile");
    fs::copy(&f.binding.pile, &candidate).unwrap();
    add_missing_config_payload(&candidate, &f.signer);
    let error = validate(&f.binding.pile, &candidate, &f.signer, false).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("configuration facts unavailable"),
        "{error:#}"
    );
}

#[test]
fn native_config_acquisition_failures_are_compared_as_outcomes() {
    let f = Fixture::new();
    f.configured();
    let missing = f.binding.pile.with_file_name("missing-descriptor.pile");
    fs::write(&missing, []).unwrap();
    let acquire_missing = || {
        let snapshot = config_snapshot(&missing, &f.signer).unwrap();
        ConfigView::from_snapshot(
            snapshot,
            Collection::from_handle(faculties::config::handle(f.signer.verifying_key())),
        )
    };
    assert!(
        acquire_missing().is_err(),
        "the native acquisition boundary actually fails"
    );
    compare_config_views(acquire_missing(), acquire_missing()).unwrap();
    let readable = config_view(
        config_snapshot(&f.binding.pile, &f.signer).unwrap(),
        &f.signer,
    );
    assert!(compare_config_views(readable, acquire_missing()).is_err());
}

#[test]
fn cancellation_after_temp_durability_prevents_locator_publication_and_cleans_temp() {
    let dir = tempfile::tempdir().unwrap();
    let locator = dir.path().join("node.json");
    fs::write(&locator, b"original locator").unwrap();
    let control = StartupControl::new(true);
    let error = publish_locator(&locator, b"new locator", &control, || {
        let temporary = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path != &locator)
            .expect("durable sibling exists before the callback");
        assert_eq!(fs::read(temporary).unwrap(), b"new locator");
        assert_eq!(fs::read(&locator).unwrap(), b"original locator");
        control.cancel();
        Ok(())
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("before locator publication"));
    assert_eq!(fs::read(&locator).unwrap(), b"original locator");
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "cancelled publication removes its temp"
    );
}

#[test]
fn cancellation_after_rename_reports_the_committed_locator() {
    let dir = tempfile::tempdir().unwrap();
    let locator = dir.path().join("node.json");
    let control = StartupControl::new(true);
    publish_locator(&locator, b"selected locator", &control, || Ok(())).unwrap();
    control.cancel();
    assert!(
        control
            .check()
            .unwrap_err()
            .to_string()
            .contains("new locator remains selected")
    );
    assert_eq!(fs::read(locator).unwrap(), b"selected locator");
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

fn append_unknown_current_kind(path: &Path) {
    use std::io::Write;
    let records = triblespace::core::repo::pile::PileRecords::open(path).unwrap();
    let mut frame = [0u8; 256];
    // Reuse the fixture's actual current recovery prefix. This sentinel is
    // deliberately not a schema/format identifier introduced by the app.
    frame[..28].copy_from_slice(&records.bytes()[..28]);
    frame[28..32].copy_from_slice(&1u32.to_le_bytes());
    frame[32..64].fill(0xa5);
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&frame).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn current_unknown_bounded_kind_starts_without_migration_or_consent() {
    let f = Fixture::new();
    let workspace = f.configured();
    append_unknown_current_kind(&f.binding.pile);
    let inspection = migration::inspect(&f.binding.pile).unwrap();
    assert!(inspection.current_framing);
    assert_eq!(
        inspection.unsupported.len(),
        1,
        "unsupported for rewrite, not for ordinary replay"
    );
    let before = fs::read(&f.binding.pile).unwrap();
    let locator = f.locator();
    let mut runtime = Runtime::start(&f.layout).unwrap();
    assert_eq!(runtime.binding(), &f.binding);
    assert_eq!(
        runtime.config().unwrap().workspace(workspace.id).unwrap(),
        Some(workspace)
    );
    runtime.finish().unwrap();
    assert_eq!(f.locator(), locator);
    assert!(fs::read(&f.binding.pile).unwrap().starts_with(&before));
    assert_eq!(
        fs::read_dir(f.binding.pile.parent().unwrap())
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn unknown_kind_still_refuses_an_actual_rewrite() {
    let f = Fixture::new();
    f.configured();
    append_unknown_current_kind(&f.binding.pile);
    old_framing::append_released_blob(&f.binding.pile);
    let before = fs::read(&f.binding.pile).unwrap();
    let locator = f.locator();
    let error = Runtime::start(&f.layout).err().unwrap();
    assert!(
        error
            .to_string()
            .contains("format this Erlkonig cannot convert")
    );
    assert_eq!(f.locator(), locator);
    assert_eq!(fs::read(&f.binding.pile).unwrap(), before);
    assert!(!f.layout.dir.join("mcp.token").exists());
}

/// The fixture is signed/serialized by an independently built historical
/// library. No test helper here constructs old assertions or signatures.
fn historical_app_fixture(f: &mut Fixture) -> crate::workspace::Workspace {
    let workspace = f.configured();
    let historical = f.binding.pile.with_file_name("historical.pile");
    legacy32_input::export(
        f._dir.path(),
        &f.binding.pile,
        &f.binding.key,
        &hex::encode(f.binding.signer),
        &historical,
    );
    let writer = std::env::var_os("ERLKONIG_LEGACY32_WRITER")
        .expect("explicit independent historical writer binary is required");
    let output = std::process::Command::new(writer)
        .arg(f._dir.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "historical writer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        receipt["historical_core"],
        "3ca5c1cb5504d8ff60730edfe5bf641b01f44913"
    );
    assert_eq!(receipt["node_id"], hex::encode(f.binding.signer));
    f.binding.pile = historical;
    fs::write(
        f.layout.dir.join("node.json"),
        serde_json::to_vec(&NodeLocator {
            pile: f.binding.pile.clone(),
            key: f.binding.key.clone(),
            expected_signer: hex::encode(f.binding.signer),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(
        migration::inspect(&f.binding.pile)
            .unwrap()
            .requirements
            .iter()
            .any(|r| matches!(r, Requirement::Canonical { .. }))
    );
    workspace
}

#[test]
#[ignore = "requires ERLKONIG_LEGACY32_WRITER built against independent historical core 3ca5c1cb"]
fn historical_writer_config_and_workspace_survive_canonical_startup_and_restart() {
    let mut f = Fixture::new();
    let workspace = historical_app_fixture(&mut f);
    let original = fs::read(&f.binding.pile).unwrap();
    let key = fs::read(&f.binding.key).unwrap();
    let control = StartupControl::new(true);
    let worker_control = control.clone();
    let layout = f.layout.clone();
    let worker = thread::spawn(move || Runtime::start_controlled(&layout, &worker_control));
    let consent = prompt(&control);
    assert_eq!(consent.title, "Update local data?");
    consent.answer(true);
    let mut runtime = worker.join().unwrap().unwrap();
    let selected = runtime.binding().clone();
    assert_ne!(selected.pile, f.binding.pile);
    assert_eq!(selected.signer, f.binding.signer);
    assert_eq!(
        runtime.config().unwrap().workspace(workspace.id).unwrap(),
        Some(workspace.clone())
    );
    assert_eq!(
        runtime.config().unwrap().settings().unwrap(),
        NodeSettings {
            mcp_port: 0,
            sync_enabled: false,
            maintenance_enabled: false,
        }
    );
    runtime.finish().unwrap();
    assert_eq!(fs::read(&f.binding.pile).unwrap(), original);
    assert_eq!(fs::read(&f.binding.key).unwrap(), key);
    let locator = f.locator();
    let mut restarted = Runtime::start(&f.layout).unwrap();
    assert_eq!(restarted.binding(), &selected);
    assert_eq!(
        restarted.config().unwrap().workspace(workspace.id).unwrap(),
        Some(workspace)
    );
    restarted.finish().unwrap();
    assert_eq!(f.locator(), locator);
}

#[test]
#[ignore = "requires ERLKONIG_LEGACY32_WRITER built against independent historical core 3ca5c1cb"]
fn historical_foreign_author_is_not_replaced_by_the_nodes_signer() {
    let mut f = Fixture::new();
    historical_app_fixture(&mut f);
    let original = fs::read(&f.binding.pile).unwrap();
    let author = hex::encode(f.binding.signer);
    f.binding.key = f.binding.key.with_file_name("other-synthetic-node.key");
    f.signer = signing_key_file::init(&f.binding.key).unwrap();
    f.binding.signer = f.signer.verifying_key().to_bytes();
    fs::write(
        f.layout.dir.join("node.json"),
        serde_json::to_vec(&NodeLocator {
            pile: f.binding.pile.clone(),
            key: f.binding.key.clone(),
            expected_signer: hex::encode(f.binding.signer),
        })
        .unwrap(),
    )
    .unwrap();
    let locator = f.locator();
    let control = StartupControl::new(true);
    let worker_control = control.clone();
    let layout = f.layout.clone();
    let worker = thread::spawn(move || Runtime::start_controlled(&layout, &worker_control));
    let consent = prompt(&control);
    assert_eq!(consent.title, "Update local data?");
    consent.answer(true);
    let error = worker.join().unwrap().err().unwrap();
    assert!(error.to_string().contains("original authors"), "{error:#}");
    assert!(
        error.to_string().contains(&author),
        "the exact missing author is actionable"
    );
    assert_eq!(f.locator(), locator);
    assert_eq!(fs::read(&f.binding.pile).unwrap(), original);
    assert!(!f.layout.dir.join("mcp.token").exists());
}
