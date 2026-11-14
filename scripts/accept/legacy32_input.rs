//! Export unsigned payload coordinates from this helper's fresh synthetic app
//! fixture. The independently built historical writer owns old signatures.
use serde_json::json;
use std::{fs, path::Path};
use triblespace::core::{
    collection::CollectionRecord,
    repo::pile::{PileRecordContent, PileRecords},
};

pub fn export(root: &Path, pile: &Path, key: &Path, public: &str, destination: &Path) {
    let input = root.join("legacy32-input");
    fs::create_dir(&input).unwrap();
    let mut records = PileRecords::open(pile).unwrap();
    let mut blobs = std::collections::BTreeSet::new();
    let mut commits = Vec::new();
    while let Some(record) = records.next() {
        match record.unwrap().content {
            PileRecordContent::Blob {
                hash,
                data_offset,
                data_len,
                ..
            } => {
                let handle = hex::encode(hash.raw);
                fs::write(
                    input.join(format!("{handle}.blob")),
                    &records.bytes()[data_offset..data_offset + data_len],
                )
                .unwrap();
                blobs.insert(handle);
            }
            PileRecordContent::Collection {
                record: CollectionRecord::Commit(commit),
            } => {
                assert_eq!(hex::encode(commit.public_key().raw), public);
                commits.push(json!({"collection":hex::encode(commit.collection().raw),
                    "data":hex::encode(commit.data().raw),"metadata":hex::encode(commit.metadata().raw)}));
            }
            PileRecordContent::Want { .. } => {} // Local demand is not fixture authority.
            other => panic!("unexpected record in freshly generated app fixture: {other:?}"),
        }
    }
    assert!(!commits.is_empty());
    fs::write(
        input.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "fixture":"erlkonig-synthetic-config-workspace-v1", "node_id":public,
            "key":key.strip_prefix(root).unwrap(),
            "destination":destination.strip_prefix(root).unwrap(),
            "blobs":blobs,"commits":commits,
        }))
        .unwrap(),
    )
    .unwrap();
}
