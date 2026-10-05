//! Size-rotated log files: `name.log`, then `name.log.1` .. `name.log.<keep>`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub struct RotatingLog {
    path: PathBuf,
    max_bytes: u64,
    keep: u32,
    file: Option<File>,
    size: u64,
}

impl RotatingLog {
    pub const DEFAULT_MAX: u64 = 4 << 20;
    pub const DEFAULT_KEEP: u32 = 3;

    pub fn new(path: impl Into<PathBuf>, max_bytes: u64, keep: u32) -> Self {
        RotatingLog { path: path.into(), max_bytes, keep: keep.max(1), file: None, size: 0 }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            let f = OpenOptions::new().append(true).create(true).mode(0o600).open(&self.path)?;
            self.size = f.metadata()?.len();
            self.file = Some(f);
        }
        Ok(self.file.as_mut().expect("just opened"))
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let numbered = |n: u32| PathBuf::from(format!("{}.{n}", self.path.display()));
        let _ = fs::remove_file(numbered(self.keep));
        for n in (1..self.keep).rev() {
            let _ = fs::rename(numbered(n), numbered(n + 1));
        }
        match fs::rename(&self.path, numbered(1)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Appends one line (a newline is added).
    pub fn line(&mut self, text: &str) -> io::Result<()> {
        let len = text.len() as u64 + 1;
        self.open()?;
        if self.size > 0 && self.size + len > self.max_bytes {
            self.rotate()?;
        }
        let f = self.open()?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
        self.size += len;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_keeps_a_bounded_number_of_files() {
        let d = std::env::temp_dir().join(format!("erlkonig-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let mut log = RotatingLog::new(d.join("x.log"), 100, 2);
        for i in 0..50 {
            log.line(&format!("line {i:03} padding padding")).unwrap();
        }
        let mut names: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["x.log", "x.log.1", "x.log.2"]);
        for n in &names {
            assert!(fs::metadata(d.join(n)).unwrap().len() <= 100);
        }
        assert!(fs::read_to_string(d.join("x.log")).unwrap().contains("line 049"));
    }
}
