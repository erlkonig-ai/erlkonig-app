//! Run native snapshot invariants with the retained app dependency lock.
use std::collections::BTreeSet;
use triblespace_net::storage_snapshot::*;
use triblespace_core::{
    collection::{
        CollectionHandle, HeldBlobs, HeldRead,
        record_counts::RecordCountStatus, record_patch::collection_record_patch,
    },
    repo::BlobStoreList,
};
type Raw = [u8; 32];
#[path = "../../../../upstream/triblespace-rs/triblespace-net/src/storage_snapshot/tests.rs"]
mod native;
