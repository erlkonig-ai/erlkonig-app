//! Narrow access to ERP piles and their durable signing identity.
//!
//! This is intentionally local rather than a dependency on the complete
//! Faculties office suite. Its behavior is the same small seam used there:
//! explicit signer initialization, strict signer loading, and strict pile
//! opening without implicit tail repair.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ed25519_dalek::SigningKey;
use triblespace::core::repo::pile::{Pile, ReadError};
use triblespace::core::signing_key_file;

/// Resolve the durable signer path without touching the filesystem.
pub fn signer_path(pile: &Path, explicit: Option<&Path>) -> PathBuf {
    signing_key_file::resolve_path(explicit, pile)
}

/// Load an existing durable signer without ever creating one.
pub fn load_signer(pile: &Path, explicit: Option<&Path>) -> Result<SigningKey> {
    let path = signer_path(pile, explicit);
    signing_key_file::load_existing(&path)
        .with_context(|| format!("load durable signing key {}", path.display()))
}

/// Explicitly initialize a durable signer, or load the valid concurrent winner.
pub fn initialize_signer(pile: &Path, explicit: Option<&Path>) -> Result<SigningKey> {
    let path = signer_path(pile, explicit);
    signing_key_file::init(&path)
        .with_context(|| format!("initialize durable signing key {}", path.display()))
}

/// Open and refresh an existing pile without automatic repair.
pub fn open_pile_strict(path: &Path) -> Result<Pile> {
    let mut pile = Pile::open(path).with_context(|| format!("open pile {}", path.display()))?;
    if let Err(error) = pile.refresh() {
        let close = pile.close();
        let mut failure = read_error(path, error);
        if let Err(close_error) = close {
            failure = failure.context(format!(
                "closing pile after failed refresh also failed: {close_error}"
            ));
        }
        return Err(failure);
    }
    Ok(pile)
}

fn read_error(path: &Path, error: ReadError) -> anyhow::Error {
    match error {
        ReadError::CorruptPile { valid_length } => anyhow!(
            "pile {} is corrupt at byte {valid_length}; refusing to auto-repair. \
             If and only if the tail is a genuinely torn write, repair it \
             explicitly with `trible pile amputate {}`",
            path.display(),
            path.display(),
        ),
        other => anyhow!("refresh pile {}: {other}", path.display()),
    }
}

/// Parse the ordinary faculty exact-descriptor syntax: 64 hex digits, with
/// optional `blake3:` prefix. Never derive a collection identity from a name.
pub fn parse_collection(
    text: &str,
) -> Result<triblespace::core::collection::records::CollectionHandle> {
    use triblespace::prelude::Inline;
    let text = text.trim().strip_prefix("blake3:").unwrap_or(text.trim());
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!(
            "collection must be one exact 64-hex descriptor handle, optionally prefixed blake3:"
        );
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)?;
    }
    Ok(Inline::new(bytes))
}

/// An exact route for non-CLI callers such as the read-only examples. Missing
/// or invalid configuration is an error, never a private-root fallback.
pub fn configured_collection() -> Result<triblespace::core::collection::records::CollectionHandle> {
    let text = std::env::var("TRIBLESPACE_COLLECTION_ERP")
        .context("set TRIBLESPACE_COLLECTION_ERP to an exact existing ERP descriptor")?;
    parse_collection(&text)
}

/// Open the supplied native ERP descriptor in this same immutable observation.
/// The configured role binding selects the descriptor; its human name is not
/// authority. There is no registration, migration, rebind, or name fallback.
pub fn open_collection(
    snapshot: &triblespace::core::repo::pile::PileSnapshot,
    handle: triblespace::core::collection::records::CollectionHandle,
    signer: &SigningKey,
    write: bool,
) -> Result<
    triblespace::core::collection::Collection<
        triblespace::core::blob::encodings::simplearchive::SimpleArchive,
    >,
> {
    use triblespace::core::blob::encodings::simplearchive::SimpleArchive;
    use triblespace::core::collection::Collection;
    let collection =
        Collection::<SimpleArchive>::open(snapshot, handle).context("open exact ERP descriptor")?;
    if !collection.reader_is_admitted(snapshot, signer.verifying_key())? {
        anyhow::bail!("durable signer is not admitted to READ the supplied ERP collection");
    }
    if write && !collection.writer_is_admitted(snapshot, signer.verifying_key())? {
        anyhow::bail!("durable signer is not admitted to WRITE the supplied ERP collection");
    }
    Ok(collection)
}

/// Explicit creation boundary used only by `init` and isolated test fixtures.
/// Returns the exact descriptor that subsequent operations must supply.
pub fn initialize_collection(
    pile_path: &Path,
    key_path: Option<&Path>,
) -> Result<triblespace::core::collection::records::CollectionHandle> {
    use triblespace::core::collection::{policy::private_policy, CollectionStoreExt};
    let signer = load_signer(pile_path, key_path)?;
    let mut pile = open_pile_strict(pile_path)?;
    let result = pile
        .collection("erp", private_policy(signer.verifying_key()))
        .map(|collection| collection.handle())
        .context("explicitly initialize private ERP descriptor");
    let close = pile.close().context("close initialized ERP pile");
    match (result, close) {
        (Ok(handle), Ok(())) => Ok(handle),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(close)) => {
            Err(error.context(format!("also failed closing pile: {close}")))
        }
    }
}
