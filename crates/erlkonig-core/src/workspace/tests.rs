use super::*;
use ed25519_dalek::SigningKey;
use triblespace::core::collection::{AdmissionPolicy, CollectionPolicy, CollectionStoreExt};
use triblespace::core::metadata;
use triblespace::core::repo::StorageFlush;

#[test]
fn public_roles_match_the_supported_faculty_table() {
    let public = faculties::collection_names::table();
    assert_eq!(roles(), public);
    assert!(!public.iter().any(|(_, name)| *name == "erp"));
    for (role, name) in public {
        assert_eq!(role_name(role), Some(name));
    }
    assert_eq!(role_name(genid().id), None);
}

#[test]
fn saved_empty_bundle_roots_are_discoverable_in_another_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let bootstrap = crate::config::Bootstrap::new(dir.path());
    let signer = crate::init::prepare(&bootstrap).unwrap();
    let binding = NodeBinding {
        pile: bootstrap.pile.clone(),
        key: bootstrap.key,
        signer: signer.verifying_key().to_bytes(),
    };
    let mut node = crate::node_store::NodeStore::open(&bootstrap.pile, signer).unwrap();
    let expected = node.config().unwrap();
    let revision = node.create_workspace(&expected, "Empty bundle").unwrap();
    let routes = node
        .config()
        .unwrap()
        .workspace(revision)
        .unwrap()
        .unwrap()
        .routes;
    let before = std::fs::read(&bootstrap.pile).unwrap();
    let discovery = Discovery::open(&binding).unwrap();
    let (roots, unavailable) = discovery.roots().unwrap();
    assert!(unavailable.is_empty());
    for handle in routes.values() {
        assert!(
            roots
                .iter()
                .any(|root| root.handle == *handle && root.read && root.write)
        );
    }
    assert_eq!(std::fs::read(&bootstrap.pile).unwrap(), before);
    drop(discovery);
    node.close().unwrap();
}

#[test]
fn pending_enrollment_adds_bootstrap_without_replacing_legacy_readable_roots() {
    use triblespace::core::repo::SnapshotSource;
    let dir = tempfile::tempdir().unwrap();
    let bootstrap = crate::config::Bootstrap::new(dir.path());
    let signer = crate::init::prepare(&bootstrap).unwrap();
    let mut node = crate::node_store::NodeStore::open(&bootstrap.pile, signer.clone()).unwrap();
    let legacy = node.pile.collection("unmapped readable graph", faculties::collection_names::private_policy(signer.verifying_key())).unwrap();
    node.pile.commit(legacy, &signer, entity! { metadata::name: "legacy data" }).unwrap();
    let stranger = SigningKey::from_bytes(&[74; 32]);
    let pending = node.pile.collection("known pending root", faculties::collection_names::private_policy(stranger.verifying_key())).unwrap();
    let mut workspace = Workspace::new("Local consent");
    workspace.routes.insert(roles()[0].0, pending.handle());
    let view = node.config().unwrap();
    let revision = node.put_workspace(&view, &workspace).unwrap();
    let seeds = node.workspace_roots().unwrap();
    let before = AllAccessible::discover(&node.pile.snapshot().unwrap(), signer.verifying_key(), seeds).unwrap();
    assert!(before.sync.contains(&legacy.handle()), "legacy unmapped READ behavior is preserved");
    assert!(!before.sync.contains(&pending.handle()));
    assert!(!before.maintenance.contains(&pending.handle()));
    assert_eq!(node.enrolled_roots().unwrap(), [pending.handle()], "current Config selects pending bootstrap without manufacturing authority");
    let view = node.config().unwrap();
    node.remove_workspace_revision(&view, revision).unwrap();
    assert!(node.enrolled_roots().unwrap().is_empty());
    let seeds = node.workspace_roots().unwrap();
    assert!(AllAccessible::discover(&node.pile.snapshot().unwrap(), signer.verifying_key(), seeds).unwrap().sync.contains(&legacy.handle()));
    node.close().unwrap();
}

#[test]
fn local_discovery_preserves_bytes_distinguishes_same_names_and_supports_exact_probe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.pile");
    let key = dir.path().join("data.key");
    std::fs::File::create(&path).unwrap();
    let signer = signing_key_file::init(&key).unwrap();
    let stranger = SigningKey::from_bytes(&[84; 32]);
    let mut pile = faculties::storage::open_pile_strict_as(&path, signer.verifying_key()).unwrap();
    let own = pile
        .collection(
            "wiki",
            faculties::collection_names::private_policy(signer.verifying_key()),
        )
        .unwrap();
    let read_only = pile
        .collection(
            "wiki",
            CollectionPolicy::new(
                AdmissionPolicy::Open,
                AdmissionPolicy::direct(stranger.verifying_key()),
            ),
        )
        .unwrap();
    let denied = pile
        .collection(
            "wiki",
            faculties::collection_names::private_policy(stranger.verifying_key()),
        )
        .unwrap();
    let descriptor_only = pile
        .collection(
            "files",
            faculties::collection_names::private_policy(signer.verifying_key()),
        )
        .unwrap();
    for (root, author) in [(own, &signer), (read_only, &stranger), (denied, &stranger)] {
        pile.commit(root, author, entity! { metadata::name: "fixture" })
            .unwrap();
    }
    pile.flush().unwrap();
    pile.close().unwrap();
    let before = std::fs::read(&path).unwrap();
    let key_before = std::fs::read(&key).unwrap();
    let binding = NodeBinding {
        pile: path.clone(),
        key: key.clone(),
        signer: signer.verifying_key().to_bytes(),
    };
    let discovery = Discovery::open(&binding).unwrap();
    let (roots, unavailable) = discovery.roots().unwrap();
    assert!(unavailable.is_empty(), "{unavailable:?}");
    assert_eq!(roots.len(), 3);
    assert!(roots.iter().all(|root| root.name == "wiki"));
    assert!(
        roots
            .iter()
            .any(|root| root.handle == own.handle() && root.read && root.write)
    );
    assert!(
        roots
            .iter()
            .any(|root| root.handle == read_only.handle() && root.read && !root.write)
    );
    assert!(
        roots
            .iter()
            .any(|root| root.handle == denied.handle() && !root.read && !root.write)
    );
    assert_eq!(
        discovery.probe(descriptor_only.handle()).unwrap().name,
        "files"
    );
    let wiki = faculties::schemas::wiki::DEFAULT_SCOPE_ID;
    assert_eq!(
        discovery.select(wiki, own.handle()).unwrap().handle,
        own.handle()
    );
    assert_eq!(
        discovery.select(wiki, read_only.handle()).unwrap().handle,
        read_only.handle()
    );
    assert!(discovery.select(wiki, denied.handle()).is_err());
    assert!(discovery.select(wiki, descriptor_only.handle()).is_ok());
    assert!(discovery.select(genid().id, own.handle()).is_err());
    assert!(
        discovery
            .select(wiki, CollectionHandle::new([95; 32]))
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&key).unwrap(), key_before);
}

#[test]
fn all_accessible_uses_actual_read_and_revisits_new_record_and_proof_resources() {
    use triblespace::core::collection::{CollectionRead, grant_collection_read};
    use triblespace::core::repo::SnapshotSource;
    let host = SigningKey::from_bytes(&[11; 32]);
    let owner = SigningKey::from_bytes(&[12; 32]);
    let mut store = MemoryRepo::for_host(host.verifying_key());
    let first = store
        .collection(
            "root",
            faculties::collection_names::private_policy(host.verifying_key()),
        )
        .unwrap();
    let invited = store
        .collection(
            "invitation only",
            faculties::collection_names::private_policy(owner.verifying_key()),
        )
        .unwrap();
    let denied = store
        .collection(
            "denied",
            faculties::collection_names::private_policy(owner.verifying_key()),
        )
        .unwrap();
    store
        .commit(first, &host, entity! {metadata::name:"first"})
        .unwrap();
    store
        .commit(denied, &owner, entity! {metadata::name:"denied"})
        .unwrap();
    let old = store.snapshot().unwrap();
    let before = AllAccessible::discover(&old, host.verifying_key(), []).unwrap();
    assert_eq!(before.sync, vec![first.handle()]);
    assert_eq!(before.denied, vec![denied.handle()]);
    grant_collection_read(&mut store, invited.handle(), &owner, host.verifying_key()).unwrap();
    let next = store.snapshot().unwrap();
    assert!(
        !next.collections().unwrap().contains(&invited.handle()),
        "proof resource has no collection record"
    );
    let after = AllAccessible::discover(&next, host.verifying_key(), []).unwrap();
    assert!(after.sync.contains(&invited.handle()));
    assert!(
        !AllAccessible::discover(&old, host.verifying_key(), [])
            .unwrap()
            .sync
            .contains(&invited.handle())
    );
    let later = store
        .collection(
            "later",
            faculties::collection_names::private_policy(host.verifying_key()),
        )
        .unwrap();
    store
        .commit(later, &host, entity! {metadata::name:"later"})
        .unwrap();
    assert!(
        AllAccessible::discover(&store.snapshot().unwrap(), host.verifying_key(), [])
            .unwrap()
            .sync
            .contains(&later.handle())
    );
}

#[test]
fn invitation_without_descriptor_or_definition_is_pending_not_permission() {
    use triblespace::core::capability::{CapabilityHandle, CapabilityProof, CapabilityResource};
    use triblespace::core::repo::{CapabilityProofStore, SnapshotSource};
    let host = SigningKey::from_bytes(&[21; 32]);
    let owner = SigningKey::from_bytes(&[22; 32]);
    let mut store = MemoryRepo::for_host(host.verifying_key());
    let absent = CollectionHandle::new([23; 32]);
    store
        .insert_proof(CapabilityProof::new(
            CapabilityResource::from(absent),
            &owner,
            CapabilityHandle::new([24; 32]),
            host.verifying_key(),
        ))
        .unwrap();
    let descriptor = store
        .collection(
            "incomplete proof",
            faculties::collection_names::private_policy(owner.verifying_key()),
        )
        .unwrap();
    store
        .insert_proof(CapabilityProof::new(
            CapabilityResource::from(descriptor.handle()),
            &owner,
            CapabilityHandle::new([25; 32]),
            host.verifying_key(),
        ))
        .unwrap();
    let selection =
        AllAccessible::discover(&store.snapshot().unwrap(), host.verifying_key(), []).unwrap();
    assert!(selection.sync.is_empty());
    assert!(selection.denied.is_empty());
    assert_eq!(selection.pending.len(), 2);
}

#[test]
fn local_attachments_follow_readable_parent_without_policy_or_write_gate() {
    use triblespace::core::blob::encodings::succinctarchive::SuccinctArchiveBlob;
    use triblespace::core::repo::SnapshotSource;
    let host = SigningKey::from_bytes(&[31; 32]);
    let owner = SigningKey::from_bytes(&[32; 32]);
    let mut store = MemoryRepo::for_host(host.verifying_key());
    let root = store
        .collection(
            "read only root",
            CollectionPolicy::new(
                AdmissionPolicy::Open,
                AdmissionPolicy::direct(owner.verifying_key()),
            ),
        )
        .unwrap();
    store
        .commit(root, &owner, entity! {metadata::name:"row"})
        .unwrap();
    let attached = store.attach::<SuccinctArchiveBlob>(root, ()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(store.maintain_attached(attached, &host))
        .unwrap();
    let selection =
        AllAccessible::discover(&store.snapshot().unwrap(), host.verifying_key(), []).unwrap();
    assert_eq!(selection.sync, vec![root.handle()]);
    assert!(selection.maintenance.contains(&root.handle()));
    assert!(selection.maintenance.contains(&attached.handle()));
}

#[test]
fn derived_roots_are_read_selected_and_unknown_mapping_does_not_stop_supported_dependencies() {
    use trible::maintenance::{Driver, Event, SuccinctBackend, Target};
    use triblespace::core::blob::encodings::succinctarchive::SuccinctArchiveBlob;
    use triblespace::core::capability::policy;
    use triblespace::core::collection::{CollectionSnapshotExt, records};
    use triblespace::core::metadata::MetaDescribe;
    use triblespace::core::repo::BlobStorePut;
    let host = SigningKey::from_bytes(&[41; 32]);
    let mut store = MemoryRepo::for_host(host.verifying_key());
    let root = store
        .collection(
            "parent",
            faculties::collection_names::private_policy(host.verifying_key()),
        )
        .unwrap();
    store
        .commit(root, &host, entity! {metadata::name:"row"})
        .unwrap();
    let attachment = store.attach::<SuccinctArchiveBlob>(root, ()).unwrap();
    // A descriptor with an explicitly unsupported mapping is still an
    // authorized derived collection, not an attachment or false denial.
    let descriptor = entity! {
        metadata::tag: records::KIND_COLLECTION_DESCRIPTOR,
        records::collection_source: root.handle(),
        policy::resource_policy*: faculties::collection_names::private_policy(host.verifying_key()).fragment(),
        records::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
        records::collection_mapping*: entity!{ records::mapping_algorithm: genid().id },
    };
    let derived = store
        .put::<blobencodings::SimpleArchive, _>(descriptor.facts().clone())
        .unwrap();
    let selection = AllAccessible::discover(
        &store.snapshot().unwrap(),
        host.verifying_key(),
        [derived, attachment.handle()],
    )
    .unwrap();
    assert!(
        selection.sync.contains(&derived),
        "derived is not excluded as a host-local attachment"
    );
    assert!(!selection.sync.contains(&attachment.handle()));
    let mut driver = Driver::new(
        [Target::Handle(derived), Target::Handle(attachment.handle())],
        true,
        SuccinctBackend::Cpu,
    )
    .unwrap();
    let mut order = Vec::new();
    let mut errors = Vec::new();
    let mut observer = |event| match event {
        Event::HopStarted { collection } => order.push(collection),
        Event::HopFinished {
            error: Some(error), ..
        }
        | Event::Failure { error, .. } => errors.push(error),
        _ => {}
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let pass = runtime
        .block_on(driver.tick(&mut store, &host, &mut observer))
        .unwrap()
        .unwrap();
    assert_eq!(pass.failures, 1);
    assert!(!errors.is_empty(), "unsupported mapping remains visible");
    assert_eq!(
        order.first(),
        Some(&root.handle()),
        "upstream before derived or attached target"
    );
    assert_eq!(
        order
            .iter()
            .filter(|handle| **handle == root.handle())
            .count(),
        1
    );
    assert_eq!(
        store
            .snapshot()
            .unwrap()
            .attached(attachment)
            .unwrap()
            .cover()
            .len(),
        1,
        "supported attachment still advances"
    );
}
