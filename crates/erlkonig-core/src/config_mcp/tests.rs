use super::*;
use crate::{
    config::Bootstrap, init, node_store::NodeStore, workspace_dispatch::WorkspaceDispatch,
};
use faculties::out::Part;
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

    fn enroll(&self, invitation: &Value) -> Result<Value> {
        self.call("config_enroll_workspace_share", json!({"observation":self.inspect()["observation"],
            "bootstrap":invitation["bootstrap"],"label":"Local shared workspace"}))
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
        let mut selected = crate::workspace::AllAccessible::discover(&node.pile.snapshot().unwrap(), node.signer.verifying_key(), roots).unwrap();
        selected.sync.extend(node.enrolled_roots().unwrap());
        selected.sync.sort_unstable(); selected.sync.dedup();
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
            match evidence.authorized_readers() {
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
    call(&owner.dispatch, "compass_add", json!({"workspace":id,"title":"Shared evidence"})).unwrap();
    let before = owner.inspect();
    let invitation = owner.share(&id, &recipient, "read_only", None).unwrap();
    let key = triblespace::core::signing_key_file::load_existing(&owner.config.binding.key).unwrap();
    let encoded = invitation.to_string();
    assert!(!encoded.contains(&hex::encode(key.to_bytes())));
    assert!(!encoded.contains(owner.config.binding.key.to_str().unwrap()));
    assert!(!encoded.contains(owner.config.binding.pile.to_str().unwrap()));
    assert_eq!(invitation["delegate"], false);
    assert_eq!(invitation["sync_complete"], false);
    assert_eq!(before["observation"], owner.inspect()["observation"], "inviting does not edit Config");
    let owner_proofs = owner.proofs();
    assert_eq!(owner.share(&id, &recipient, "read_only", None).unwrap(), invitation);
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
    let before_probe = recipient.inspect();
    let pile_before = std::fs::read(&recipient.config.binding.pile).unwrap();
    let probe = recipient.call("config_probe_workspace_share", json!({"bootstrap":invitation["bootstrap"]})).unwrap();
    assert_eq!(probe["source_label"], "Shared research");
    assert_eq!(probe["source_workspace"], id);
    assert_eq!(probe["access"], "read_only");
    assert_eq!(probe["metadata_verified"], true);
    assert_eq!(probe["native_admission"], "not_confirmed_by_probe");
    assert_eq!(probe["collections"].as_array().unwrap().len(), original.routes.len());
    assert!(probe["collections"].as_array().unwrap().iter().any(|row|
        row["role"] == "compass" && row["names"] == json!(["Shared research · compass"])));
    assert_eq!(before_probe, recipient.inspect(), "probe has no Config mutation");
    assert_eq!(pile_before, std::fs::read(&recipient.config.binding.pile).unwrap(), "probe imports nothing");
    let wire: Value = serde_json::from_str(invitation["bootstrap"].as_str().unwrap()).unwrap();
    assert!(wire.get("proofs").is_none(), "handoff never carries proof bytes");
    for value in wire["blobs"].as_array().unwrap() {
        use triblespace::core::blob::{Blob, Bytes, TryFromBlob};
        let blob = Blob::<blobencodings::SimpleArchive>::new(Bytes::from_source(hex::decode(value["bytes"].as_str().unwrap()).unwrap()));
        if let Ok(facts) = TribleSet::try_from_blob(blob) {
            assert!(!exists!(pattern!(&facts, [{ triblespace::core::capability::capability_action: _?action }])), "handoff never carries capability definition bodies");
        }
    }
    assert_eq!(recipient.inspect()["workspaces"], json!([]));
    let unselected = recipient.selection();
    for root in original.routes.values() {
        assert!(!unselected.sync.contains(root), "off-store probe cannot select Full hydration");
        assert!(!unselected.maintenance.contains(root), "off-store probe cannot select Driver upkeep");
    }
    let accepted = recipient.enroll(&invitation).unwrap();
    assert_eq!(accepted["access"], "read_only");
    assert_eq!(accepted["delegate"], false);
    assert_eq!(accepted["sync_complete"], false);
    let local_id = &accepted["workspace"]["workspace"];
    let local = recipient.workspace(local_id);
    assert_eq!(local.label, "Local shared workspace", "recipient chooses a local alias");
    assert_eq!(local.routes, original.routes);
    let selected = recipient.selection();
    assert!(local.routes.values().all(|root| selected.sync.contains(root) && !selected.maintenance.contains(root)), "local consent selects pending known-C bootstrap, not maintenance authority");
    assert!(recipient.admission(&local).iter().all(|value| *value == (false, false)), "metadata/enrollment cannot manufacture admission");
    assert_eq!(recipient.proofs(), 0);
    owner.repair_authority_to(&recipient, &original);
    assert!(recipient.selection().maintenance.iter().any(|root| local.routes.values().any(|selected| selected == root)));
    assert!(recipient.admission(&local).iter().all(|value| *value == (true, false)));
    // Enrollment imports descriptors/names, not authority or member data.
    assert!(!call(&recipient.dispatch, "compass_list", json!({"workspace":local_id})).unwrap().contains("Shared evidence"));
    owner.replicate_to(&recipient, &original);
    assert!(call(&recipient.dispatch, "compass_list", json!({"workspace":local_id})).unwrap().contains("Shared evidence"));
    assert!(call(&recipient.dispatch, "compass_add", json!({"workspace":local_id,"title":"Denied write"})).is_err());
    let proof_count = recipient.proofs();
    assert!(recipient.share(local_id, &third, "read_only", None).is_err());
    assert_eq!(recipient.proofs(), proof_count, "failed delegation publishes no proof");
    let repeated = recipient.enroll(&invitation).unwrap();
    assert_eq!(repeated["already_enrolled"], true);
    assert_eq!(repeated["workspace"]["workspace"], *local_id);
    assert_eq!(recipient.proofs(), proof_count);
    assert!(recipient.call("config_enroll_workspace_share", json!({"observation":recipient.inspect()["observation"],
        "bootstrap":invitation["bootstrap"],"label":"Different alias"})).is_err(), "repeat cannot silently change a chosen alias");
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
        "mcp_port":0,"sync_enabled":false,"maintenance_enabled":false,"peers":[]})).unwrap()["settings"].clone();
    let runtime = recipient.inspect()["runtime"].clone();
    let key_bytes = std::fs::read(&recipient.config.binding.key).unwrap();
    let id = owner.create("Collaboration");
    let original = owner.workspace(&id);
    let invoke_accept = recipient.enroll(&owner.share(&invoke_only, &recipient, "read_write", None).unwrap()).unwrap();
    let invoke_id = &invoke_accept["workspace"]["workspace"];
    owner.repair_authority_to(&recipient, &owner.workspace(&invoke_only));
    assert!(recipient.admission(&recipient.workspace(invoke_id)).iter().all(|value| *value == (true, true)));
    assert!(recipient.share(invoke_id, &third, "read_only", None).is_err(), "WRITE invocation is not delegation");
    let accepted = recipient.enroll(&owner.share(&id, &recipient, "read_write", Some(true)).unwrap()).unwrap();
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
    let onward = recipient.share(local_id, &third, "read_only", Some(true)).unwrap();
    let accepted_third = third.enroll(&onward).unwrap();
    let third_id = &accepted_third["workspace"]["workspace"];
    recipient.repair_authority_to(&third, &local);
    assert!(third.admission(&third.workspace(third_id)).iter().all(|value| *value == (true, false)));
    assert!(third.share(third_id, &fourth, "read_write", Some(true)).is_err(), "cannot broaden actions");
    let final_invite = third.share(third_id, &fourth, "read_only", None).unwrap();
    let final_accept = fourth.enroll(&final_invite).unwrap();
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
fn sharing_rejects_wrong_recipient_malformed_and_tampered_without_mutation() {
    let owner = Fixture::new();
    let recipient = Fixture::new();
    let wrong = Fixture::new();
    let id = owner.create("Integrity");
    let invitation = owner.share(&id, &recipient, "read_only", None).unwrap();
    let wrong_before = wrong.inspect();
    let wrong_proofs = wrong.proofs();
    assert!(wrong.enroll(&invitation).is_err());
    assert_eq!(wrong_before, wrong.inspect());
    assert_eq!(wrong.proofs(), wrong_proofs);
    let before = recipient.inspect();
    let proofs = recipient.proofs();
    let mut malformed = invitation.clone();
    malformed["bootstrap"] = json!("{not JSON");
    assert!(recipient.enroll(&malformed).is_err());
    let original: Value = serde_json::from_str(invitation["bootstrap"].as_str().unwrap()).unwrap();
    for part in ["commit", "blob"] {
        let mut transport = original.clone();
        let value = match part {
            "commit" => &mut transport["commit"],
            _ => &mut transport["blobs"][0]["bytes"],
        };
        let mut bytes = hex::decode(value.as_str().unwrap()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        *value = json!(hex::encode(bytes));
        let mut tampered = invitation.clone();
        tampered["bootstrap"] = json!(serde_json::to_string(&transport).unwrap());
        assert!(recipient.enroll(&tampered).is_err(), "reject tampered {part}");
        assert_eq!(before, recipient.inspect());
        assert_eq!(recipient.proofs(), proofs);
    }
    let mut forbidden = original;
    forbidden["proofs"] = json!([]);
    let mut malformed = invitation.clone();
    malformed["bootstrap"] = json!(serde_json::to_string(&forbidden).unwrap());
    assert!(recipient.enroll(&malformed).is_err(), "proof-carrier transport is not a supported metadata handoff");
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
    let invitation = owner.share(&id, &recipient, "read_only", None).unwrap();
    let mut replacement = owner.workspace(&extra);
    replacement.anchor = original.anchor;
    let mut guard = owner.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let view = node.config().unwrap();
    node.put_workspace(&view, &replacement).unwrap();
    drop(guard);
    assert!(owner.share(&id, &recipient, "read_only", None).is_err(), "old revision cannot issue fresh invitation");
    let accepted = recipient.enroll(&invitation).unwrap();
    let local_id = &accepted["workspace"]["workspace"];
    assert_eq!(recipient.workspace(local_id).routes, original.routes, "previously issued invitation is frozen, never future-following");
    let before = recipient.inspect();
    let key_before = std::fs::read(&recipient.config.binding.key).unwrap();
    recipient.call("config_remove_workspace", json!({"observation":before["observation"],"workspace":local_id})).unwrap();
    let after = recipient.inspect();
    assert!(recipient.enroll(&invitation).is_err(), "repeat cannot resurrect withdrawn workspace");
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
    let invitation = owner.share(&id, &recipient, "read_only", None).unwrap();
    let accepted = recipient.enroll(&invitation).unwrap();
    let local = recipient.workspace(&accepted["workspace"]["workspace"]);
    owner.repair_authority_to(&recipient, &value);
    assert_eq!(local.routes, value.routes);
    assert_eq!(recipient.admission(&local), [(true, false)]);
    let mut guard = recipient.config.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let snapshot = node.pile.snapshot().unwrap();
    assert_eq!(snapshot.proofs().unwrap().filter(|proof| proof.as_ref().unwrap().leaf_key() == node.signer.verifying_key()).count(),
        2, "two recipient-bound rooted paths; native AUTH also retains the issuer's parent proof");
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
    args["peers"] = json!([hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&[45; 32])
            .verifying_key()
            .to_bytes()
    )]);
    let saved = f.call("config_set_settings", args.clone()).unwrap();
    assert_eq!(saved["mcp_url"], "http://127.0.0.1:41773/");
    assert_eq!(saved["port_restart_required"], true);
    let now = f.inspect();
    assert!(now["settings_problem"].is_null());
    assert_eq!(now["settings"]["sync_enabled"], false);
    assert_eq!(now["settings"]["maintenance_enabled"], true);
    assert_eq!(now["settings"]["peers"], args["peers"]);
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
