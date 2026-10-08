use super::*;
use ed25519_dalek::SigningKey;
use triblespace::core::collection::{AdmissionPolicy, CollectionPolicy, CollectionStoreExt};
use triblespace::core::metadata;
use triblespace::core::repo::StorageFlush;

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
    let managed = ManagedPile::new("Fixture", path.clone(), key.clone());
    let discovery = Discovery::open(&managed).unwrap();
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
    assert!(discovery.select(wiki, descriptor_only.handle()).is_err());
    assert!(
        discovery
            .select(wiki, CollectionHandle::new([95; 32]))
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&key).unwrap(), key_before);
    assert!(!managed.sync_enabled && !managed.maintenance_enabled);
}
