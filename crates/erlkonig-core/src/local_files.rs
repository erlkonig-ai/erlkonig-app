//! Descriptor-bound local I/O for explicit authenticated host-file operations.
//! Paths are not collection authority. No registry, Files staging or UI policy.
use anyhow::{Result, bail};
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use zeroize::Zeroizing;

/// Open once; never reopen the supplied pathname to read its bytes.
pub(crate) struct LocalFile { file: File, selected: Metadata }

impl LocalFile {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW).open(path)
            .map_err(|_| anyhow::anyhow!("Cannot open the local regular file"))?;
        let selected = file.metadata()?;
        if !selected.is_file() { bail!("The local source must be a regular file"); }
        Ok(Self { file, selected })
    }

    pub(crate) fn read(mut self, limit: usize) -> Result<Zeroizing<Vec<u8>>> {
        let unchanged = |current: &Metadata| {
            current.is_file() && current.len() == self.selected.len()
                && current.modified().ok() == self.selected.modified().ok()
                && current.ctime() == self.selected.ctime()
                && current.ctime_nsec() == self.selected.ctime_nsec()
        };
        let error = || anyhow::anyhow!("Local file changed, is unreadable, or exceeds the input limit");
        if self.selected.len() > limit as u64 || !unchanged(&self.file.metadata().map_err(|_| error())?) {
            return Err(error());
        }
        // The selected length was checked against a usize limit above. Allocate
        // and initialize exactly that length once under zeroizing ownership;
        // slice reads cannot grow/reallocate a buffer holding plaintext.
        let bytes = read_fixed_length(&mut self.file, self.selected.len() as usize).map_err(|_| error())?;
        // A changed/growing source is refused, never accommodated by resizing.
        if !unchanged(&self.file.metadata().map_err(|_| error())?) { return Err(error()); }
        Ok(bytes)
    }
}

/// File metadata can underreport readable content (for example, pseudo-files).
/// Preserve exact-length semantics with one zeroizing byte of EOF lookahead.
fn read_fixed_length(reader: &mut impl Read, length: usize) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(vec![0; length]);
    reader.read_exact(bytes.as_mut_slice())?;
    let mut extra = Zeroizing::new([0u8; 1]);
    loop {
        match reader.read(extra.as_mut_slice()) {
            Ok(0) => return Ok(bytes),
            Ok(_) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                "Local file contains more bytes than its selected length")),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::symlink;

    #[test]
    fn descriptor_reads_large_file_and_rejects_nonregular_or_changed_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.json");
        let bytes = vec![b' '; 1024 * 1024 + 17];
        fs::write(&path, &bytes).unwrap();
        assert_eq!(&*LocalFile::open(&path).unwrap().read(bytes.len()).unwrap(), &bytes);
        assert!(LocalFile::open(&path).unwrap().read(bytes.len() - 1).is_err());
        let opened = LocalFile::open(&path).unwrap();
        fs::write(&path, b"changed").unwrap();
        assert!(opened.read(bytes.len()).is_err());
        assert!(LocalFile::open(dir.path()).is_err());
        let link = dir.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert!(LocalFile::open(&link).is_err());
    }

    #[test]
    fn fixed_reader_has_exact_capacity_and_refuses_growth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synthetic.1pux");
        let plaintext = b"0123456789abcdefg";
        fs::write(&path, plaintext).unwrap();
        let bytes = LocalFile::open(&path).unwrap().read(64).unwrap();
        assert_eq!(bytes.as_slice(), plaintext);
        assert_eq!(bytes.capacity(), plaintext.len(), "fixed allocation has no growth reserve");
        let opened = LocalFile::open(&path).unwrap();
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"grew").unwrap();
        assert!(opened.read(64).is_err(), "growth after selection is refused even below the cap");
        fs::write(&path, b"").unwrap();
        let empty = LocalFile::open(&path).unwrap().read(0).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.capacity(), 0);
    }


    #[test]
    fn fixed_reader_rejects_underreported_length_and_retries_probe_interruptions() {
        assert!(read_fixed_length(&mut &b"extra"[..], 0).is_err());
        assert!(read_fixed_length(&mut &b"extra"[..], 4).is_err());
        assert!(read_fixed_length(&mut &b"short"[..], 6).is_err());
        struct InterruptAtEnd { bytes: std::io::Cursor<&'static [u8]>, interrupted: bool }
        impl Read for InterruptAtEnd {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.bytes.position() == self.bytes.get_ref().len() as u64 && !self.interrupted {
                    self.interrupted = true;
                    Err(std::io::ErrorKind::Interrupted.into())
                } else { self.bytes.read(out) }
            }
        }
        let mut reader = InterruptAtEnd { bytes: std::io::Cursor::new(b"exact".as_slice()), interrupted: false };
        let bytes = read_fixed_length(&mut reader, 5).unwrap();
        assert_eq!(bytes.as_slice(), b"exact");
        assert_eq!(bytes.capacity(), 5);
        assert!(reader.interrupted, "the EOF probe retries Interrupted instead of accepting or failing early");
    }

}
