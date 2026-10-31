//! Copy this example into an isolated historical triblespace-core checkout at
//! 3ca5c1cb5504d8ff60730edfe5bf641b01f44913. Do not compile it against the
//! converter under test. Historical PileFile and CollectionCommit own every
//! old frame and signature; this code only supplies synthetic app payloads.
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use triblespace_core::{
    blob::{Blob, Bytes, encodings::UnknownBlob},
    collection::{
        CollectionStore,
        records::{CollectionCommit, CollectionRecord},
    },
    inline::Inline,
    repo::{
        BlobStorePut,
        pile::{PileFile, PileRecordContent, PileRecords},
    },
    signing_key_file,
};

fn handle(value: &Value) -> [u8; 32] {
    hex::decode(value.as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

fn child(root: &Path, value: &Value) -> PathBuf {
    let path = Path::new(value.as_str().unwrap());
    assert!(
        !path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
    );
    root.join(path)
}

fn main() {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: legacy32_writer SYNTHETIC_FIXTURE_ROOT"),
    );
    let root = root.canonicalize().unwrap();
    let input = root.join("legacy32-input");
    let manifest: Value =
        serde_json::from_slice(&fs::read(input.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["fixture"],
        "erlkonig-synthetic-config-workspace-v1"
    );
    let key = child(&root, &manifest["key"]);
    assert!(key.canonicalize().unwrap().starts_with(&root));
    let signer = signing_key_file::load_existing(&key).unwrap();
    assert_eq!(
        hex::encode(signer.verifying_key().to_bytes()),
        manifest["node_id"].as_str().unwrap()
    );
    let destination = child(&root, &manifest["destination"]);
    assert!(
        destination
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .starts_with(&root)
    );
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&destination)
        .unwrap();
    let mut pile = PileFile::open(&destination).unwrap();
    pile.publish_record_kind_descriptions().unwrap();
    let blobs = manifest["blobs"].as_array().unwrap();
    for item in blobs {
        let expected = handle(item);
        let payload = fs::read(input.join(format!("{}.blob", hex::encode(expected)))).unwrap();
        let stored = pile
            .put::<UnknownBlob, _>(Blob::<UnknownBlob>::new(Bytes::from_source(payload)))
            .unwrap();
        assert_eq!(stored.raw, expected);
    }
    let commits = manifest["commits"].as_array().unwrap();
    assert!(!commits.is_empty());
    for commit in commits {
        let record = CollectionRecord::Commit(CollectionCommit::sign(
            &signer,
            Inline::new(handle(&commit["collection"])),
            Inline::new(handle(&commit["data"])),
            Inline::new(handle(&commit["metadata"])),
        ));
        record.verify_strict().unwrap();
        pile.insert(record).unwrap();
    }
    pile.close().unwrap();
    let serialized = fs::read(&destination).unwrap();
    let mut count = 0;
    for frame in PileRecords::open(&destination).unwrap() {
        let frame = frame.unwrap();
        if let PileRecordContent::Collection { record } = frame.content {
            // Assert the actual historical pile envelope, not the distinct
            // dense record representation returned by CollectionRecord::to_bytes.
            assert_eq!(
                hex::encode(&serialized[frame.offset + 32..frame.offset + 64]),
                "a1322bb3f5214287c314d42afcc1a97cb264facd9a22b4938838be78db31aa59",
                "this example must run against the independent historical writer"
            );
            record.verify_strict().unwrap();
            count += 1;
        }
    }
    assert_eq!(count, commits.len());
    println!(
        "{}",
        json!({"historical_core":"3ca5c1cb5504d8ff60730edfe5bf641b01f44913",
        "pile":destination,"blobs":blobs.len(),"signed_commits":count,"node_id":manifest["node_id"],
        "blake3":blake3::hash(&serialized).to_hex().as_str()})
    );
}
