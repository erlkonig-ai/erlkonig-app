use super::*;
use crate::{
    config::Bootstrap, init, node_store::NodeStore, workspace_dispatch::WorkspaceDispatch,
};
use faculties::out::Part;
use ed25519_dalek::VerifyingKey;
use std::sync::{Arc, Mutex};
use triblespace::core::collection::{CollectionRecord, CollectionStore, CollectionStoreExt};
use triblespace::core::repo::{BlobStoreList, BlobStorePut, CapabilityProofRead, SnapshotSource, StorageFlush};

struct Fixture {
    config: ConfigMcp,
    dispatch: WorkspaceDispatch,
    _dir: tempfile::TempDir,
}

#[test]
fn runtime_inspection_redacts_errors_and_retry_does_not_edit_config() {
    use crate::status::{CleanupFailure, ServiceState, Status};
    let mut f = Fixture::new();
    let status = Arc::new(Mutex::new(Status::default()));
    {
        let mut state = status.lock().unwrap();
        state.node.services = ServiceState::Stopped;
        state.node.stop_reason = Some("service settings changed".into());
        state.node.problem = Some("private path /fixture/self.pile and blake3:CAPABILITY".into());
        state.node.cleanup_failures = vec![CleanupFailure::EndpointShutdownTimeout];
    }
    let (stop, _) = tokio::sync::watch::channel(false);
    let (control, requests) = ServiceControl::new(status.clone(), stop.clone());
    f.config.services = control;
    let before = f.inspect();
    assert_eq!(before["runtime"]["services"], "stopped");
    assert_eq!(before["runtime"]["cleanup_failures"], json!(["endpoint_shutdown_timeout"]));
    assert_eq!(before["runtime"]["session_failed"], true);
    assert!(!before.to_string().contains("CAPABILITY"));
    assert!(!before.to_string().contains("/fixture"));
    assert_eq!(f.call("config_retry_services", json!({})).unwrap()["queued"], true);
    assert_eq!(f.call("config_retry_services", json!({})).unwrap()["queued"], true);
    assert!(requests.try_recv().is_ok());
    assert!(requests.try_recv().is_err(), "repeated retry coalesces");
    assert_eq!(f.inspect()["observation"], before["observation"]);
    {
        let mut state = status.lock().unwrap();
        state.node.services = ServiceState::Running;
        state.node.retry_pending = false;
    }
    assert_eq!(f.call("config_retry_services", json!({})).unwrap()["queued"], false);
    assert!(requests.try_recv().is_err());
    stop.send_replace(true);
    assert!(f.call("config_retry_services", json!({})).is_err());
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let binding = NodeBinding {
            pile: bootstrap.pile.clone(),
            key: bootstrap.key,
            signer: signer.verifying_key().to_bytes(),
        };
        let node = Arc::new(Mutex::new(Some(
            NodeStore::open(&bootstrap.pile, signer).unwrap(),
        )));
        let reader = node.clone();
        let dispatch = WorkspaceDispatch::new(binding.clone(), move || {
            reader.lock().unwrap().as_mut().unwrap().config()
        })
        .unwrap();
        let (stop, _) = tokio::sync::watch::channel(false);
        let (services, _) = ServiceControl::new(Default::default(), stop);
        Self {
            config: ConfigMcp::new(binding, node, "http://127.0.0.1:41773/".into(), 41773, services),
            dispatch,
            _dir: dir,
        }
    }

    fn call(&self, name: &str, args: Value) -> Result<Value> {
        serde_json::from_str(&call(&self.config, name, args)?).map_err(Into::into)
    }

    fn inspect(&self) -> Value {
        self.call("config_inspect", json!({})).unwrap()
    }

    fn create(&self, label: &str) -> Value {
        self.call(
            "config_create_workspace",
            json!({"observation":self.inspect()["observation"],"label":label}),
        )
        .unwrap()["workspace"]
            .clone()
    }

    fn annotate(&self, fragment: Fragment) {
        let mut guard = self.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        node.pile
            .commit(node.config, &node.signer, fragment)
            .unwrap();
        node.pile.flush().unwrap();
    }

    fn proofs(&self) -> usize {
        self.config
            .node
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .pile
            .snapshot()
            .unwrap()
            .proofs()
            .unwrap()
            .count()
    }

    fn share(&self, workspace: &Value, recipient: &Fixture, access: &str, delegate: Option<bool>) -> Result<Value> {
        let mut args = json!({"observation":self.inspect()["observation"],
            "workspace":workspace,"recipient":hex::encode(recipient.config.binding.signer),"access":access});
        if let Some(delegate) = delegate {
            args["delegate"] = json!(delegate);
        }
        self.call("config_share_workspace", args)
    }

    /// Deterministic native evidence acquisition, followed by the same local
    /// pending-share operation exposed to clients; no file or network claim.
    fn enroll_native(&self, owner: &Fixture, workspace: &Workspace) -> Result<Value> {
        owner.repair_authority_to(self, workspace);
        owner.publish_descriptors_to(self, workspace);
        self.enroll_pending(&workspace.anchor, &self.shown_roots(&workspace.anchor),
            Some("Local shared workspace"))
    }

    fn list_pending(&self) -> Value {
        self.call("config_list_pending_shares", json!({})).unwrap()
    }

    fn enroll_pending(&self, anchor: &Id, roots: &[String], label: Option<&str>) -> Result<Value> {
        let mut args = json!({"observation":self.inspect()["observation"],
            "anchor":format!("{anchor:X}"),"roots":roots});
        if let Some(label) = label {
            args["label"] = json!(label);
        }
        self.call("config_enroll_pending_share", args)
    }

    /// The roots config_list_pending_shares prints for one anchor right now.
    fn shown_roots(&self, anchor: &Id) -> Vec<String> {
        let pending = self.list_pending();
        pending["shares"].as_array().unwrap().iter()
            .find(|share| share["anchor"] == format!("{anchor:X}"))
            .map(|share| share["collections"].as_array().unwrap().iter()
                .map(|row| row["root"].as_str().unwrap().to_owned()).collect())
            .unwrap_or_default()
    }

    /// Owner side: register one more descriptor for `workspace`, tagged with
    /// its anchor and declaring `role`, and publish the grown revision.
    fn grow_workspace(&self, workspace: &mut Workspace, role: &str) -> Id {
        use triblespace::core::capability::policy;
        use triblespace::core::collection::{Collection, records};
        use triblespace::core::metadata::MetaDescribe;
        let role_id = workspace::roles().into_iter().find(|(_, name)| *name == role).unwrap().0;
        let mut guard = self.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let signer = node.signer.verifying_key();
        let name = format!("{} · {role}", workspace.label);
        let collection: Collection<blobencodings::SimpleArchive> = node.pile.register_collection(entity! {
            metadata::tag*: [records::KIND_COLLECTION_DESCRIPTOR, workspace.anchor],
            records::collection_name: name.as_str(),
            attrs::collection_role: role_id,
            records::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
            policy::resource_policy*: faculties::collection_names::private_policy(signer).fragment(),
        }).unwrap();
        workspace.routes.insert(role_id, collection.handle());
        let view = node.config().unwrap();
        node.put_workspace(&view, workspace).unwrap()
    }

    /// A local workspace over explicit routes, as config_create_workspace
    /// with explicit routes would publish it.
    fn put_local_workspace(&self, label: &str, routes: BTreeMap<Id, CollectionHandle>) -> Id {
        let mut guard = self.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let mut workspace = Workspace::new(label);
        workspace.routes = routes;
        let view = node.config().unwrap();
        node.put_workspace(&view, &workspace).unwrap()
    }

    /// Owner side: share `revision` natively and let the recipient acquire
    /// its proofs, definitions and descriptors.
    fn deliver(&self, revision: Id, workspace: &Workspace, recipient: &Fixture) {
        self.share(&json!(format!("{revision:X}")), recipient, "read_only", None).unwrap();
        self.repair_authority_to(recipient, workspace);
        self.publish_descriptors_to(recipient, workspace);
    }

    /// Deterministic stand-in for native descriptor acquisition: copy exactly
    /// the root descriptors and their names, as sync would, nothing else.
    fn publish_descriptors_to(&self, recipient: &Fixture, workspace: &Workspace) {
        self.publish_descriptor_blobs_to(recipient, workspace, true);
    }

    /// The descriptors alone: sync may land a descriptor before the name
    /// blob it references.
    fn publish_bare_descriptors_to(&self, recipient: &Fixture, workspace: &Workspace) {
        self.publish_descriptor_blobs_to(recipient, workspace, false);
    }

    fn publish_descriptor_blobs_to(&self, recipient: &Fixture, workspace: &Workspace, with_names: bool) {
        use triblespace::core::repo::transfer;
        let mut source = self.config.node.lock().unwrap();
        let source = source.as_mut().unwrap();
        let snapshot = source.pile.snapshot().unwrap();
        let mut target = recipient.config.node.lock().unwrap();
        let target = target.as_mut().unwrap();
        let mut handles: Vec<Inline<inlineencodings::Handle<triblespace::core::blob::encodings::UnknownBlob>>> = Vec::new();
        for root in workspace.routes.values() {
            let facts: TribleSet = snapshot.get(*root).unwrap();
            handles.push(root.transmute());
            if !with_names {
                continue;
            }
            if let Some(name) = triblespace::core::collection::descriptor::name(&facts).unwrap() {
                handles.push(name.transmute());
            }
        }
        for copied in transfer(&snapshot, &mut target.pile, handles) {
            copied.unwrap();
        }
        target.pile.flush().unwrap();
    }

    fn workspace(&self, id: &Value) -> Workspace {
        self.config.node.lock().unwrap().as_mut().unwrap().config().unwrap()
            .workspace(revision(id.as_str().unwrap()).unwrap()).unwrap().unwrap()
    }

    fn admission(&self, workspace: &Workspace) -> Vec<(bool, bool)> {
        let mut guard = self.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let snapshot = node.pile.snapshot().unwrap();
        workspace.routes.values().map(|root| {
            let collection = triblespace::core::collection::Collection::<blobencodings::SimpleArchive>::open(&snapshot, *root).unwrap();
            (collection.reader_is_admitted(&snapshot, node.signer.verifying_key()).unwrap(),
                collection.writer_is_admitted(&snapshot, node.signer.verifying_key()).unwrap())
        }).collect()
    }

    fn selection(&self) -> crate::workspace::AllAccessible {
        let mut guard = self.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let roots = node.workspace_roots().unwrap();
        let snapshot = node.pile.snapshot().unwrap();
        let mut selected = crate::workspace::AllAccessible::discover(&snapshot, node.signer.verifying_key(), roots).unwrap();
        selected.sync = crate::workspace::selected_sync_roots(&snapshot, node.signer.verifying_key()).unwrap();
        selected
    }

    /// Deterministic native AUTH + exact-definition acquisition fixture, not
    /// a network-completion assertion. The server's pinned native audience
    /// admits the recipient BEFORE its resident proofs/definitions exist.
    fn repair_authority_to(&self, recipient: &Fixture, workspace: &Workspace) {
        use triblespace::core::collection::CollectionReadAudience;
        use triblespace::core::repo::{CapabilityProofStore, transfer};
        let mut source = self.config.node.lock().unwrap();
        let source = source.as_mut().unwrap();
        let snapshot = source.pile.snapshot().unwrap();
        let mut target = recipient.config.node.lock().unwrap();
        let target = target.as_mut().unwrap();
        for root in workspace.routes.values().copied().collect::<std::collections::BTreeSet<_>>() {
            let overlay = triblespace_net::collection_activation::collection_repair_overlay(&snapshot, root).unwrap();
            let evidence = overlay.authorization_evidence();
            match evidence.authorized_readers(&snapshot) {
                CollectionReadAudience::Open => {}
                CollectionReadAudience::Restricted(subjects) => assert!(subjects.contains(&target.signer.verifying_key()), "server resident authority admits this endpoint"),
            }
            let proofs = evidence.proofs().cloned().collect::<Vec<_>>();
            // AUTH repairs proof bytes, not definitions. Their native blob
            // references are acquired separately through ordinary exact-H.
            for proof in &proofs {
                target.pile.insert_proof(proof.clone()).unwrap();
            }
            for copied in transfer(&snapshot, &mut target.pile, proofs.iter().flat_map(|proof| proof.blob_references())) {
                copied.unwrap();
            }
        }
        target.pile.flush().unwrap();
    }

    /// A deterministic native replication fixture, not a network-completion
    /// assertion: copy foundations and their blob closure for these exact roots.
    fn replicate_to(&self, recipient: &Fixture, workspace: &Workspace) {
        use triblespace::core::collection::{CollectionRead, CollectionRecordSelector};
        use triblespace::core::repo::{reachable, transfer};
        let mut source = self.config.node.lock().unwrap();
        let source = source.as_mut().unwrap();
        source.pile.refresh().unwrap();
        let snapshot = source.pile.snapshot().unwrap();
        let selectors = workspace.routes.values().map(|root| CollectionRecordSelector::Foundations(*root)).collect();
        let records = snapshot.select_records(&selectors).unwrap();
        let mut target = recipient.config.node.lock().unwrap();
        let target = target.as_mut().unwrap();
        let refs = records.iter().flat_map(|record| record.blob_references());
        for copied in transfer(&snapshot, &mut target.pile, reachable(&snapshot, refs)) {
            copied.unwrap();
        }
        for record in records {
            target.pile.insert(record).unwrap();
        }
        target.pile.flush().unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.dispatch.close().unwrap();
        self.config
            .node
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .close()
            .unwrap();
    }
}

fn root_handles(workspace: &Workspace) -> Vec<String> {
    workspace.routes.values().map(|root| format!("blake3:{}", hex::encode(root.raw))).collect()
}

fn call(faculty: &impl Faculty, name: &str, args: Value) -> Result<String> {
    let mut text = String::new();
    faculty.call(
        name,
        Bytes::from(serde_json::to_vec(&args)?),
        &mut Out::new(&mut |part| {
            if let Part::Text { text: value } = part {
                text.push_str(&value);
            }
            Ok(())
        }),
    )?;
    Ok(text)
}

#[test]
fn sharing_read_only_enrolls_same_roots_and_reads_without_write_or_delegation() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let third = Fixture::new();
    let id = owner.create("Shared research");
    let original = owner.workspace(&id);
    assert_eq!(original.routes.len(), workspace::roles().len());
    call(&owner.dispatch, "compass_add", json!({"workspace":id,"title":"Shared evidence"})).unwrap();
    let before = owner.inspect();
    let shared = owner.share(&id, &recipient, "read_only", None).unwrap();
    let key = triblespace::core::signing_key_file::load_existing(&owner.config.binding.key).unwrap();
    let encoded = shared.to_string();
    assert!(!encoded.contains(&hex::encode(key.to_bytes())));
    assert!(!encoded.contains(owner.config.binding.key.to_str().unwrap()));
    assert!(!encoded.contains(owner.config.binding.pile.to_str().unwrap()));
    assert_eq!(shared["delegate"], false);
    assert_eq!(shared["sync_complete"], false);
    assert_eq!(before["observation"], owner.inspect()["observation"], "sharing does not edit Config");
    let owner_proofs = owner.proofs();
    let repeated = owner.share(&id, &recipient, "read_only", None).unwrap();
    assert_eq!(repeated, shared);
    assert_eq!(owner.proofs(), owner_proofs, "repeat issue is a native set no-op");
    {
        let mut guard = owner.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let snapshot = node.pile.snapshot().unwrap();
        let key = VerifyingKey::from_bytes(&recipient.config.binding.signer).unwrap();
        for root in original.routes.values() {
            let collection = triblespace::core::collection::Collection::<blobencodings::SimpleArchive>::open(&snapshot, *root).unwrap();
            assert!(collection.reader_is_admitted(&snapshot, key).unwrap(), "grant is valid before recipient enrollment");
        }
    }
    assert_eq!(recipient.inspect()["workspaces"], json!([]));
    assert!(original.routes.values().all(|root| !recipient.selection().sync.contains(root)),
        "a sender grant does not select recipient roots");
    let accepted = recipient.enroll_native(&owner, &original).unwrap();
    assert_eq!(accepted["access"], "read_only");
    assert_eq!(accepted["delegate"], false);
    assert_eq!(accepted["sync_complete"], false);
    let local_id = &accepted["workspace"]["workspace"];
    let local = recipient.workspace(local_id);
    assert_eq!(local.label, "Local shared workspace", "recipient chooses a local alias");
    assert_eq!(local.routes, original.routes);
    {
        use triblespace::core::collection::selection::{sync_selection, Selection};
        let facts = recipient.config.node.lock().unwrap().as_mut().unwrap().config().unwrap().facts().unwrap();
        assert!(local.routes.values().all(|root| sync_selection(&facts, *root) == Selection::Selected), "enrollment bridges to the native selection register");
    }
    let selected = recipient.selection();
    assert!(local.routes.values().all(|root| selected.sync.contains(root)));
    assert!(recipient.admission(&local).iter().all(|value| *value == (true, false)));
    // Native evidence acquisition and local enrollment did not copy member data.
    assert!(!call(&recipient.dispatch, "compass_list", json!({"workspace":local_id})).unwrap().contains("Shared evidence"));
    owner.replicate_to(&recipient, &original);
    assert!(call(&recipient.dispatch, "compass_list", json!({"workspace":local_id})).unwrap().contains("Shared evidence"));
    assert!(call(&recipient.dispatch, "compass_add", json!({"workspace":local_id,"title":"Denied write"})).is_err());
    let proof_count = recipient.proofs();
    assert!(recipient.share(local_id, &third, "read_only", None).is_err());
    assert_eq!(recipient.proofs(), proof_count, "failed delegation publishes no proof");
    let repeated = recipient.enroll_pending(&original.anchor, &root_handles(&original), Some("Local shared workspace")).unwrap();
    assert_eq!(repeated["already_enrolled"], true);
    assert_eq!(repeated["workspace"]["workspace"], *local_id);
    assert_eq!(recipient.proofs(), proof_count);
    assert!(recipient.enroll_pending(&original.anchor, &root_handles(&original),
        Some("Different alias")).is_err(), "repeat cannot silently change a chosen alias");
}

#[test]
fn sharing_read_write_admits_authored_commit_and_opt_in_delegation_is_bounded() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let third = Fixture::new();
    let fourth = Fixture::new();
    let fifth = Fixture::new();
    let unrelated = owner.create("Private unrelated");
    let invoke_only = owner.create("Invoke-only read/write");
    let recipient_private = recipient.create("Recipient private");
    let recipient_private_routes = recipient.workspace(&recipient_private).routes;
    let settings = recipient.call("config_set_settings", json!({"observation":recipient.inspect()["observation"],
        "mcp_port":0,"sync_enabled":false,"maintenance_enabled":false})).unwrap()["settings"].clone();
    let runtime = recipient.inspect()["runtime"].clone();
    let key_bytes = std::fs::read(&recipient.config.binding.key).unwrap();
    let id = owner.create("Collaboration");
    let original = owner.workspace(&id);
    owner.share(&invoke_only, &recipient, "read_write", None).unwrap();
    let invoke_accept = recipient.enroll_native(&owner, &owner.workspace(&invoke_only)).unwrap();
    let invoke_id = &invoke_accept["workspace"]["workspace"];
    owner.repair_authority_to(&recipient, &owner.workspace(&invoke_only));
    assert!(recipient.admission(&recipient.workspace(invoke_id)).iter().all(|value| *value == (true, true)));
    assert!(recipient.share(invoke_id, &third, "read_only", None).is_err(), "WRITE invocation is not delegation");
    owner.share(&id, &recipient, "read_write", Some(true)).unwrap();
    let accepted = recipient.enroll_native(&owner, &original).unwrap();
    let local_id = &accepted["workspace"]["workspace"];
    let local = recipient.workspace(local_id);
    owner.repair_authority_to(&recipient, &original);
    assert_eq!(local.routes, original.routes);
    assert_eq!(recipient.workspace(&recipient_private).routes, recipient_private_routes);
    assert_eq!(recipient.inspect()["settings"], settings);
    assert_eq!(recipient.inspect()["runtime"], runtime);
    assert_eq!(recipient.inspect()["node_id"], hex::encode(recipient.config.binding.signer));
    assert_eq!(std::fs::read(&recipient.config.binding.key).unwrap(), key_bytes);
    assert!(recipient.admission(&local).iter().all(|value| *value == (true, true)));
    call(&recipient.dispatch, "compass_add", json!({"workspace":local_id,"title":"Recipient authored"})).unwrap();
    assert!(call(&recipient.dispatch, "compass_list", json!({"workspace":local_id})).unwrap().contains("Recipient authored"));
    // The grantor already persisted recipient evidence; an imported recipient
    // foundation is admitted without re-signing or copying a key.
    recipient.replicate_to(&owner, &local);
    assert!(call(&owner.dispatch, "compass_list", json!({"workspace":id})).unwrap().contains("Recipient authored"));
    recipient.share(local_id, &third, "read_only", Some(true)).unwrap();
    let accepted_third = third.enroll_native(&recipient, &local).unwrap();
    let third_id = &accepted_third["workspace"]["workspace"];
    recipient.repair_authority_to(&third, &local);
    assert!(third.admission(&third.workspace(third_id)).iter().all(|value| *value == (true, false)));
    let before = third.proofs();
    assert!(third.share(third_id, &fourth, "read_write", Some(true)).is_err(), "cannot broaden actions");
    assert_eq!(third.proofs(), before, "denied WRITE leaves staged READ unpublished");
    third.share(third_id, &fourth, "read_only", None).unwrap();
    let final_accept = fourth.enroll_native(&third, &third.workspace(third_id)).unwrap();
    third.repair_authority_to(&fourth, &third.workspace(third_id));
    assert!(fourth.share(&final_accept["workspace"]["workspace"], &fifth, "read_only", None).is_err(), "invoke-only attenuation denies a new participant, not a directly authorized existing owner");
    let private_roots = owner.workspace(&unrelated).routes;
    let mut guard = recipient.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let snapshot = node.pile.snapshot().unwrap();
    assert!(private_roots.values().all(|root| !snapshot.contains_blob(*root).unwrap()), "unrelated workspace not disclosed");
    let mut permitted_roots = original.routes.values().copied().collect::<Vec<_>>();
    permitted_roots.extend(owner.workspace(&invoke_only).routes.values().copied());
    assert!(snapshot.proofs().unwrap().all(|proof| {
        let resource = proof.unwrap().resource().into_bytes();
        permitted_roots.iter().any(|root| root.raw == resource)
    }));
}

#[test]
fn sharing_native_evidence_does_not_admit_a_different_recipient() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let wrong = Fixture::new();
    let id = owner.create("Recipient bound grants");
    let original = owner.workspace(&id);
    owner.share(&id, &recipient, "read_only", None).unwrap();
    owner.repair_authority_to(&recipient, &original);
    owner.publish_descriptors_to(&wrong, &original);
    // Even resident valid proofs and definitions for another endpoint cannot
    // grant this node native admission or make its pending enrollment valid.
    {
        use triblespace::core::repo::{CapabilityProofStore, transfer};
        let mut source = recipient.config.node.lock().unwrap();
        let snapshot = source.as_mut().unwrap().pile.snapshot().unwrap();
        let proofs = snapshot.proofs().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let mut target = wrong.config.node.lock().unwrap();
        let target = target.as_mut().unwrap();
        for proof in &proofs { target.pile.insert_proof(proof.clone()).unwrap(); }
        for copied in transfer(&snapshot, &mut target.pile,
            proofs.iter().flat_map(|proof| proof.blob_references())) { copied.unwrap(); }
        target.pile.flush().unwrap();
    }
    assert!(wrong.admission(&original).iter().all(|value| *value == (false, false)));
    assert_eq!(wrong.list_pending()["shares"], json!([]));
    let before = wrong.inspect();
    let proofs = wrong.proofs();
    assert!(wrong.enroll_pending(&original.anchor, &root_handles(&original), Some("Wrong recipient")).is_err());
    assert_eq!(wrong.inspect(), before);
    assert_eq!(wrong.proofs(), proofs);
    let mut invalid_subject = json!({"observation":owner.inspect()["observation"],"workspace":id,
        "recipient":"00".repeat(32),"access":"read_only"});
    assert!(owner.call("config_share_workspace", invalid_subject.clone()).is_err());
    invalid_subject["recipient"] = json!("/arbitrary/key/path");
    assert!(owner.call("config_share_workspace", invalid_subject).is_err());
}

#[test]
fn sharing_freezes_current_bundle_and_never_resurrects_or_overwrites_enrollment() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let id = owner.create("First revision");
    let extra = owner.create("Future roots");
    let original = owner.workspace(&id);
    owner.share(&id, &recipient, "read_only", None).unwrap();
    let mut replacement = owner.workspace(&extra);
    replacement.anchor = original.anchor;
    let mut guard = owner.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let view = node.config().unwrap();
    node.put_workspace(&view, &replacement).unwrap();
    drop(guard);
    assert!(owner.share(&id, &recipient, "read_only", None).is_err(), "old revision cannot issue fresh grants");
    let accepted = recipient.enroll_native(&owner, &original).unwrap();
    let local_id = &accepted["workspace"]["workspace"];
    assert_eq!(recipient.workspace(local_id).routes, original.routes, "issued grants cover the original roots, never future roots");
    let before = recipient.inspect();
    let key_before = std::fs::read(&recipient.config.binding.key).unwrap();
    recipient.call("config_remove_workspace", json!({"observation":before["observation"],"workspace":local_id})).unwrap();
    let after = recipient.inspect();
    assert!(recipient.enroll_pending(&original.anchor, &root_handles(&original), Some("Local shared workspace")).is_err(), "repeat cannot resurrect withdrawn workspace");
    assert_eq!(after, recipient.inspect());
    assert_eq!(after["node_id"], before["node_id"]);
    assert_eq!(after["settings"], before["settings"]);
    assert_eq!(after["runtime"], before["runtime"]);
    assert_eq!(key_before, std::fs::read(&recipient.config.binding.key).unwrap());
    assert_eq!(after["workspaces"], json!([]));
    assert!(call(&recipient.dispatch, "compass_list", json!({})).is_err(), "no global default after accepting");
}

#[test]
fn sharing_uses_independent_quorum_paths_and_ignores_unavailable_future_policy() {
    use ed25519_dalek::SigningKey;
    use triblespace::core::capability::{capability_action, capability_delegate_action, policy};
    use triblespace::core::collection::{AdmissionPolicy, Collection, CollectionPolicy, ACTION_READ, records};
    use triblespace::core::metadata::MetaDescribe;
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let second_root = SigningKey::from_bytes(&[91; 32]);
    let mut external = MemoryRepo::default();
    let future = external.put::<blobencodings::SimpleArchive, _>(
        entity! { capability_action: genid().id }.facts().clone(),
    ).unwrap();
    let mut guard = owner.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let signer = node.signer.verifying_key();
    let read = AdmissionPolicy::quorum([signer, second_root.verifying_key()], 2, None).unwrap();
    let ordinary = CollectionPolicy::new(read, AdmissionPolicy::direct(signer));
    let collection: Collection<blobencodings::SimpleArchive> = node.pile.register_collection(entity! {
        metadata::tag: records::KIND_COLLECTION_DESCRIPTOR,
        records::collection_name: "Two independent roots",
        records::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
        policy::resource_policy*: ordinary.fragment() + AdmissionPolicy::direct(signer).binding(future),
    }).unwrap();
    assert!(!node.pile.snapshot().unwrap().contains_blob(future).unwrap());
    let grant = node.pile.put::<blobencodings::SimpleArchive, _>(entity! {
        capability_action: ACTION_READ, capability_delegate_action: ACTION_READ,
    }.facts().clone()).unwrap();
    triblespace::core::collection::grant_collection_capability(
        &mut node.pile, collection.handle(), grant, &second_root, signer,
    ).unwrap();
    let role = workspace::roles().into_iter().find(|(_, name)| *name == "compass").unwrap().0;
    let mut value = Workspace::new("Quorum workspace");
    value.routes.insert(role, collection.handle());
    let view = node.config().unwrap();
    let id = node.put_workspace(&view, &value).unwrap();
    drop(guard);
    let id = json!(format!("{id:X}"));
    owner.share(&id, &recipient, "read_only", None).unwrap();
    owner.repair_authority_to(&recipient, &value);
    owner.publish_descriptors_to(&recipient, &value);
    let accepted = recipient.call("config_create_workspace", json!({
        "observation":recipient.inspect()["observation"], "label":"Local quorum",
        "routes":[{"role":"compass","root":format!("blake3:{}", hex::encode(collection.handle().raw))}]})).unwrap();
    let local = recipient.workspace(&accepted["workspace"]);
    assert_eq!(local.routes, value.routes);
    assert_eq!(recipient.admission(&local), [(true, false)]);
    let mut guard = recipient.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let snapshot = node.pile.snapshot().unwrap();
    assert_eq!(snapshot.proofs().unwrap().filter(|proof| proof.as_ref().unwrap().leaf_key() == node.signer.verifying_key()).count(),
        2, "two recipient-bound rooted paths; native AUTH also retains the issuer's parent proof");
}

#[test]
fn sharing_counts_distinct_resident_roots_for_each_policy_until_invocation_is_admitted() {
    use triblespace::core::capability::{capability_action, capability_delegate_action, policy};
    use triblespace::core::collection::{ACTION_READ, AdmissionPolicy, Collection, CollectionPolicy, records};
    use triblespace::core::repo::{CapabilityProofStore, transfer};

    let first = Fixture::new();
    let second = Fixture::new();
    let other = Fixture::new();
    let recipient = Fixture::new();
    let key = |fixture: &Fixture| VerifyingKey::from_bytes(&fixture.config.binding.signer).unwrap();
    let (a, b, c) = (key(&first), key(&second), key(&other));
    let spare = ed25519_dalek::SigningKey::from_bytes(&[49; 32]).verifying_key();
    let write_only_root = ed25519_dalek::SigningKey::from_bytes(&[89; 32]);
    let role = workspace::roles().into_iter().find(|(_, name)| *name == "compass").unwrap().0;
    let anchor = genid().id;
    let policy = CollectionPolicy::new(
        AdmissionPolicy::quorum([a, b, spare], 2, None).unwrap(),
        AdmissionPolicy::quorum([a, b, c, write_only_root.verifying_key()], 1, None).unwrap(),
    ).with_capability(
        entity! { capability_action: ACTION_READ },
        AdmissionPolicy::quorum([c, spare], 2, None).unwrap(),
    );

    // Register the same public descriptor on each fixture, as if acquired
    // natively. All three signers can bind it through their own WRITE access.
    let bind = |fixture: &Fixture| {
        let root = {
            let mut guard = fixture.config.node.lock().unwrap();
            let node = guard.as_mut().unwrap();
            let collection: Collection<blobencodings::SimpleArchive> = node.pile.register_collection(entity! {
                metadata::tag*: [records::KIND_COLLECTION_DESCRIPTOR, anchor],
                records::collection_name: "Council",
                records::collection_representation*: <blobencodings::SimpleArchive as triblespace::core::metadata::MetaDescribe>::describe(),
                attrs::collection_role: role,
                policy::resource_policy*: policy.fragment(),
            }).unwrap();
            collection.handle()
        };
        let label = format!("Council {}", hex::encode(fixture.config.binding.signer));
        let workspace = fixture.call("config_create_workspace", json!({
            "observation":fixture.inspect()["observation"], "label":label,
            "routes":[{"role":"compass", "root":format!("blake3:{}", hex::encode(root.raw))}],
        })).unwrap();
        (workspace["workspace"].clone(), root)
    };
    let (first_id, root) = bind(&first);
    let (second_id, second_root) = bind(&second);
    let (other_id, other_root) = bind(&other);
    assert_eq!((second_root, other_root), (root, root));
    first.publish_descriptors_to(&recipient, &first.workspace(&first_id));

    // Native resource participants can acquire proofs before they have
    // collection READ. Transfer only this resource's exact evidence.
    let acquire = |source: &Fixture, target: &Fixture| {
        let mut guard = source.config.node.lock().unwrap();
        let snapshot = guard.as_mut().unwrap().pile.snapshot().unwrap();
        let mut guard = target.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        for proof in snapshot.proofs().unwrap() {
            let proof = proof.unwrap();
            if proof.resource().into_bytes() != root.raw {
                continue;
            }
            for copied in transfer(&snapshot, &mut node.pile, proof.blob_references()) { copied.unwrap(); }
            node.pile.insert_proof(proof).unwrap();
        }
        node.pile.flush().unwrap();
    };
    let counts = |action: &Value| {
        let mut counts = BTreeMap::new();
        for alternative in action["alternatives"].as_array().unwrap() {
            let roots = alternative["roots"].as_array().unwrap();
            assert_eq!(alternative["threshold"], 2);
            assert_eq!(alternative["contributing_roots"].as_array().unwrap().len() as u64,
                alternative["have"].as_u64().unwrap());
            counts.insert(roots.len(), alternative["have"].as_u64().unwrap());
        }
        counts
    };
    let pending = first.share(&first_id, &recipient, "read_write", Some(true)).unwrap();
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["collections"][0]["write"]["admitted"], true);
    assert_eq!(counts(&pending["collections"][0]["read"]), BTreeMap::from([(2, 0), (3, 1)]));
    let before = first.proofs();
    assert_eq!(first.share(&first_id, &recipient, "read_write", Some(true)).unwrap(), pending);
    assert_eq!(first.proofs(), before);

    // A configured subject supplies its own quorum share. Repeating an
    // invite to itself must not grow a chain of identical self-delegations.
    let own = first.share(&first_id, &first, "read_only", Some(true)).unwrap();
    assert_eq!(counts(&own["collections"][0]["read"]), BTreeMap::from([(2, 0), (3, 1)]));
    let own_proofs = first.proofs();
    assert_eq!(first.share(&first_id, &first, "read_only", Some(true)).unwrap(), own);
    assert_eq!(first.proofs(), own_proofs);

    // A different signed path from the same trust root is still one share.
    first.share(&first_id, &recipient, "read_only", None).unwrap();
    acquire(&first, &recipient);
    let observed = recipient.list_pending();
    let row = &observed["shares"][0]["collections"][0];
    assert_eq!(counts(&row["admission"]["read"]), BTreeMap::from([(2, 0), (3, 1)]));
    assert_eq!(row["admitted"], json!({"read":false,"write":true}));
    assert_eq!(recipient.admission(&first.workspace(&first_id)), [(false, true)]);

    acquire(&first, &other);
    let other_share = other.share(&other_id, &recipient, "read_only", None).unwrap();
    assert_eq!(other_share["status"], "pending", "alternatives cannot pool votes");
    assert_eq!(counts(&other_share["collections"][0]["read"]), BTreeMap::from([(2, 1), (3, 1)]));
    acquire(&other, &second);
    let complete = second.share(&second_id, &recipient, "read_only", None).unwrap();
    assert_eq!(complete["status"], "admitted");
    let read = &complete["collections"][0]["read"];
    assert_eq!(counts(read), BTreeMap::from([(2, 1), (3, 2)]));
    for alternative in read["alternatives"].as_array().unwrap() {
        let expected: BTreeSet<_> = if alternative["roots"].as_array().unwrap().len() == 3 {
            [a, b].into_iter().map(|key| hex::encode(key.as_bytes())).collect()
        } else {
            BTreeSet::from([hex::encode(c.as_bytes())])
        };
        let actual: BTreeSet<String> = serde_json::from_value(alternative["contributing_roots"].clone()).unwrap();
        assert_eq!(actual, expected);
    }
    acquire(&second, &recipient);
    let row = &recipient.list_pending()["shares"][0]["collections"][0];
    assert_eq!(row["admitted"], json!({"read":true,"write":true}));
    assert_eq!(recipient.admission(&first.workspace(&first_id)), [(true, true)]);

    // Only a has given this recipient a delegable share. Onward sharing
    // contributes a's READ vote only, despite this node's full invocation.
    let onward = Fixture::new();
    let recipient_binding = recipient.call("config_create_workspace", json!({
        "observation":recipient.inspect()["observation"], "label":"Council member",
        "routes":[{"role":"compass", "root":format!("blake3:{}", hex::encode(root.raw))}],
    })).unwrap();
    let partial = recipient.share(&recipient_binding["workspace"], &onward, "read_only", None).unwrap();
    assert_eq!(partial["status"], "pending");
    assert_eq!(counts(&partial["collections"][0]["read"]), BTreeMap::from([(2, 0), (3, 1)]));

    // A valid signed/delegable READ definition rooted outside both READ
    // policies cannot contribute. Resident votes already admitting the
    // invitee cannot authorize an unrelated signer to issue a share.
    let stranger = Fixture::new();
    first.publish_descriptors_to(&stranger, &first.workspace(&first_id));
    acquire(&second, &stranger);
    let stranger_id = stranger.put_local_workspace("Known council", first.workspace(&first_id).routes);
    {
        let mut guard = stranger.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let handle = node.pile.put::<blobencodings::SimpleArchive, _>(entity! {
            capability_action: ACTION_READ, capability_delegate_action: ACTION_READ,
        }.facts().clone()).unwrap();
        node.pile.insert_proof(triblespace::core::capability::CapabilityProof::new(
            root.into(), &write_only_root, handle, key(&stranger),
        )).unwrap();
    }
    let before = stranger.proofs();
    assert!(stranger.share(&json!(format!("{stranger_id:X}")), &recipient, "read_only", None).is_err());
    assert_eq!(stranger.proofs(), before);
}

#[test]
fn zero_workspaces_bootstrap_without_paths_keys_tokens_or_grants() {
    let f = Fixture::new();
    let before = f.inspect();
    assert_eq!(before["workspaces"], json!([]));
    assert_eq!(before["node_id"], hex::encode(f.config.binding.signer));
    let encoded = before.to_string();
    assert!(!encoded.contains(f.config.binding.key.to_str().unwrap()));
    assert!(!encoded.contains(f.config.binding.pile.to_str().unwrap()));
    let key = triblespace::core::signing_key_file::load_existing(&f.config.binding.key).unwrap();
    assert!(!encoded.contains(&hex::encode(key.to_bytes())));
    assert!(!encoded.contains("token"));
    let view = f
        .config
        .node
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .config()
        .unwrap();
    for member in view.observed.support().unwrap().members() {
        assert!(
            !encoded.contains(&hex::encode(member.raw)),
            "no Config blob bearer handles"
        );
    }
    let proofs = f.proofs();
    let id = f.create("First");
    assert_eq!(
        f.proofs(),
        proofs,
        "workspace creation issues no sharing grant"
    );
    assert!(call(&f.dispatch, "compass_list", json!({})).is_err());
    call(
        &f.dispatch,
        "compass_add",
        json!({"workspace":id,"title":"first task"}),
    )
    .unwrap();
    assert!(
        call(&f.dispatch, "compass_list", json!({"workspace":id}))
            .unwrap()
            .contains("first task")
    );
    assert_eq!(f.inspect()["workspaces"][0]["workspace"], id);
    for tool in f.config.tools() {
        let schema: Value = serde_json::from_str(tool.input_schema).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        for forbidden in ["key", "token", "path", "pile", "secret"] {
            assert!(schema["properties"].get(forbidden).is_none());
        }
    }
}

#[test]
fn missing_label_is_redacted_and_arrival_invalidates_stateless_observation() {
    let f = Fixture::new();
    let mut source = MemoryRepo::default();
    let label = source
        .put::<blobencodings::UTF8String, _>("Arrived later")
        .unwrap();
    let item = genid();
    f.annotate(
        entity! { &item @ metadata::tag:attrs::WORKSPACE, config::anchor:item.id,
        attrs::enrolled:true, metadata::name:label },
    );
    let before = f.inspect();
    assert_eq!(before["unavailable"].as_array().unwrap().len(), 1);
    assert!(!before.to_string().contains(&hex::encode(label.raw)));
    assert!(
        !call(&f.dispatch, "list_workspaces", json!({}))
            .unwrap()
            .contains(&hex::encode(label.raw))
    );
    let error = call(
        &f.dispatch,
        "compass_list",
        json!({"workspace":format!("{:X}",item.id)}),
    )
    .unwrap_err();
    assert!(!format!("{error:#}").contains(&hex::encode(label.raw)));
    {
        let mut guard = f.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        node.pile
            .put::<blobencodings::UTF8String, _>("Arrived later")
            .unwrap();
        node.pile.flush().unwrap();
    }
    let bytes = std::fs::read(&f.config.binding.pile).unwrap();
    let error = f
        .call(
            "config_remove_workspace",
            json!({"observation":before["observation"],"workspace":format!("{:X}",item.id)}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("changed since inspection"));
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), bytes);
    assert_ne!(f.inspect()["observation"], before["observation"]);
}

#[test]
fn missing_config_payload_error_never_exposes_a_bearer_handle() {
    let f = Fixture::new();
    let handle = {
        let mut guard = f.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let mut source = MemoryRepo::default();
        let commit = source
            .commit(
                node.config,
                &node.signer,
                entity! {
                    config::anchor:attrs::NODE_SETTINGS,attrs::mcp_port:0_u16,
                    attrs::sync_enabled:false,attrs::maintenance_enabled:false,
                },
            )
            .unwrap();
        node.pile.insert(CollectionRecord::Commit(commit)).unwrap();
        commit.data()
    };
    let error = f.call("config_inspect", json!({})).unwrap_err();
    assert!(error.to_string().contains("configuration is unavailable"));
    assert!(!format!("{error:#}").contains(&hex::encode(handle.raw)));
    let error = call(&f.dispatch, "list_workspaces", json!({})).unwrap_err();
    assert!(!format!("{error:#}").contains(&hex::encode(handle.raw)));
}

#[test]
fn stale_invalid_edits_append_nothing_and_old_revisions_never_retarget() {
    let f = Fixture::new();
    let old = f.inspect()["observation"].clone();
    let id = f.create("Before");
    let before = std::fs::read(&f.config.binding.pile).unwrap();
    assert!(
        f.call(
            "config_edit_workspace",
            json!({"observation":old,"workspace":id,"label":"stale"})
        )
        .is_err()
    );
    let observation = f.inspect()["observation"].clone();
    for args in [
        json!({"observation":observation,"workspace":id,"label":" "}),
        json!({"observation":observation,"workspace":"Before","label":"invalid"}),
        json!({"observation":observation,"workspace":id,"routes":[]}),
        json!({"observation":observation,"workspace":id,"key":"untrusted"}),
    ] {
        assert!(f.call("config_edit_workspace", args).is_err());
    }
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), before);
    let next = f
        .call(
            "config_edit_workspace",
            json!({"observation":observation,"workspace":id,"label":"After"}),
        )
        .unwrap()["workspace"]
        .clone();
    assert_ne!(next, id);
    assert!(call(&f.dispatch, "compass_list", json!({"workspace":id})).is_err());
    call(&f.dispatch, "compass_list", json!({"workspace":next})).unwrap();
    let proofs = f.proofs();
    f.call(
        "config_remove_workspace",
        json!({"observation":f.inspect()["observation"],"workspace":next}),
    )
    .unwrap();
    assert!(call(&f.dispatch, "compass_list", json!({"workspace":next})).is_err());
    assert_eq!(f.inspect()["workspaces"], json!([]));
    assert_eq!(f.proofs(), proofs);
}

#[test]
fn existing_roots_use_explicit_roles_admission_and_never_grant_access() {
    let f = Fixture::new();
    let first = f.create("Origin");
    let inspected = f.inspect();
    let roles = inspected["workspaces"][0]["roles"].as_array().unwrap();
    let root = roles.iter().find(|role| role["role"] == "compass").unwrap()["root"].clone();
    let listed = f.call("config_list_collections", json!({})).unwrap();
    assert!(
        listed["collections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|choice| choice["root"] == root
                && choice["read"] == true
                && choice["write"] == true)
    );
    let proofs = f.proofs();
    let shared = f.call("config_create_workspace",json!({"observation":inspected["observation"],"label":"Same goals","routes":[{"role":"compass","root":root}]})).unwrap()["workspace"].clone();
    call(
        &f.dispatch,
        "compass_add",
        json!({"workspace":shared,"title":"same exact root"}),
    )
    .unwrap();
    assert!(
        call(&f.dispatch, "compass_list", json!({"workspace":first}))
            .unwrap()
            .contains("same exact root")
    );
    assert!(call(&f.dispatch, "wiki_list", json!({"workspace":shared})).is_err());
    assert_eq!(f.proofs(), proofs);
    let denied = {
        let mut guard = f.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[72; 32]).verifying_key();
        let collection = node
            .pile
            .collection(
                "not yours",
                faculties::collection_names::private_policy(stranger),
            )
            .unwrap();
        node.pile.flush().unwrap();
        format!("blake3:{}", hex::encode(collection.handle().raw))
    };
    let before = std::fs::read(&f.config.binding.pile).unwrap();
    assert!(f.call("config_create_workspace",json!({"observation":f.inspect()["observation"],"label":"Denied","routes":[{"role":"compass","root":denied}]})).is_err());
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), before);
}

#[test]
fn settings_conflicts_are_inspected_and_explicitly_reconciled() {
    let f = Fixture::new();
    for port in [12123_u16, 12124] {
        f.annotate(
            entity! { config::anchor:attrs::NODE_SETTINGS, attrs::mcp_port:port,
            attrs::sync_enabled:true, attrs::maintenance_enabled:false },
        );
    }
    let inspected = f.inspect();
    assert!(inspected["settings"].is_null());
    assert!(
        inspected["settings_problem"]
            .as_str()
            .unwrap()
            .contains("conflicting")
    );
    assert_eq!(inspected["settings_revisions"].as_array().unwrap().len(), 2);
    let before = std::fs::read(&f.config.binding.pile).unwrap();
    let mut args = json!({"observation":inspected["observation"],"mcp_port":0,"sync_enabled":false,"maintenance_enabled":true,"peers":["invalid"]});
    assert!(f.call("config_set_settings", args.clone()).is_err());
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), before);
    args["peers"] = json!([]);
    assert!(f.call("config_set_settings", args.clone()).is_err(), "removed peer fields are unsupported, not accepted as no-ops");
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), before);
    args.as_object_mut().unwrap().remove("peers");
    let saved = f.call("config_set_settings", args.clone()).unwrap();
    assert_eq!(saved["mcp_url"], "http://127.0.0.1:41773/");
    assert_eq!(saved["port_restart_required"], true);
    let now = f.inspect();
    assert!(now["settings_problem"].is_null());
    assert_eq!(now["settings"]["sync_enabled"], false);
    assert_eq!(now["settings"]["maintenance_enabled"], true);
    assert!(now["settings"].get("peers").is_none());
    assert!(
        f.call("config_set_settings", args).is_err(),
        "stale complete settings rejected"
    );
}

#[test]
fn conflicting_workspace_is_not_edited_and_exact_withdrawal_stays_available() {
    let f = Fixture::new();
    let workspace = f.create("Before");
    let id = revision(workspace.as_str().unwrap()).unwrap();
    let anchor = f
        .config
        .node
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .config()
        .unwrap()
        .workspace(id)
        .unwrap()
        .unwrap()
        .anchor;
    f.annotate(
        entity! { metadata::tag:attrs::WORKSPACE, config::anchor:anchor,
        attrs::enrolled:true, metadata::name:"Concurrent" },
    );
    let inspected = f.inspect();
    assert_eq!(inspected["unavailable"].as_array().unwrap().len(), 2);
    assert!(
        f.call(
            "config_edit_workspace",
            json!({"observation":inspected["observation"],"workspace":workspace,"label":"guess"})
        )
        .is_err()
    );
    f.call(
        "config_remove_workspace",
        json!({"observation":inspected["observation"],"workspace":workspace}),
    )
    .unwrap();
    let now = f.inspect();
    assert_eq!(
        now["unavailable"].as_array().unwrap().len(),
        1,
        "withdrawal does not elect another fork"
    );
}

#[test]
fn independent_concurrent_clients_never_share_a_workspace_selection() {
    let f = Fixture::new();
    let a = f.create("A");
    let b = f.create("B");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers = [(a.clone(), "only alpha"), (b.clone(), "only beta")]
        .into_iter()
        .map(|(workspace, title)| {
            let node = f.config.node.clone();
            let binding = f.config.binding.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let dispatch = WorkspaceDispatch::new(binding, move || {
                    node.lock().unwrap().as_mut().unwrap().config()
                })
                .unwrap();
                barrier.wait();
                call(
                    &dispatch,
                    "compass_add",
                    json!({"workspace":workspace,"title":title}),
                )
                .unwrap();
                dispatch.close().unwrap();
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    let a = call(&f.dispatch, "compass_list", json!({"workspace":a})).unwrap();
    let b = call(&f.dispatch, "compass_list", json!({"workspace":b})).unwrap();
    assert!(a.contains("only alpha") && !a.contains("only beta"));
    assert!(b.contains("only beta") && !b.contains("only alpha"));
}

#[test]
fn native_grants_list_by_workspace_anchor_and_enroll_without_bootstrap() {
    use triblespace::core::collection::selection::{Selection, sync_selection};
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let owner_id = hex::encode(owner.config.binding.signer);
    let a = owner.create("Shared research");
    let b = owner.create("Collaboration");
    let original_a = owner.workspace(&a);
    let original_b = owner.workspace(&b);
    let empty = recipient.list_pending();
    assert_eq!(empty["shares"], json!([]));
    assert_eq!(empty["ungrouped"], json!([]));
    assert_eq!(empty["sync_memory_pending"], json!([]));
    owner.share(&a, &recipient, "read_only", None).unwrap();
    owner.share(&b, &recipient, "read_write", Some(true)).unwrap();
    // Native grant exchange lands proofs and fetches their definitions; the
    // descriptors arrive separately through sync or repair.
    owner.repair_authority_to(&recipient, &original_a);
    owner.repair_authority_to(&recipient, &original_b);
    let before = recipient.inspect();
    let pile_before = std::fs::read(&recipient.config.binding.pile).unwrap();
    let pending = recipient.list_pending();
    assert_eq!(pending["shares"], json!([]), "no descriptor, no anchor, no group");
    let ungrouped = pending["ungrouped"].as_array().unwrap();
    assert_eq!(ungrouped.len(), original_a.routes.len() + original_b.routes.len());
    for row in ungrouped {
        assert_eq!(row["descriptor"], "pending");
        assert!(row["name"].is_null() && row["role"].is_null() && row["admitted"].is_null(), "nothing invented: {row}");
        assert_eq!(row["issuers"], json!([&owner_id]));
        assert_eq!(row["claimed"]["read"], true);
        assert_eq!(row["claimed"]["definitions_pending"], 0);
    }
    let roots = |workspace: &Workspace| workspace.routes.values()
        .map(|root| json!(format!("blake3:{}", hex::encode(root.raw)))).collect::<Vec<_>>();
    assert!(ungrouped.iter().filter(|row| roots(&original_a).contains(&row["root"]))
        .all(|row| row["claimed"]["write"] == false && row["claimed"]["delegate"] == false));
    assert!(ungrouped.iter().filter(|row| roots(&original_b).contains(&row["root"]))
        .all(|row| row["claimed"]["write"] == true && row["claimed"]["delegate"] == true));
    assert_eq!(before, recipient.inspect(), "listing has no Config mutation");
    assert_eq!(pile_before, std::fs::read(&recipient.config.binding.pile).unwrap(), "listing imports nothing");
    assert!(recipient.enroll_pending(&original_a.anchor, &root_handles(&original_a), Some("Too early")).is_err(), "descriptor-pending roots carry no anchor to enroll");
    assert_eq!(before, recipient.inspect());
    owner.publish_descriptors_to(&recipient, &original_a);
    owner.publish_descriptors_to(&recipient, &original_b);
    let pending = recipient.list_pending();
    assert_eq!(pending["ungrouped"], json!([]));
    let shares = pending["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 2);
    let share_of = |workspace: &Workspace| shares.iter()
        .find(|share| share["anchor"] == format!("{:X}", workspace.anchor)).unwrap().clone();
    let share_a = share_of(&original_a);
    assert!(share_a["collections"].as_array().unwrap().iter().any(|row|
        row["role"] == "compass" && row["role_source"] == "declared"));
    assert_eq!(share_a["label"], "Shared research");
    assert_eq!(share_a["claimed_access"], "read_only");
    assert_eq!(share_a["claimed_delegate"], false);
    assert_eq!(share_a["admitted_access"], "read_only");
    assert_eq!(share_a["admitted_any"], true);
    assert_eq!(share_a["enrollable"], true);
    assert_eq!(share_a["local_workspace"], "none");
    assert_eq!(share_a["issuers"], json!([&owner_id]));
    let listed: std::collections::BTreeSet<(String, String)> = share_a["collections"].as_array().unwrap().iter().map(|row| {
        assert_eq!(row["descriptor"], "resident");
        assert_eq!(row["role_source"], "declared");
        assert_eq!(row["admitted"], json!({"read":true,"write":false}));
        assert_eq!(row["name"], format!("Shared research · {}", row["role"].as_str().unwrap()));
        (row["role_id"].as_str().unwrap().to_owned(), row["root"].as_str().unwrap().to_owned())
    }).collect();
    let expected: std::collections::BTreeSet<(String, String)> = original_a.routes.iter()
        .map(|(role, root)| (format!("{role:X}"), format!("blake3:{}", hex::encode(root.raw)))).collect();
    assert_eq!(listed, expected, "grouped roots are exactly the shared bundle");
    let share_b = share_of(&original_b);
    assert_eq!(share_b["label"], "Collaboration");
    assert_eq!(share_b["claimed_access"], "read_write");
    assert_eq!(share_b["claimed_delegate"], true);
    assert_eq!(share_b["admitted_access"], "read_write");
    let accepted = recipient.enroll_pending(&original_a.anchor, &recipient.shown_roots(&original_a.anchor), Some("Local research")).unwrap();
    assert_eq!(accepted["already_enrolled"], false);
    assert_eq!(accepted["access"], "read_only");
    assert_eq!(accepted["sync_complete"], false);
    let local_id = &accepted["workspace"]["workspace"];
    let local = recipient.workspace(local_id);
    assert_eq!(local.label, "Local research", "recipient chooses a local alias");
    assert_eq!(local.routes, original_a.routes);
    assert_eq!(local.anchor, original_a.anchor);
    {
        let facts = recipient.config.node.lock().unwrap().as_mut().unwrap().config().unwrap().facts().unwrap();
        assert!(local.routes.values().all(|root| sync_selection(&facts, *root) == Selection::Selected), "enrollment selects the roots for native sync");
    }
    assert!(recipient.admission(&local).iter().all(|value| *value == (true, false)));
    assert_eq!(recipient.inspect()["workspaces"].as_array().unwrap().len(), 1);
    let pending = recipient.list_pending();
    assert_eq!(pending["shares"].as_array().unwrap().len(), 1, "enrolled roots leave the prompt");
    assert_eq!(pending["shares"][0]["anchor"], format!("{:X}", original_b.anchor));
    let proofs = recipient.proofs();
    let repeated = recipient.enroll_pending(&original_a.anchor, &root_handles(&original_a), Some("Local research")).unwrap();
    assert_eq!(repeated["already_enrolled"], true);
    assert_eq!(repeated["workspace"]["workspace"], *local_id);
    assert_eq!(recipient.proofs(), proofs);
    assert!(recipient.enroll_pending(&original_a.anchor, &root_handles(&original_a), Some("Different alias")).is_err(), "repeat cannot silently change a chosen alias");
    assert!(recipient.enroll_pending(&genid().id, &root_handles(&original_a), Some("Unknown")).is_err(), "unknown anchor is refused");
    assert!(recipient.call("config_enroll_pending_share", json!({"observation":recipient.inspect()["observation"],
        "anchor":"not-hex","roots":root_handles(&original_b),"label":"x"})).is_err());
    assert!(recipient.call("config_enroll_pending_share", json!({"observation":"stale",
        "anchor":format!("{:X}", original_b.anchor),"roots":root_handles(&original_b),"label":"x"})).is_err(), "stale observation is refused");
    assert!(recipient.call("config_enroll_pending_share", json!({"observation":recipient.inspect()["observation"],
        "anchor":format!("{:X}", original_b.anchor),"label":"x"})).is_err(), "roots are the consent unit and are required");
    assert_eq!(recipient.inspect()["workspaces"].as_array().unwrap().len(), 1);
    let accepted_b = recipient.enroll_pending(&original_b.anchor, &recipient.shown_roots(&original_b.anchor), Some("Local collaboration")).unwrap();
    assert_eq!(accepted_b["access"], "read_write");
    assert_eq!(accepted_b["delegate"], true);
    let local_b = recipient.workspace(&accepted_b["workspace"]["workspace"]);
    assert_eq!(local_b.routes, original_b.routes);
    assert!(recipient.admission(&local_b).iter().all(|value| *value == (true, true)));
    call(&recipient.dispatch, "compass_add", json!({"workspace":accepted_b["workspace"]["workspace"],"title":"Recipient authored"})).unwrap();
    assert_eq!(recipient.list_pending()["shares"], json!([]));
    assert_eq!(recipient.list_pending()["ungrouped"], json!([]));
}

#[test]
fn native_grants_refuse_withdrawn_anchors_and_require_explicit_routes_without_declarations() {
    use triblespace::core::capability::policy;
    use triblespace::core::collection::{Collection, records};
    use triblespace::core::metadata::MetaDescribe;
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let id = owner.create("Withdrawn later");
    let original = owner.workspace(&id);
    owner.share(&id, &recipient, "read_only", None).unwrap();
    owner.repair_authority_to(&recipient, &original);
    owner.publish_descriptors_to(&recipient, &original);
    let accepted = recipient.enroll_pending(&original.anchor, &recipient.shown_roots(&original.anchor), Some("Local")).unwrap();
    let before = recipient.inspect();
    recipient.call("config_remove_workspace", json!({"observation":before["observation"],
        "workspace":accepted["workspace"]["workspace"]})).unwrap();
    let after = recipient.inspect();
    let pending = recipient.list_pending();
    assert_eq!(pending["shares"].as_array().unwrap().len(), 1, "a withdrawn share stays visible");
    assert_eq!(pending["shares"][0]["local_workspace"], "changed_or_withdrawn");
    assert_eq!(pending["shares"][0]["enrollable"], false);
    assert!(recipient.enroll_pending(&original.anchor, &root_handles(&original), Some("Local")).is_err(), "repeat cannot resurrect a withdrawn workspace");
    assert_eq!(after, recipient.inspect());
    // Descriptors created before roles were declared, and descriptors that
    // name no role at all, are shared the same way. A recognizable suffix
    // is display text, so both need explicit routes.
    let legacy_anchor = genid().id;
    let bare_anchor = genid().id;
    let compass = workspace::roles().into_iter().find(|(_, name)| *name == "compass").unwrap().0;
    let mut guard = owner.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let signer = node.signer.verifying_key();
    let mut register = |anchor: Id, name: &str| -> triblespace::core::collection::CollectionHandle {
        let collection: Collection<blobencodings::SimpleArchive> = node.pile.register_collection(entity! {
            metadata::tag*: [records::KIND_COLLECTION_DESCRIPTOR, anchor],
            records::collection_name: name,
            records::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
            policy::resource_policy*: faculties::collection_names::private_policy(signer).fragment(),
        }).unwrap();
        collection.handle()
    };
    let legacy_root = register(legacy_anchor, "X · archive");
    let bare_root = register(bare_anchor, "Two independent roots");
    let mut legacy = Workspace::new("Legacy");
    legacy.anchor = legacy_anchor;
    legacy.id = legacy_anchor;
    legacy.routes.insert(compass, legacy_root);
    let mut bare = Workspace::new("Bare");
    bare.anchor = bare_anchor;
    bare.id = bare_anchor;
    bare.routes.insert(compass, bare_root);
    let view = node.config().unwrap();
    let legacy_id = node.put_workspace(&view, &legacy).unwrap();
    let view = node.config().unwrap();
    let bare_id = node.put_workspace(&view, &bare).unwrap();
    drop(guard);
    for (revision, workspace) in [(legacy_id, &legacy), (bare_id, &bare)] {
        owner.share(&json!(format!("{revision:X}")), &recipient, "read_only", None).unwrap();
        owner.repair_authority_to(&recipient, workspace);
        owner.publish_descriptors_to(&recipient, workspace);
    }
    let pending = recipient.list_pending();
    let shares = pending["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 3);
    let share_of = |anchor: Id| shares.iter().find(|share| share["anchor"] == format!("{anchor:X}")).unwrap().clone();
    let legacy_share = share_of(legacy_anchor);
    assert_eq!(legacy_share["label"], "X · archive", "no declared role to shorten the label");
    assert_eq!(legacy_share["enrollable"], false);
    assert!(legacy_share["collections"][0]["role"].is_null());
    assert!(legacy_share["collections"][0]["role_source"].is_null());
    let bare_share = share_of(bare_anchor);
    assert_eq!(bare_share["label"], "Two independent roots");
    assert_eq!(bare_share["enrollable"], false);
    assert!(bare_share["collections"][0]["role"].is_null());
    assert!(bare_share["collections"][0]["role_source"].is_null());
    assert!(recipient.enroll_pending(&bare_anchor, &recipient.shown_roots(&bare_anchor), Some("Bare")).is_err(), "a root without an understood role needs explicit routes");
    let before = recipient.inspect();
    let proofs = recipient.proofs();
    assert!(recipient.enroll_pending(&legacy_anchor, &recipient.shown_roots(&legacy_anchor), Some("Legacy local")).is_err(), "a current display-name suffix establishes no role");
    assert_eq!(recipient.inspect(), before);
    let local_id = recipient.call("config_create_workspace", json!({
        "observation":before["observation"], "label":"Chosen locally",
        "routes":[{"role":"compass", "root":format!("blake3:{}", hex::encode(legacy_root.raw))}],
    })).unwrap()["workspace"].clone();
    let local = recipient.workspace(&local_id);
    assert_eq!(local.routes, legacy.routes, "explicit routes may disagree with a display name");
    assert_eq!(recipient.admission(&local), [(true, false)]);
    let edited = recipient.call("config_edit_workspace", json!({
        "observation":recipient.inspect()["observation"], "workspace":local_id,
        "routes":[
            {"role":"compass", "root":format!("blake3:{}", hex::encode(legacy_root.raw))},
            {"role":"wiki", "root":format!("blake3:{}", hex::encode(bare_root.raw))},
        ],
    })).unwrap()["workspace"].clone();
    let wiki = workspace::roles().into_iter().find(|(_, name)| *name == "wiki").unwrap().0;
    let local = recipient.workspace(&edited);
    assert_eq!(local.routes, BTreeMap::from([(compass, legacy_root), (wiki, bare_root)]));
    assert_eq!(recipient.admission(&local), [(true, false), (true, false)]);
    assert_eq!(recipient.proofs(), proofs, "routing never grants permissions");
}

#[test]
fn native_grants_use_only_understood_declared_roles_independently_of_names() {
    use triblespace::core::capability::policy;
    use triblespace::core::collection::{Collection, records};
    use triblespace::core::metadata::MetaDescribe;
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let compass = workspace::roles().into_iter().find(|(_, name)| *name == "compass").unwrap().0;
    let wiki = workspace::roles().into_iter().find(|(_, name)| *name == "wiki").unwrap().0;
    let unknown = genid().id;
    for (name, declared, expected) in [
        ("An arbitrary title", vec![compass], Some("compass")),
        ("X · archive", vec![compass, unknown], Some("compass")),
        ("X · compass", vec![compass, wiki], None),
        ("X · archive", vec![unknown], None),
    ] {
        let anchor = genid().id;
        let mut guard = owner.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let signer = node.signer.verifying_key();
        let collection: Collection<blobencodings::SimpleArchive> = node.pile.register_collection(entity! {
            metadata::tag*: [records::KIND_COLLECTION_DESCRIPTOR, anchor],
            metadata::name: "unrelated metadata",
            records::collection_name: name,
            records::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
            policy::resource_policy*: faculties::collection_names::private_policy(signer).fragment(),
            attrs::collection_role*: declared,
        }).unwrap();
        let mut original = Workspace::new(name);
        original.anchor = anchor;
        original.id = anchor;
        original.routes.insert(compass, collection.handle());
        let view = node.config().unwrap();
        let revision = node.put_workspace(&view, &original).unwrap();
        drop(guard);
        owner.share(&json!(format!("{revision:X}")), &recipient, "read_only", None).unwrap();
        owner.repair_authority_to(&recipient, &original);
        owner.publish_descriptors_to(&recipient, &original);
        let before = recipient.inspect();
        let pending = recipient.list_pending();
        let share = pending["shares"].as_array().unwrap().iter()
            .find(|share| share["anchor"] == format!("{anchor:X}")).unwrap();
        let row = &share["collections"][0];
        assert_eq!(row["name"], name);
        assert_eq!(row["role"], json!(expected), "{row}");
        assert_eq!(row["role_source"], json!(expected.map(|_| "declared")));
        assert_eq!(row["admitted"], json!({"read":true,"write":false}));
        assert_eq!(share["enrollable"], expected.is_some());
        assert_eq!(share["label"], name, "a suffix alone cannot change the role or label");
        assert_eq!(recipient.inspect(), before, "listing changes no binding");
        let accepted = recipient.enroll_pending(&anchor, &recipient.shown_roots(&anchor), Some("Local"));
        if expected.is_some() {
            let local = recipient.workspace(&accepted.unwrap()["workspace"]["workspace"]);
            assert_eq!(local.routes, original.routes);
            assert_eq!(recipient.admission(&local), [(true, false)]);
        } else {
            assert!(accepted.is_err(), "unknown or conflicting roles require explicit routes");
            assert_eq!(recipient.inspect(), before);
        }
    }
}

#[test]
fn native_grants_list_a_descriptor_whose_name_blob_has_not_arrived() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let id = owner.create("Named later");
    let original = owner.workspace(&id);
    owner.share(&id, &recipient, "read_only", None).unwrap();
    owner.repair_authority_to(&recipient, &original);
    // Sync can land a descriptor before the name blob it references. The
    // descriptor carries the anchor and the declared role, so the share is
    // grouped and nothing about the name is invented.
    owner.publish_bare_descriptors_to(&recipient, &original);
    let pending = recipient.list_pending();
    assert_eq!(pending["ungrouped"], json!([]));
    assert_eq!(pending["skipped"], 0);
    let shares = pending["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 1, "a missing name degrades the entry, never the listing");
    let share = &shares[0];
    assert_eq!(share["anchor"], format!("{:X}", original.anchor));
    assert!(share["label"].is_null(), "no name, no invented label: {share}");
    assert_eq!(share["enrollable"], true, "roles are declared on the descriptor");
    assert_eq!(share["collections"].as_array().unwrap().len(), original.routes.len());
    for row in share["collections"].as_array().unwrap() {
        assert_eq!(row["descriptor"], "resident");
        assert!(row["name"].is_null(), "{row}");
        assert_eq!(row["reason"], "name pending");
        assert_eq!(row["role_source"], "declared");
        assert_eq!(row["admitted"], json!({"read":true,"write":false}));
    }
    owner.publish_descriptors_to(&recipient, &original);
    let pending = recipient.list_pending();
    let share = &pending["shares"][0];
    assert_eq!(share["label"], "Named later");
    for row in share["collections"].as_array().unwrap() {
        assert_eq!(row["name"], format!("Named later · {}", row["role"].as_str().unwrap()));
        assert!(row["reason"].is_null(), "{row}");
    }
    let accepted = recipient.enroll_pending(&original.anchor, &recipient.shown_roots(&original.anchor), Some("Local")).unwrap();
    assert_eq!(recipient.workspace(&accepted["workspace"]["workspace"]).routes, original.routes);
}

#[test]
fn native_grants_skip_opaque_resources_and_keep_listing_collection_shares() {
    use triblespace::core::capability::{CapabilityProof, CapabilityResource};
    use triblespace::core::collection::read_capability;
    use triblespace::core::repo::CapabilityProofStore;
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let id = owner.create("Beside opaque grants");
    let original = owner.workspace(&id);
    owner.share(&id, &recipient, "read_only", None).unwrap();
    owner.repair_authority_to(&recipient, &original);
    owner.publish_descriptors_to(&recipient, &original);
    // The capability model also grants opaque resources that are no
    // collection: a resident blob that is not a descriptor, a resident blob
    // that does not decode as facts at all, and a resource this node holds no
    // blob for. None of them is a share; none of them may hide the share.
    let owner_signer = owner.config.node.lock().unwrap().as_mut().unwrap().signer.clone();
    let absent = [0x5A; 32];
    let (opaque, undecodable) = {
        let mut guard = recipient.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let me = node.signer.verifying_key();
        let opaque = node.pile.put::<blobencodings::SimpleArchive, _>(entity! {
            metadata::tag: genid().id, metadata::name: "Delivery resource",
        }.facts().clone()).unwrap();
        let undecodable = node.pile.put::<blobencodings::UTF8String, _>("not a descriptor").unwrap();
        for resource in [opaque.raw, undecodable.raw, absent] {
            node.pile.insert_proof(CapabilityProof::new(
                CapabilityResource::new(resource), &owner_signer, read_capability(), me,
            )).unwrap();
        }
        node.pile.flush().unwrap();
        (opaque, undecodable)
    };
    let pending = recipient.list_pending();
    let shares = pending["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 1, "the collection share is listed beside opaque grants");
    assert_eq!(shares[0]["anchor"], format!("{:X}", original.anchor));
    assert_eq!(shares[0]["label"], "Beside opaque grants");
    assert_eq!(shares[0]["collections"].as_array().unwrap().len(), original.routes.len());
    let ungrouped = pending["ungrouped"].as_array().unwrap();
    assert_eq!(ungrouped.len(), 1, "a non-resident resource is still only a pending handle: {ungrouped:?}");
    assert_eq!(ungrouped[0]["root"], format!("blake3:{}", hex::encode(absent)));
    assert_eq!(ungrouped[0]["descriptor"], "pending");
    assert!(ungrouped[0]["name"].is_null() && ungrouped[0]["role"].is_null());
    assert_eq!(pending["skipped"], 2, "resident non-descriptors are skipped per entry");
    let listed = serde_json::to_string(&pending).unwrap();
    assert!(!listed.contains(&hex::encode(opaque.raw)) && !listed.contains(&hex::encode(undecodable.raw)),
        "skipped resources are not presented as collections");
    let accepted = recipient.enroll_pending(&original.anchor, &recipient.shown_roots(&original.anchor), Some("Local")).unwrap();
    assert_eq!(recipient.workspace(&accepted["workspace"]["workspace"]).routes, original.routes);
}

#[test]
fn native_grants_list_descriptors_with_ambiguous_or_missing_anchor_tags_ungrouped() {
    use triblespace::core::capability::policy;
    use triblespace::core::collection::{Collection, records};
    use triblespace::core::metadata::MetaDescribe;
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let id = owner.create("Grouped");
    let grouped = owner.workspace(&id);
    owner.share(&id, &recipient, "read_only", None).unwrap();
    owner.repair_authority_to(&recipient, &grouped);
    owner.publish_descriptors_to(&recipient, &grouped);
    // Tags are open world. A descriptor carrying an extra tag the reader
    // does not model offers two anchor candidates; one carrying only its
    // kind offers none. Both are listed, ungrouped, with the reason; neither
    // is rejected, and grouping continues for the rest.
    let ambiguous_anchor = genid().id;
    let extra_tag = genid().id;
    let unanchored_anchor = genid().id;
    let compass = workspace::roles().into_iter().find(|(_, name)| *name == "compass").unwrap().0;
    let mut guard = owner.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let signer = node.signer.verifying_key();
    let mut register = |tags: Vec<Id>, name: &str| -> triblespace::core::collection::CollectionHandle {
        let collection: Collection<blobencodings::SimpleArchive> = node.pile.register_collection(entity! {
            metadata::tag*: tags,
            attrs::collection_role: compass,
            records::collection_name: name,
            records::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
            policy::resource_policy*: faculties::collection_names::private_policy(signer).fragment(),
        }).unwrap();
        collection.handle()
    };
    let ambiguous_root = register(vec![records::KIND_COLLECTION_DESCRIPTOR, ambiguous_anchor, extra_tag], "Ambiguous · compass");
    let unanchored_root = register(vec![records::KIND_COLLECTION_DESCRIPTOR], "Unanchored · compass");
    let mut ambiguous = Workspace::new("Ambiguous");
    ambiguous.anchor = ambiguous_anchor;
    ambiguous.id = ambiguous_anchor;
    ambiguous.routes.insert(compass, ambiguous_root);
    let mut unanchored = Workspace::new("Unanchored");
    unanchored.anchor = unanchored_anchor;
    unanchored.id = unanchored_anchor;
    unanchored.routes.insert(compass, unanchored_root);
    let view = node.config().unwrap();
    let ambiguous_id = node.put_workspace(&view, &ambiguous).unwrap();
    let view = node.config().unwrap();
    let unanchored_id = node.put_workspace(&view, &unanchored).unwrap();
    drop(guard);
    for (revision, workspace) in [(ambiguous_id, &ambiguous), (unanchored_id, &unanchored)] {
        owner.share(&json!(format!("{revision:X}")), &recipient, "read_only", None).unwrap();
        owner.repair_authority_to(&recipient, workspace);
        owner.publish_descriptors_to(&recipient, workspace);
    }
    let pending = recipient.list_pending();
    let shares = pending["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 1, "grouping continues for descriptors with one anchor");
    assert_eq!(shares[0]["anchor"], format!("{:X}", grouped.anchor));
    assert_eq!(pending["skipped"], 0, "an unmodelled tag is not a reason to skip");
    let ungrouped = pending["ungrouped"].as_array().unwrap();
    assert_eq!(ungrouped.len(), 2, "{ungrouped:?}");
    let row_of = |root: triblespace::core::collection::CollectionHandle| ungrouped.iter()
        .find(|row| row["root"] == format!("blake3:{}", hex::encode(root.raw))).unwrap().clone();
    let ambiguous_row = row_of(ambiguous_root);
    assert_eq!(ambiguous_row["descriptor"], "resident");
    assert_eq!(ambiguous_row["name"], "Ambiguous · compass");
    assert_eq!(ambiguous_row["role"], "compass");
    assert_eq!(ambiguous_row["role_source"], "declared");
    assert_eq!(ambiguous_row["reason"], "descriptor carries 2 candidate anchors");
    let unanchored_row = row_of(unanchored_root);
    assert_eq!(unanchored_row["descriptor"], "resident");
    assert_eq!(unanchored_row["name"], "Unanchored · compass");
    assert_eq!(unanchored_row["role"], "compass");
    assert_eq!(unanchored_row["role_source"], "declared");
    assert_eq!(unanchored_row["reason"], "descriptor carries no candidate anchor");
    assert!(recipient.enroll_pending(&ambiguous_anchor, &[format!("blake3:{}", hex::encode(ambiguous_root.raw))], Some("Ambiguous")).is_err(), "an ungrouped root needs explicit routes");
    let accepted = recipient.enroll_pending(&grouped.anchor, &recipient.shown_roots(&grouped.anchor), Some("Local")).unwrap();
    assert_eq!(recipient.workspace(&accepted["workspace"]["workspace"]).routes, grouped.routes);
}

#[test]
fn native_grants_enroll_exactly_the_listed_roots_or_refuse_naming_the_difference() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let mut grown = Workspace::new("Grows");
    grown.anchor = genid().id;
    grown.id = grown.anchor;
    let first = owner.grow_workspace(&mut grown, "compass");
    owner.deliver(first, &grown, &recipient);
    let shown = recipient.shown_roots(&grown.anchor);
    assert_eq!(shown, root_handles(&grown));
    // A root arrives under the same anchor between the listing and the
    // enrollment. The consent covered what was shown, not what is there now.
    let second = owner.grow_workspace(&mut grown, "wiki");
    owner.deliver(second, &grown, &recipient);
    let wiki = workspace::roles().into_iter().find(|(_, name)| *name == "wiki").unwrap().0;
    let wiki_root = hex::encode(grown.routes[&wiki].raw);
    let refused = recipient.enroll_pending(&grown.anchor, &shown, Some("Local")).unwrap_err();
    let refused = format!("{refused:#}");
    assert!(refused.contains(&wiki_root), "names the root that arrived: {refused}");
    assert!(refused.contains("config_list_pending_shares"), "{refused}");
    assert_eq!(recipient.inspect()["workspaces"], json!([]), "nothing was widened silently");
    // A root the listing never showed.
    let stray = format!("blake3:{}", hex::encode([0x11; 32]));
    let mut widened = recipient.shown_roots(&grown.anchor);
    widened.push(stray.clone());
    let refused = format!("{:#}", recipient.enroll_pending(&grown.anchor, &widened, Some("Local")).unwrap_err());
    assert!(refused.contains(&stray[7..]), "names the root that is not granted: {refused}");
    assert_eq!(recipient.inspect()["workspaces"], json!([]), "nothing was narrowed silently");
    // Exactly what the listing shows now.
    let accepted = recipient.enroll_pending(&grown.anchor, &recipient.shown_roots(&grown.anchor), Some("Local")).unwrap();
    assert_eq!(accepted["already_enrolled"], false);
    assert_eq!(accepted["extended"], false);
    let local = recipient.workspace(&accepted["workspace"]["workspace"]);
    assert_eq!(local.routes, grown.routes);
    assert_eq!(local.routes.len(), 2);
    assert_eq!(recipient.shown_roots(&grown.anchor), Vec::<String>::new());
}

#[test]
fn native_grants_extend_an_enrolled_workspace_with_roots_that_arrive_later() {
    use triblespace::core::collection::selection::{Selection, sync_selection};
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let mut grown = Workspace::new("Grows");
    grown.anchor = genid().id;
    grown.id = grown.anchor;
    let first = owner.grow_workspace(&mut grown, "compass");
    owner.deliver(first, &grown, &recipient);
    let accepted = recipient.enroll_pending(&grown.anchor, &recipient.shown_roots(&grown.anchor), Some("Local")).unwrap();
    let first_local = accepted["workspace"]["workspace"].clone();
    let compass_routes = grown.routes.clone();
    // A new root under the enrolled anchor is listed as an extension, not as
    // a changed workspace; enrolling exactly it adds a revision.
    let second = owner.grow_workspace(&mut grown, "wiki");
    owner.deliver(second, &grown, &recipient);
    let pending = recipient.list_pending();
    let share = pending["shares"].as_array().unwrap().iter()
        .find(|share| share["anchor"] == format!("{:X}", grown.anchor)).unwrap().clone();
    assert_eq!(share["local_workspace"], "enrolled");
    assert_eq!(share["enrollable"], true);
    let shown = recipient.shown_roots(&grown.anchor);
    assert_eq!(shown.len(), 1, "only the unrouted root is shown: {shown:?}");
    let wiki = workspace::roles().into_iter().find(|(_, name)| *name == "wiki").unwrap().0;
    assert_eq!(shown[0], format!("blake3:{}", hex::encode(grown.routes[&wiki].raw)));
    let extended = recipient.enroll_pending(&grown.anchor, &shown, None).unwrap();
    assert_eq!(extended["extended"], true);
    assert_eq!(extended["already_enrolled"], false);
    let second_local = extended["workspace"]["workspace"].clone();
    assert_ne!(second_local, first_local, "a new revision");
    let local = recipient.workspace(&second_local);
    assert_eq!(local.label, "Local", "label preserved when none is given");
    assert_eq!(local.anchor, grown.anchor);
    assert_eq!(local.routes, grown.routes, "union of routes");
    assert!(compass_routes.iter().all(|(role, root)| local.routes.get(role) == Some(root)));
    assert_eq!(recipient.inspect()["workspaces"].as_array().unwrap().len(), 1, "one workspace, two revisions");
    {
        let facts = recipient.config.node.lock().unwrap().as_mut().unwrap().config().unwrap().facts().unwrap();
        assert_eq!(sync_selection(&facts, grown.routes[&wiki]), Selection::Selected, "the new root is selected");
    }
    assert_eq!(recipient.shown_roots(&grown.anchor), Vec::<String>::new());
    // The identical full set stays idempotent; a different alias is refused.
    let repeated = recipient.enroll_pending(&grown.anchor, &root_handles(&grown), Some("Local")).unwrap();
    assert_eq!(repeated["already_enrolled"], true);
    assert_eq!(repeated["extended"], false);
    assert_eq!(repeated["workspace"]["workspace"], second_local);
    let repeated = recipient.enroll_pending(&grown.anchor, &root_handles(&grown), None).unwrap();
    assert_eq!(repeated["already_enrolled"], true);
    assert!(recipient.enroll_pending(&grown.anchor, &root_handles(&grown), Some("Other")).is_err());
    // A later extension may rename, when a label is given.
    let third = owner.grow_workspace(&mut grown, "files");
    owner.deliver(third, &grown, &recipient);
    let renamed = recipient.enroll_pending(&grown.anchor, &recipient.shown_roots(&grown.anchor), Some("Renamed")).unwrap();
    assert_eq!(renamed["extended"], true);
    let local = recipient.workspace(&renamed["workspace"]["workspace"]);
    assert_eq!(local.label, "Renamed");
    assert_eq!(local.routes, grown.routes);
    assert_eq!(local.routes.len(), 3);
}

#[test]
fn native_grants_report_claimed_access_apart_from_admission() {
    use ed25519_dalek::SigningKey;
    use triblespace::core::capability::{CapabilityProof, CapabilityResource, capability_action, capability_delegate_action};
    use triblespace::core::collection::{ACTION_READ, ACTION_WRITE};
    use triblespace::core::repo::CapabilityProofStore;
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let id = owner.create("Claimed");
    let original = owner.workspace(&id);
    owner.share(&id, &recipient, "read_only", None).unwrap();
    owner.repair_authority_to(&recipient, &original);
    owner.publish_descriptors_to(&recipient, &original);
    // A descriptor the recipient is not admitted to at all: delivered, never
    // shared by its owner.
    let mut unadmitted = Workspace::new("Unadmitted");
    unadmitted.anchor = genid().id;
    unadmitted.id = unadmitted.anchor;
    owner.grow_workspace(&mut unadmitted, "compass");
    owner.publish_descriptors_to(&recipient, &unadmitted);
    // A stranger's proofs claim READ, WRITE and delegation over every root.
    // The edges say read_write; no descriptor policy admits that chain.
    let stranger = SigningKey::from_bytes(&[0x77; 32]);
    {
        let mut guard = recipient.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let me = node.signer.verifying_key();
        let definition = node.pile.put::<blobencodings::SimpleArchive, _>(entity! {
            capability_action*: [ACTION_READ, ACTION_WRITE], capability_delegate_action: ACTION_WRITE,
        }.facts().clone()).unwrap();
        for root in original.routes.values().chain(unadmitted.routes.values()) {
            node.pile.insert_proof(CapabilityProof::new(
                CapabilityResource::new(root.raw), &stranger, definition, me,
            )).unwrap();
        }
        node.pile.flush().unwrap();
    }
    let pending = recipient.list_pending();
    let shares = pending["shares"].as_array().unwrap();
    let share_of = |anchor: Id| shares.iter().find(|share| share["anchor"] == format!("{anchor:X}")).unwrap().clone();
    let claimed = share_of(original.anchor);
    assert_eq!(claimed["claimed_access"], "read_write", "what the edges say");
    assert_eq!(claimed["claimed_delegate"], true);
    assert_eq!(claimed["admitted_access"], "read_only", "what admission grants");
    assert_eq!(claimed["admitted_any"], true);
    assert!(claimed["issuers"].as_array().unwrap().contains(&json!(hex::encode(stranger.verifying_key().to_bytes()))));
    for row in claimed["collections"].as_array().unwrap() {
        assert_eq!(row["claimed"]["write"], true);
        assert_eq!(row["claimed"]["delegate"], true);
        assert_eq!(row["admitted"], json!({"read":true,"write":false}), "{row}");
    }
    let none = share_of(unadmitted.anchor);
    assert_eq!(none["claimed_access"], "read_write");
    assert_eq!(none["admitted_access"], "none");
    assert_eq!(none["admitted_any"], false);
    for row in none["collections"].as_array().unwrap() {
        assert_ne!(row["admitted"]["read"], true, "{row}");
        assert_ne!(row["admitted"]["write"], true, "{row}");
    }
}

#[test]
fn native_grants_route_only_the_consented_roots_and_disclose_roots_routed_elsewhere() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let mut shared = Workspace::new("Shared");
    shared.anchor = genid().id;
    shared.id = shared.anchor;
    owner.grow_workspace(&mut shared, "compass");
    let revision = owner.grow_workspace(&mut shared, "wiki");
    owner.deliver(revision, &shared, &recipient);
    let role = |name: &str| workspace::roles().into_iter().find(|(_, role)| *role == name).unwrap().0;
    let (compass, wiki) = (role("compass"), role("wiki"));
    let (c1, c2) = (shared.routes[&compass], shared.routes[&wiki]);
    let hex_of = |root: CollectionHandle| format!("blake3:{}", hex::encode(root.raw));
    // The recipient's own workspace B already routes C1.
    let b = recipient.put_local_workspace("B", BTreeMap::from([(compass, c1)]));
    let pending = recipient.list_pending();
    let share = pending["shares"].as_array().unwrap().iter()
        .find(|share| share["anchor"] == format!("{:X}", shared.anchor)).unwrap().clone();
    assert_eq!(share["local_workspace"], "none");
    let rows = share["collections"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "a root routed elsewhere is disclosed, not dropped: {rows:?}");
    let row_of = |root: CollectionHandle| rows.iter().find(|row| row["root"] == hex_of(root)).unwrap().clone();
    assert_eq!(row_of(c1)["routed_in"], json!([format!("{b:X}")]));
    assert!(row_of(c1)["reason"].as_str().unwrap().contains("routed in local workspace"), "{}", row_of(c1));
    assert_eq!(row_of(c2)["routed_in"], json!([]));
    assert!(row_of(c2)["reason"].is_null());
    // Consent to C2 alone publishes exactly C2.
    let accepted = recipient.enroll_pending(&shared.anchor, &[hex_of(c2)], Some("Local")).unwrap();
    assert_eq!(accepted["extended"], false);
    let local = recipient.workspace(&accepted["workspace"]["workspace"]);
    assert_eq!(local.routes, BTreeMap::from([(wiki, c2)]), "no route the user never saw");
    assert_eq!(recipient.inspect()["workspaces"].as_array().unwrap().len(), 2);
    // C1 stays disclosed beside the enrolled workspace; repeating the
    // consented root is idempotent.
    assert_eq!(recipient.shown_roots(&shared.anchor), vec![hex_of(c1)]);
    let pending = recipient.list_pending();
    let share = pending["shares"].as_array().unwrap().iter()
        .find(|share| share["anchor"] == format!("{:X}", shared.anchor)).unwrap().clone();
    assert_eq!(share["local_workspace"], "enrolled");
    let repeated = recipient.enroll_pending(&shared.anchor, &[hex_of(c2)], None).unwrap();
    assert_eq!(repeated["already_enrolled"], true);
    assert_eq!(repeated["workspace"]["workspace"], accepted["workspace"]["workspace"]);
    // C1 given explicitly extends to the union; B is untouched.
    let extended = recipient.enroll_pending(&shared.anchor, &[hex_of(c1)], None).unwrap();
    assert_eq!(extended["extended"], true);
    let local = recipient.workspace(&extended["workspace"]["workspace"]);
    assert_eq!(local.routes, shared.routes);
    assert_eq!(local.label, "Local");
    assert_eq!(recipient.workspace(&json!(format!("{b:X}"))).routes, BTreeMap::from([(compass, c1)]));
    assert_eq!(recipient.shown_roots(&shared.anchor), Vec::<String>::new());
}

#[test]
fn native_grants_treat_an_undecodable_definition_as_unavailable_for_that_edge() {
    use ed25519_dalek::SigningKey;
    use triblespace::core::capability::{CapabilityProof, CapabilityResource};
    use triblespace::core::repo::CapabilityProofStore;
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let id = owner.create("Defined");
    let original = owner.workspace(&id);
    owner.share(&id, &recipient, "read_only", None).unwrap();
    owner.repair_authority_to(&recipient, &original);
    owner.publish_descriptors_to(&recipient, &original);
    // A valid proof whose resident definition blob is no SimpleArchive at
    // all, naming this node for a shared root and for a resource this node
    // holds no blob for.
    let stranger = SigningKey::from_bytes(&[0x66; 32]);
    let absent = [0x33; 32];
    {
        let mut guard = recipient.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let me = node.signer.verifying_key();
        let odd = node.pile.put::<blobencodings::UTF8String, _>("not a definition").unwrap();
        for resource in original.routes.values().map(|root| root.raw).chain([absent]) {
            node.pile.insert_proof(CapabilityProof::new(
                CapabilityResource::new(resource), &stranger, odd.transmute(), me,
            )).unwrap();
        }
        node.pile.flush().unwrap();
    }
    let pending = recipient.list_pending();
    let shares = pending["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 1, "the share lists beside the odd proof");
    assert_eq!(shares[0]["anchor"], format!("{:X}", original.anchor));
    assert_eq!(shares[0]["claimed_access"], "read_only");
    assert_eq!(shares[0]["admitted_access"], "read_only");
    for row in shares[0]["collections"].as_array().unwrap() {
        assert_eq!(row["claimed"]["read"], true);
        assert_eq!(row["claimed"]["write"], false);
        assert_eq!(row["claimed"]["definitions_pending"], 1, "{row}");
        assert!(row["reason"].as_str().unwrap().contains("definition"), "{row}");
        assert_eq!(row["admitted"], json!({"read":true,"write":false}));
    }
    let ungrouped = pending["ungrouped"].as_array().unwrap();
    assert_eq!(ungrouped.len(), 1);
    assert_eq!(ungrouped[0]["root"], format!("blake3:{}", hex::encode(absent)));
    assert_eq!(ungrouped[0]["claimed"]["definitions_pending"], 1);
    assert_eq!(pending["skipped"], 0);
    let accepted = recipient.enroll_pending(&original.anchor, &recipient.shown_roots(&original.anchor), Some("Local")).unwrap();
    assert_eq!(recipient.workspace(&accepted["workspace"]["workspace"]).routes, original.routes);
}

#[test]
fn native_grants_refuse_an_empty_roots_list_instead_of_publishing_an_empty_workspace() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let mut shared = Workspace::new("Shared");
    shared.anchor = genid().id;
    shared.id = shared.anchor;
    let revision = owner.grow_workspace(&mut shared, "compass");
    owner.deliver(revision, &shared, &recipient);
    let compass = workspace::roles().into_iter().find(|(_, role)| *role == "compass").unwrap().0;
    let c1 = shared.routes[&compass];
    let hex_of = |root: CollectionHandle| format!("blake3:{}", hex::encode(root.raw));
    // The recipient's own workspace B routes every root of the group, so the
    // listing shows C1 only as routed elsewhere and nothing is required.
    let b = recipient.put_local_workspace("B", BTreeMap::from([(compass, c1)]));
    let pending = recipient.list_pending();
    let share = pending["shares"].as_array().unwrap().iter()
        .find(|share| share["anchor"] == format!("{:X}", shared.anchor)).unwrap().clone();
    let rows = share["collections"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["routed_in"], json!([format!("{b:X}")]));
    let proofs = recipient.proofs();
    // An empty consent is no consent: refused, naming the listing, before
    // anything is published.
    let outcome = recipient.enroll_pending(&shared.anchor, &[], Some("Local"));
    assert!(outcome.is_err(), "an empty roots list published a workspace: {outcome:?}");
    let refused = format!("{:#}", outcome.unwrap_err());
    assert!(refused.contains("nothing to enroll"), "{refused}");
    assert!(refused.contains("config_list_pending_shares"), "{refused}");
    let workspaces = recipient.inspect()["workspaces"].as_array().unwrap().clone();
    assert_eq!(workspaces.len(), 1, "no empty-route workspace was published: {workspaces:?}");
    assert_eq!(workspaces[0]["workspace"], format!("{b:X}"));
    assert_eq!(recipient.proofs(), proofs);
    // Giving the routed root explicitly still enrolls it, as before.
    let accepted = recipient.enroll_pending(&shared.anchor, &[hex_of(c1)], Some("Local")).unwrap();
    assert_eq!(accepted["already_enrolled"], false);
    let local = recipient.workspace(&accepted["workspace"]["workspace"]);
    assert_eq!(local.routes, BTreeMap::from([(compass, c1)]));
    assert_eq!(recipient.inspect()["workspaces"].as_array().unwrap().len(), 2);
    // An empty list is refused for an enrolled workspace too: it is not the
    // idempotent repeat, which names the routed set.
    let refused = format!("{:#}", recipient.enroll_pending(&shared.anchor, &[], None).unwrap_err());
    assert!(refused.contains("nothing to enroll"), "{refused}");
    assert_eq!(recipient.inspect()["workspaces"].as_array().unwrap().len(), 2);
    let repeated = recipient.enroll_pending(&shared.anchor, &[hex_of(c1)], None).unwrap();
    assert_eq!(repeated["already_enrolled"], true);
    assert_eq!(repeated["workspace"]["workspace"], accepted["workspace"]["workspace"]);
}


#[test]
fn sharing_catalog_and_result_use_only_native_grants() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let workspace = owner.create("Native sharing");
    let names = owner.config.tools().iter().map(|tool| tool.name).collect::<Vec<_>>();
    assert!(names.contains(&"config_share_workspace"));
    assert!(names.contains(&"config_list_pending_shares"));
    assert!(names.contains(&"config_enroll_pending_share"));
    assert!(!names.contains(&"config_probe_workspace_share"));
    assert!(!names.contains(&"config_enroll_workspace_share"));
    let tool = owner.config.tools().iter().find(|tool| tool.name == "config_share_workspace").unwrap();
    let schema: Value = serde_json::from_str(tool.input_schema).unwrap();
    let properties = schema["properties"].as_object().unwrap().keys().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(properties, BTreeSet::from(["observation", "workspace", "recipient", "access", "delegate"]));
    let result = owner.share(&workspace, &recipient, "read_only", None).unwrap();
    let fields = result.as_object().unwrap().keys().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(fields, BTreeSet::from(["workspace", "recipient", "delegate", "issuer_routing_hint", "publication", "sync_complete", "status", "collections"]));
    assert_eq!(result["publication"], "local_native_grants");
    assert_eq!(result["sync_complete"], false);
    assert_eq!(recipient.inspect()["workspaces"], json!([]), "grant publishing never enrolls another node");
}

#[test]
fn native_health_reporting_uses_only_the_explicit_host_destination() {
    use crate::swarm_health::Reporter;
    use triblespace::core::clock::mono_now;
    use triblespace::core::collection::{CollectionSnapshotExt, grant_collection_read, grant_collection_write};
    use triblespace_net::{health::{HealthSnapshot, StoreHealth}, health_record as health};

    let f = Fixture::new();
    let node_guard = || f.config.node.lock().unwrap();
    let foreign = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
    let (shared, workspace, signer) = {
        let mut guard = node_guard();
        let node = guard.as_mut().unwrap();
        let signer = node.signer.verifying_key();
        let shared = node.pile.collection("shared fixture", faculties::collection_names::private_policy(foreign.verifying_key())).unwrap();
        let workspace = node.pile.collection("workspace fixture", faculties::collection_names::private_policy(signer)).unwrap();
        grant_collection_read(&mut node.pile, shared.handle(), &foreign, signer).unwrap();
        (shared, workspace, signer)
    };
    f.put_local_workspace("workspace with health", [(health::DEFAULT_SCOPE_ID, workspace.handle())].into());
    let selected = || {
        let mut guard = node_guard();
        let snapshot = guard.as_mut().unwrap().pile.snapshot().unwrap();
        crate::workspace::selected_sync_roots(&snapshot, signer).unwrap()
    };
    let initially_selected = selected();
    let mut reporter = Reporter::default();
    let mut publish = |sample: Option<&HealthSnapshot>| {
        reporter.publish(node_guard().as_mut().unwrap(), sample)
    };
    let read = |collection| {
        node_guard().as_mut().unwrap().pile.snapshot().unwrap()
            .collection_acquiring(collection).unwrap().view::<TribleSet>().unwrap()
    };
    let configure = |root: Value| f.call("config_set_health_reporting",
        json!({"observation":f.inspect()["observation"],"root":root}));
    let root = json!(format!("blake3:{}", hex::encode(shared.handle().raw)));

    let record_count = || node_guard().as_mut().unwrap().pile.snapshot().unwrap().records().unwrap().count();
    assert!(f.inspect()["health_reporting"]["root"].is_null());
    let before = record_count();
    publish(None).unwrap();
    assert_eq!(record_count(), before);
    assert!(read(workspace).is_empty());
    assert!(configure(root.clone()).unwrap_err().to_string().contains("WRITE"));
    assert!(configure(json!(format!("blake3:{}", "ff".repeat(32)))).is_err());
    assert!(f.call("config_set_health_reporting", json!({"observation":f.inspect()["observation"]})).is_err());
    assert!(read(shared).is_empty());

    // Bypass the setter to exercise publication's own admission check.
    f.annotate(entity! { config::anchor: attrs::HEALTH_REPORTING, config::selects: shared.handle() });
    let before = record_count();
    assert!(publish(None).unwrap_err().to_string().contains("WRITE"));
    assert_eq!(record_count(), before, "an unauthorized append would be invisible in the admitted view");
    {
        let mut guard = node_guard();
        grant_collection_write(&mut guard.as_mut().unwrap().pile, shared.handle(), &foreign, signer).unwrap();
    }
    configure(Value::Null).unwrap();
    let stale = f.inspect()["observation"].clone();
    assert_eq!(configure(root.clone()).unwrap()["root"], root);
    let configured = f.inspect()["observation"].clone();
    configure(root.clone()).unwrap();
    assert_eq!(f.inspect()["observation"], configured);
    assert!(f.call("config_set_health_reporting", json!({"observation":stale,"root":null})).is_err());
    assert_eq!(f.inspect()["health_reporting"]["root"], root);

    publish(None).unwrap();
    let startup = read(shared);
    assert!(exists!(pattern!(&startup, [
        { _?report @ metadata::tag: &health::KIND_REPORT, health::attrs::condition: _?condition,
            health::attrs::node: _?node },
        { _?node @ health::attrs::endpoint: signer },
        { _?condition @ metadata::tag: &health::HOST, health::attrs::state: &health::UNKNOWN },
    ])));
    let now = mono_now();
    let observed = HealthSnapshot {
        node: iroh_base::PublicKey::from_bytes(signer.as_bytes()).unwrap(),
        started_at: Some(now), observed_at: Some(now),
        store: StoreHealth { resident_blobs: 31, serving_snapshot: true, last_snapshot_observed_at: Some(now), ..Default::default() },
        publication: Default::default(), blob_serving: Default::default(),
        collections: vec![], peerings: vec![], grants: vec![], named_unselected: vec![],
    };
    publish(Some(&observed)).unwrap();
    let facts = read(shared);
    assert!(exists!(pattern!(&facts, [
        { _?startup @ health::attrs::session: _?session, health::attrs::condition: _?unknown },
        { _?unknown @ metadata::tag: &health::HOST, health::attrs::state: &health::UNKNOWN },
        { _?report @ health::attrs::session: _?session, health::attrs::condition: _?store },
        { _?store @ metadata::tag: &health::STORE, health::attrs::resident_blobs: 31u64 },
    ])));
    assert!(read(workspace).is_empty());
    assert_eq!(selected(), initially_selected);

    // A concurrent disable cannot be hidden by another head selecting a root.
    f.annotate(entity! { config::anchor: attrs::HEALTH_REPORTING });
    assert!(f.inspect()["health_reporting"]["problem"].is_string());
    assert!(publish(Some(&observed)).is_err());
    assert!(configure(Value::Null).unwrap()["root"].is_null());
    publish(Some(&observed)).unwrap();
    assert_eq!(read(shared), facts);
    assert!(read(workspace).is_empty());
}

#[test]
fn native_health_reporting_rejects_a_writable_derived_collection() {
    use triblespace::core::capability::policy::resource_policy;
    use triblespace::core::collection::{
        Collection, KIND_COLLECTION_DESCRIPTOR, KIND_COLLECTION_MAPPING, collection_mapping,
        collection_representation, collection_source, mapping_algorithm,
    };
    use triblespace::core::metadata::MetaDescribe;

    let f = Fixture::new();
    let derived: Collection<blobencodings::SimpleArchive> = {
        let mut guard = f.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let algorithm = genid();
        node.pile.register_collection(entity! {
            metadata::tag: KIND_COLLECTION_DESCRIPTOR,
            collection_source: node.metrics.handle(),
            resource_policy*: faculties::collection_names::private_policy(node.signer.verifying_key()).fragment(),
            collection_representation*: blobencodings::SimpleArchive::describe(),
            collection_mapping*: entity! { metadata::tag: KIND_COLLECTION_MAPPING, mapping_algorithm: &algorithm },
        }).unwrap()
    };
    let inspected = f.inspect();
    let error = f.call("config_set_health_reporting", json!({
        "observation":inspected["observation"],"root":format!("blake3:{}",hex::encode(derived.handle().raw))
    })).unwrap_err();
    assert!(error.to_string().contains("source collection"));
    assert_eq!(inspected["observation"], f.inspect()["observation"]);
    let error = f.call("config_set_sync_telemetry", json!({
        "observation":inspected["observation"],
        "destination":{"root":format!("blake3:{}",hex::encode(derived.handle().raw)),"worker":"fixture"}
    })).unwrap_err();
    assert!(error.to_string().contains("source collection"));
    assert_eq!(inspected["observation"], f.inspect()["observation"]);
}

#[test]
fn native_sync_telemetry_uses_the_configured_pair_and_preserves_its_session() {
    use crate::sync_telemetry::Reporter;
    use std::time::Duration;
    use triblespace::core::{clock::epoch_now, collection::CollectionSnapshotExt};
    use triblespace_net::{dashboard::CountMetric, health::HealthSnapshot, reconcile::ReconcileStats, telemetry};

    let f = Fixture::new();
    let guard = || f.config.node.lock().unwrap();
    let (destination, other, signer) = {
        let mut guard = guard();
        let node = guard.as_mut().unwrap();
        let signer = node.signer.verifying_key();
        let policy = || faculties::collection_names::private_policy(signer);
        (node.pile.collection("destination", policy()).unwrap(),
         node.pile.collection("other", policy()).unwrap(), signer)
    };
    let health = HealthSnapshot {
        node: iroh_base::EndpointId::from_bytes(signer.as_bytes()).unwrap(),
        started_at: None, observed_at: None, store: Default::default(),
        collections: vec![], publication: Default::default(), blob_serving: Default::default(),
        peerings: vec![], grants: vec![], named_unselected: vec![],
    };
    let read = |collection| {
        let snapshot = guard().as_mut().unwrap().pile.snapshot().unwrap();
        let facts = snapshot.collection(collection).unwrap().view::<TribleSet>().unwrap();
        let now = epoch_now().to_tai_duration().total_nanoseconds();
        telemetry::observe(&facts, &snapshot, now, Duration::from_secs(180))
    };
    let configure = |target: Value| f.call("config_set_sync_telemetry", json!({
        "observation":f.inspect()["observation"], "destination":target,
    }));
    let choice = |root: CollectionHandle, worker: &str| json!({
        "root":format!("blake3:{}",hex::encode(root.raw)), "worker":worker,
    });
    let mut reporter = Reporter::default();
    let publish = |r: &mut Reporter| r.publish(guard().as_mut().unwrap(), &health);
    let count = || guard().as_mut().unwrap().pile.snapshot().unwrap().records().unwrap().count();

    assert_eq!(f.inspect()["sync_telemetry"], json!({"destination":null}));
    let before = count();
    publish(&mut reporter).unwrap();
    assert!(reporter.publisher().is_none());
    assert_eq!(before, count());
    let target = choice(destination.handle(), "app-sync");
    let stale = f.inspect()["observation"].clone();
    assert_eq!(configure(target.clone()).unwrap()["destination"], target);
    let observed = f.inspect()["observation"].clone();
    configure(target.clone()).unwrap();
    assert_eq!(observed, f.inspect()["observation"]);
    assert!(f.call("config_set_sync_telemetry", json!({"observation":stale,"destination":null})).is_err());
    assert!(configure(choice(destination.handle(), "bad\nlabel")).is_err());
    assert!(configure(json!({"root":target["root"]})).is_err());

    publish(&mut reporter).unwrap();
    reporter.publisher().unwrap().reconciled(
        &ReconcileStats { landed: 7, received_bytes: 321, pending_blobs: Some(2), ..Default::default() },
        Duration::from_millis(3),
    );
    publish(&mut reporter).unwrap();
    let reports = read(destination);
    let roles: BTreeSet<_> = reports.iter().map(|r| r.role.as_str()).collect();
    assert_eq!(roles, BTreeSet::from(["hydration","serve","repair","publication","process"]));
    assert_eq!(reports.len(), 5);
    assert!(reports.iter().all(|r| r.node == signer.to_bytes() && r.worker == "app-sync" && r.session == reports[0].session));
    let hydration = reports.iter().find(|r| r.role == "hydration").unwrap();
    for (metric, value) in [(telemetry::Metric::Completed, 7), (telemetry::Metric::ReceivedBytes, 321),
                            (telemetry::Metric::Queued, 2), (telemetry::Metric::WorkNs, 3_000_000)] {
        assert_eq!(hydration.metrics.iter().find(|m| m.metric == metric).unwrap().value, CountMetric::Value(value));
    }
    assert!(read(other).is_empty());

    configure(choice(destination.handle(), "renamed-worker")).unwrap();
    publish(&mut reporter).unwrap();
    let after_rename = read(destination);
    let renamed = after_rename.iter().find(|r| r.worker == "renamed-worker" && r.role == "hydration").unwrap();
    assert_ne!(renamed.session, hydration.session);
    assert_eq!(renamed.metrics.iter().find(|m| m.metric == telemetry::Metric::Completed).unwrap().value, CountMetric::Value(0));
    assert_eq!(after_rename.len(), 10);

    // Competing complete proposals never select a root from one and a worker from the other.
    f.annotate(entity! {config::anchor:attrs::SYNC_TELEMETRY,
        config::selects:other.handle(), telemetry::attrs::worker:"other-worker"});
    assert!(f.inspect()["sync_telemetry"]["problem"].is_string());
    let before = count();
    assert!(publish(&mut reporter).is_err());
    assert!(reporter.publisher().is_none());
    assert_eq!(count(), before);
    configure(choice(other.handle(), "other-worker")).unwrap();
    publish(&mut reporter).unwrap();
    let next = read(other);
    assert_eq!(next.len(), 5);
    assert!(next.iter().all(|r| r.worker == "other-worker" && r.session != hydration.session));
    let next_hydration = next.iter().find(|r| r.role == "hydration").unwrap();
    assert_eq!(next_hydration.metrics.iter().find(|m| m.metric == telemetry::Metric::Completed).unwrap().value, CountMetric::Value(0));
    assert_eq!(read(destination).iter().map(|r| r.report).collect::<BTreeSet<_>>(),
               after_rename.iter().map(|r| r.report).collect::<BTreeSet<_>>());

    configure(Value::Null).unwrap();
    let before = count();
    publish(&mut reporter).unwrap();
    assert!(reporter.publisher().is_none());
    assert_eq!(count(), before);
}

#[test]
fn native_sync_telemetry_requires_effective_write_before_any_append() {
    use crate::sync_telemetry::Reporter;
    use triblespace::core::collection::{grant_collection_read, grant_collection_write};
    use triblespace_net::{health::HealthSnapshot, telemetry};

    let f = Fixture::new();
    let foreign = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
    let (root, signer) = {
        let mut guard = f.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let signer = node.signer.verifying_key();
        let root = node.pile.collection("readable", faculties::collection_names::private_policy(foreign.verifying_key())).unwrap().handle();
        grant_collection_read(&mut node.pile, root, &foreign, signer).unwrap();
        (root, signer)
    };
    let config = f.inspect();
    let result = f.call("config_set_sync_telemetry", json!({"observation":config["observation"],
        "destination":{"root":format!("blake3:{}", hex::encode(root.raw)), "worker":"controlled"}}));
    assert!(result.unwrap_err().to_string().contains("not admitted"));
    assert_eq!(config["observation"], f.inspect()["observation"]);
    f.annotate(entity! {config::anchor:attrs::SYNC_TELEMETRY,
        config::selects:root, telemetry::attrs::worker:"controlled"});
    let mut guard = f.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let before = node.pile.snapshot().unwrap().records().unwrap().count();
    let health = HealthSnapshot {
        node: iroh_base::EndpointId::from_bytes(signer.as_bytes()).unwrap(),
        started_at: None, observed_at: None, store: Default::default(),
        collections: vec![], publication: Default::default(), blob_serving: Default::default(),
        peerings: vec![], grants: vec![], named_unselected: vec![],
    };
    let mut reporter = Reporter::default();
    assert!(reporter.publish(node, &health).is_err());
    assert_eq!(before, node.pile.snapshot().unwrap().records().unwrap().count());
    grant_collection_write(&mut node.pile, root, &foreign, signer).unwrap();
    reporter.publish(node, &health).unwrap();
    assert!(reporter.publisher().is_some());
}
