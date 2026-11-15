//! File packages: the tier above blobs.
//!
//! A blob is one object capped at 512 MiB. Bigger inputs become a **file
//! package**: parts plus a manifest under `$SVCD_TEMP_DIR/files/<sha256>/`, with
//! the whole file's SHA-256 as the id (16 GiB ceiling, 10000 parts).
//!
//! The caller sends `{"$file": "sha256:…"}`; the host replaces it with
//! `{"$file": id, "dir": …, "manifest": …, "size": n, "name": …, "mimeType": …}`
//! before the plugin runs. Read the manifest for the part list, then stream the
//! parts in order.
//!
//! See `plugin-manager/docs/plugin-blob.md` §「大文件：/api/v1/file 文件包」.

use serde_json::Value;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use crate::blob::MAX_BLOB_BYTES;

/// Largest package the host will assemble, for early rejection.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// A `$file` argument, already resolved by the host.
#[derive(Debug, Clone)]
pub struct FileArg {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: PathBuf,
    pub size: u64,
    pub name: String,
    pub mime_type: String,
}

/// Pick `arguments[field]` when it is a resolved file package.
pub fn file_argument(arguments: &Value, field: &str) -> Result<Option<FileArg>, String> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    let Some(dir) = value.get("dir").and_then(Value::as_str) else {
        return Ok(None);
    };
    let size = value.get("size").and_then(Value::as_u64).unwrap_or(0);
    if size > MAX_FILE_BYTES {
        return Err(format!("file package exceeds {} bytes", MAX_FILE_BYTES));
    }
    Ok(Some(FileArg {
        id: value.get("$file").and_then(Value::as_str).unwrap_or("").to_string(),
        dir: PathBuf::from(dir),
        manifest: value
            .get("manifest")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_default(),
        size,
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_string(),
        mime_type: value
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream")
            .to_string(),
    }))
}

impl FileArg {
    /// Part files in ascending order, as listed by `manifest.json`.
    pub fn part_paths(&self) -> Result<Vec<PathBuf>, String> {
        let manifest_path = if self.manifest.as_os_str().is_empty() {
            self.dir.join("manifest.json")
        } else {
            self.manifest.clone()
        };
        let bytes = std::fs::read(&manifest_path)
            .map_err(|error| format!("read {}: {}", manifest_path.display(), error))?;
        let manifest: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse {}: {}", manifest_path.display(), error))?;
        let mut numbers: Vec<u64> = manifest
            .get("parts")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.get("partNumber").and_then(Value::as_u64))
                    .collect()
            })
            .unwrap_or_default();
        numbers.sort_unstable();
        Ok(numbers
            .into_iter()
            .map(|number| self.dir.join(format!("part-{number:05}")))
            .collect())
    }

    /// A sequential `Read` across every part, so a caller can stream instead of
    /// loading the package into memory.
    pub fn open(&self) -> Result<PartReader, String> {
        let parts = self.part_paths()?;
        if parts.is_empty() {
            return Err("file package has no parts".into());
        }
        Ok(PartReader {
            parts,
            index: 0,
            current: None,
        })
    }

    /// Convenience for small files and tests.
    pub fn read_all(&self) -> Result<Vec<u8>, String> {
        if self.size > MAX_BLOB_BYTES {
            return Err("file package is too large to read at once; use open()".into());
        }
        let mut reader = self.open()?;
        let mut buffer = Vec::with_capacity(self.size as usize);
        reader
            .read_to_end(&mut buffer)
            .map_err(|error| format!("read package: {error}"))?;
        Ok(buffer)
    }
}

/// Reads the parts of a file package back to back.
pub struct PartReader {
    parts: Vec<PathBuf>,
    index: usize,
    current: Option<BufReader<File>>,
}

impl Read for PartReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.current.is_none() {
                if self.index >= self.parts.len() {
                    return Ok(0);
                }
                let path: &Path = &self.parts[self.index];
                let file = File::open(path)?;
                self.index += 1;
                self.current = Some(BufReader::new(file));
            }
            let reader = self.current.as_mut().expect("just opened");
            let read = reader.read(buffer)?;
            if read > 0 {
                return Ok(read);
            }
            self.current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_resolved_file_argument_is_recognised() {
        let arguments = json!({
            "input": {
                "$file": "sha256:bb",
                "dir": "/tmp/files/bb",
                "manifest": "/tmp/files/bb/manifest.json",
                "size": 10,
                "name": "x.iso",
                "mimeType": "application/x-iso9660-image"
            }
        });
        let file = file_argument(&arguments, "input").unwrap().expect("file");
        assert_eq!(file.id, "sha256:bb");
        assert_eq!(file.name, "x.iso");
    }

    #[test]
    fn a_blob_argument_is_not_a_file_package() {
        let arguments = json!({ "input": { "$blob": "sha256:aa", "path": "/tmp/x", "size": 3 } });
        assert!(file_argument(&arguments, "input").unwrap().is_none());
    }

    #[test]
    fn parts_are_read_in_order_and_concatenated() {
        let root = std::env::temp_dir().join(format!(
            "svcd-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dir = root.join("sha256-abc");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("part-00002"), b"world").expect("part 2");
        std::fs::write(dir.join("part-00001"), b"hello ").expect("part 1");
        std::fs::write(
            dir.join("manifest.json"),
            br#"{"parts":[{"partNumber":2,"size":5},{"partNumber":1,"size":6}]}"#,
        )
        .expect("manifest");

        let file = FileArg {
            id: "sha256:abc".into(),
            dir: dir.clone(),
            manifest: dir.join("manifest.json"),
            size: 11,
            name: "x".into(),
            mime_type: "text/plain".into(),
        };
        assert_eq!(file.read_all().unwrap(), b"hello world");
        let _ = std::fs::remove_dir_all(root);
    }
}
