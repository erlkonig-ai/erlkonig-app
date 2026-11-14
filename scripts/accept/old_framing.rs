//! Acceptance data from the existing public, synthetic v0.46.4 core fixture.
//! This exercises framing upgrade, not the separately authored /32 cutover.
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};
use triblespace::core::repo::pile::{PileRecordContent, PileRecords};

pub fn released_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../triblespace-rs/trible/tests/fixtures/legacy_v0464_branch.pile")
}

pub fn append_released_blob(destination: &Path) {
    let mut records = PileRecords::open(&released_fixture()).expect("public released fixture");
    while let Some(record) = records.next() {
        let record = record.expect("released record");
        if matches!(record.content, PileRecordContent::Blob { .. }) {
            assert!(!records.has_current_framing(&record));
            let mut file = OpenOptions::new().append(true).open(destination).unwrap();
            file.write_all(&records.bytes()[record.offset..record.offset + record.len])
                .unwrap();
            file.sync_all().unwrap();
            return;
        }
    }
    panic!("released fixture has no blob");
}
