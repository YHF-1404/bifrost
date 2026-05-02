//! Crash-safe file write: stage to `<path>.tmp`, then `rename`.
//!
//! `rename` is atomic on every POSIX filesystem and on Windows since
//! NTFS, so the destination either keeps its old contents or shows the
//! new contents in full — never half-written.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tokio::fs;

/// Write `contents` to `path` via the staging-and-rename idiom.
pub async fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = staging_path(path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await.ok();
        }
    }
    fs::write(&tmp, contents).await?;
    fs::rename(&tmp, path).await?;
    Ok(())
}

fn staging_path(path: &Path) -> PathBuf {
    let mut s = OsString::from(path.as_os_str());
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_writes_full_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_atomic(&path, b"hello\n").await.unwrap();
        let read = tokio::fs::read(&path).await.unwrap();
        assert_eq!(read, b"hello\n");
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.toml");
        write_atomic(&path, b"first").await.unwrap();
        write_atomic(&path, b"second").await.unwrap();
        let read = tokio::fs::read(&path).await.unwrap();
        assert_eq!(read, b"second");
    }

    #[tokio::test]
    async fn creates_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/sub/cfg.toml");
        write_atomic(&path, b"x").await.unwrap();
        assert!(path.exists());
    }
}
