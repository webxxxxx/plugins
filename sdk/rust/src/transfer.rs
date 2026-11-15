//! Chunked file transfer over `tools/call`.
//!
//! The host gives a plugin exactly one request channel (`POST ./rpc` for the
//! browser, `tools/call` on the control channel) and caps a frame at 1 MiB.
//! Files therefore move as base64 chunks with a simple append-only protocol:
//!
//! * `file_put`  `{path, offset, total, data(base64), final}` → `{"ok":…}`
//! * `file_get`  `{path, offset, length}` → `{"data":…, "size":…, "eof":…}`
//!
//! The browser half lives in `sdk/web/plugin-ui.mjs` (`uploadFile` /
//! `downloadFile`); keep the two in sync.
//!
//! Names are restricted to a single `[A-Za-z0-9._-]` component: no separators,
//! no leading dot, so a capability cannot address anything outside the store.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::{Context, ToolOutcome};

/// Largest chunk accepted in one `file_put`, before base64 expansion.
pub const MAX_CHUNK_BYTES: usize = 512 * 1024;
/// Largest file this store will assemble.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// A flat directory of user-supplied files, addressed by bare name.
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FileStore { root: root.into() }
    }

    /// `<data_dir>/upload` when the host gave us one, else a temp directory.
    pub fn from_context(context: &Context) -> Self {
        let root = context
            .data_dir()
            .map(|dir| dir.join("upload"))
            .unwrap_or_else(|| std::env::temp_dir().join("svcd-upload"));
        FileStore::new(root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_of(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty() || name.len() > 128 {
            return Err("path must be 1..128 bytes".into());
        }
        if name.starts_with('.') {
            return Err("path must not start with a dot".into());
        }
        if !name
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '-' | '_'))
        {
            return Err("path may only contain A-Z a-z 0-9 . - _".into());
        }
        Ok(self.root.join(name))
    }

    /// Handle one `file_put` chunk.
    pub fn put(&self, arguments: &Value) -> ToolOutcome {
        let name = arguments.get("path").and_then(Value::as_str).unwrap_or("");
        let path = match self.path_of(name) {
            Ok(path) => path,
            Err(error) => return ToolOutcome::error(error),
        };
        let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let total = arguments.get("total").and_then(Value::as_u64).unwrap_or(0);
        let payload = arguments.get("data").and_then(Value::as_str).unwrap_or("");
        let final_chunk = arguments.get("final").and_then(Value::as_bool).unwrap_or(false);

        if total > MAX_FILE_BYTES {
            return ToolOutcome::error(format!("file exceeds {} bytes", MAX_FILE_BYTES));
        }
        let bytes = match BASE64.decode(payload) {
            Ok(bytes) => bytes,
            Err(error) => return ToolOutcome::error(format!("data: {error}")),
        };
        if bytes.len() > MAX_CHUNK_BYTES {
            return ToolOutcome::error(format!("chunk exceeds {} bytes", MAX_CHUNK_BYTES));
        }
        if offset.saturating_add(bytes.len() as u64) > total {
            return ToolOutcome::error("chunk overruns the declared total");
        }

        if let Err(error) = fs::create_dir_all(&self.root) {
            return ToolOutcome::error(format!("create {}: {}", self.root.display(), error));
        }
        let written = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        if written != offset {
            return ToolOutcome::error(format!(
                "offset mismatch: store holds {written} bytes, chunk starts at {offset}"
            ));
        }

        let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => file,
            Err(error) => return ToolOutcome::error(format!("open {}: {}", path.display(), error)),
        };
        if let Err(error) = file.write_all(&bytes) {
            return ToolOutcome::error(format!("write {}: {}", path.display(), error));
        }
        if final_chunk {
            if let Err(error) = file.sync_all() {
                return ToolOutcome::error(format!("sync {}: {}", path.display(), error));
            }
        }

        let size = written + bytes.len() as u64;
        if final_chunk && size != total {
            return ToolOutcome::error(format!("assembled {size} bytes, expected {total}"));
        }
        ToolOutcome::text(
            json!({ "ok": true, "path": name, "size": size, "complete": size == total })
                .to_string(),
        )
    }

    /// Handle one `file_get` chunk.
    pub fn get(&self, arguments: &Value) -> ToolOutcome {
        let name = arguments.get("path").and_then(Value::as_str).unwrap_or("");
        let path = match self.path_of(name) {
            Ok(path) => path,
            Err(error) => return ToolOutcome::error(error),
        };
        let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let length = arguments
            .get("length")
            .and_then(Value::as_u64)
            .unwrap_or(MAX_CHUNK_BYTES as u64)
            .min(MAX_CHUNK_BYTES as u64);

        let size = match fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(error) => return ToolOutcome::error(format!("stat {name}: {error}")),
        };
        if offset > size {
            return ToolOutcome::error("offset is past the end of the file");
        }

        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) => return ToolOutcome::error(format!("open {name}: {error}")),
        };
        if let Err(error) = file.seek(SeekFrom::Start(offset)) {
            return ToolOutcome::error(format!("seek {name}: {error}"));
        }
        let want = length.min(size - offset) as usize;
        let mut buffer = vec![0u8; want];
        if let Err(error) = file.read_exact(&mut buffer) {
            return ToolOutcome::error(format!("read {name}: {error}"));
        }

        ToolOutcome::text(
            json!({
                "data": BASE64.encode(&buffer),
                "size": size,
                "eof": offset + want as u64 >= size,
            })
            .to_string(),
        )
    }

    /// `file_list`: names and sizes, sorted, for a UI picker.
    pub fn list(&self) -> ToolOutcome {
        let mut entries = Vec::new();
        match fs::read_dir(&self.root) {
            Ok(reader) => {
                for entry in reader.flatten() {
                    if let Ok(meta) = entry.metadata() {
                        if meta.is_file() {
                            entries.push(json!({
                                "name": entry.file_name().to_string_lossy(),
                                "size": meta.len(),
                            }));
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return ToolOutcome::error(format!("list {}: {}", self.root.display(), error)),
        }
        entries.sort_by(|left, right| {
            left["name"]
                .as_str()
                .unwrap_or("")
                .cmp(right["name"].as_str().unwrap_or(""))
        });
        ToolOutcome::text(json!({ "files": entries }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (FileStore, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "svcd-transfer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        (FileStore::new(&root), root)
    }

    fn text(outcome: ToolOutcome) -> String {
        match outcome {
            ToolOutcome::Text(text) => text,
            ToolOutcome::Error(error) => panic!("unexpected error: {error}"),
            ToolOutcome::Blob { .. } => panic!("unexpected blob result"),
        }
    }

    #[test]
    fn round_trips_in_chunks() {
        let (store, root) = store();
        let payload = b"hello transfer".to_vec();
        let encoded = BASE64.encode(&payload);

        let reply = text(store.put(&json!({
            "path": "a.txt", "offset": 0, "total": payload.len(), "data": encoded, "final": true
        })));
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["complete"], true);

        let reply = text(store.get(&json!({ "path": "a.txt", "offset": 0, "length": 1024 })));
        let value: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(BASE64.decode(value["data"].as_str().unwrap()).unwrap(), payload);
        assert_eq!(value["eof"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_paths_that_escape_the_store() {
        let (store, root) = store();
        for name in ["../etc/passwd", "/etc/passwd", ".hidden", "a/b", ""] {
            match store.put(&json!({ "path": name, "offset": 0, "total": 0, "data": "" })) {
                ToolOutcome::Error(_) => {}
                _ => panic!("{name} should have been rejected"),
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_a_chunk_that_does_not_line_up() {
        let (store, root) = store();
        match store.put(&json!({
            "path": "b.txt", "offset": 5, "total": 10, "data": BASE64.encode(b"x"), "final": false
        })) {
            ToolOutcome::Error(error) => assert!(error.contains("offset mismatch"), "{error}"),
            _ => panic!("offset mismatch should fail"),
        }
        let _ = fs::remove_dir_all(root);
    }
}
