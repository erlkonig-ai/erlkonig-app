//! Host linkage for the same screen model and reader scheduler the phone uses.
#![allow(dead_code)]
use std::path::{Path, PathBuf};
enum ModelSource {
    Live(PathBuf),
    Error(String),
    #[cfg(feature = "synthetic-proof")]
    Visual(Vec<String>),
    #[cfg(feature = "synthetic-proof")]
    Proof {
        reader: std::sync::mpsc::Receiver<Result<PathBuf, String>>,
    },
}
fn volume_capacity(_: &Path) -> Result<(u64, u64), String> {
    unreachable!("the test leaves device capacity unrequested")
}
#[cfg(feature = "synthetic-proof")]
#[path = "fixture.rs"]
mod fixture;
#[path = "model.rs"]
mod model;
