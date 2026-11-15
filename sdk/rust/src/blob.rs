//! Large inputs and outputs: `$blob` in, `$blobPath` out.
//!
//! The control channel caps a frame at 1 MiB, so big payloads never travel as
//! JSON. The host owns a content-addressed store:
//!
//! * **arguments** — the caller sends `{"$blob": "sha256:…"}` and the host
//!   replaces it with `{"$blob": id, "path": "/…", "size": n}` before the plugin
//!   sees it. Read the file; never trust a path the client made up.
//! * **results** — the plugin writes into `$SVCD_TEMP_DIR/incoming/` and returns
//!   `{"$blobPath": "/…/incoming/x", "name": …, "mimeType": …}`. The host hashes
//!   the file into the store and replaces the result with a `resource_link`.
//!
//! Both directions are covered by the env the host injects; nothing here needs
//! the management API.

use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Context, ToolOutcome};

/// Largest blob the host accepts, for early rejection.
pub const MAX_BLOB_BYTES: u64 = 512 * 1024 * 1024;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A `$blob` argument, already resolved by the host.
#[derive(Debug, Clone)]
pub struct BlobArg {
    /// `sha256:<hex>`
    pub id: String,
    /// Absolute read-only path inside the host's temp directory.
    pub path: PathBuf,
    pub size: u64,
}

impl BlobArg {
    pub fn open(&self) -> Result<File, String> {
        File::open(&self.path).map_err(|error| format!("open {}: {}", self.path.display(), error))
    }

    /// Read the whole blob as UTF-8 text. For real files, stream instead.
    pub fn read_to_string(&self) -> Result<String, String> {
        fs::read_to_string(&self.path)
            .map_err(|error| format!("read {}: {}", self.path.display(), error))
    }
}

/// Pick `arguments[field]` when it is a resolved blob.
///
/// Returns `Ok(None)` when the field is absent or is a plain value, so a
/// capability can accept either a small inline argument or a blob.
pub fn blob_argument(arguments: &Value, field: &str) -> Result<Option<BlobArg>, String> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    let Some(path) = value.get("path").and_then(Value::as_str) else {
        return Ok(None);
    };
    let size = value.get("size").and_then(Value::as_u64).unwrap_or(0);
    if size > MAX_BLOB_BYTES {
        return Err(format!("blob exceeds {} bytes", MAX_BLOB_BYTES));
    }
    Ok(Some(BlobArg {
        id: value.get("$blob").and_then(Value::as_str).unwrap_or("").to_string(),
        path: PathBuf::from(path),
        size,
    }))
}

/// Builds a result the host turns into a blob.
///
/// ```no_run
/// # use svcd_plugin::{blob::BlobWriter, Context};
/// # fn demo(context: &Context) -> Result<svcd_plugin::ToolOutcome, String> {
/// let mut writer = BlobWriter::create(context, "report.txt", "text/plain")?;
/// writer.write_all(b"hello")?;
/// Ok(writer.finish())
/// # }
/// ```
pub struct BlobWriter {
    file: Option<File>,
    path: PathBuf,
    name: String,
    mime_type: String,
}

impl BlobWriter {
    /// Create a uniquely named file inside `$SVCD_TEMP_DIR/incoming/`.
    pub fn create(context: &Context, name: &str, mime_type: &str) -> Result<Self, String> {
        let temp = context.temp_dir().ok_or_else(|| {
            "SVCD_TEMP_DIR is not set: blob results only work inside a host".to_string()
        })?;
        Self::create_in(temp, name, mime_type)
    }

    /// Same, with the host's temp root passed explicitly — handy when the
    /// plugin keeps the path rather than the whole [`Context`].
    pub fn create_in(temp: &Path, name: &str, mime_type: &str) -> Result<Self, String> {
        let incoming = temp.join("incoming");
        fs::create_dir_all(&incoming)
            .map_err(|error| format!("create {}: {}", incoming.display(), error))?;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let stem = sanitize_stem(name);
        let path = incoming.join(format!("{stem}-{}-{stamp}-{unique}", std::process::id()));

        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("create {}: {}", path.display(), error))?;

        Ok(BlobWriter {
            file: Some(file),
            path,
            name: if name.is_empty() {
                "blob".to_string()
            } else {
                name.to_string()
            },
            mime_type: if mime_type.is_empty() {
                "application/octet-stream".to_string()
            } else {
                mime_type.to_string()
            },
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        let Some(file) = self.file.as_mut() else {
            return Err("blob writer is already finished".into());
        };
        file.write_all(bytes)
            .map_err(|error| format!("write {}: {}", self.path.display(), error))
    }

    /// Flush, sync, and hand back the result object the host expects.
    pub fn finish(mut self) -> ToolOutcome {
        if let Some(mut file) = self.file.take() {
            if let Err(error) = file.flush().and_then(|()| file.sync_all()) {
                return ToolOutcome::error(format!("finalize {}: {}", self.path.display(), error));
            }
        }
        ToolOutcome::Blob {
            path: self.path,
            name: self.name,
            mime_type: self.mime_type,
        }
    }
}

fn sanitize_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '.' | '-' | '_') {
                value
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = stem.trim_matches('.').trim();
    if trimmed.is_empty() {
        "blob".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_resolved_blob_argument_is_recognised() {
        let arguments = json!({ "input": { "$blob": "sha256:aa", "path": "/tmp/x", "size": 3 } });
        let blob = blob_argument(&arguments, "input").unwrap().expect("blob");
        assert_eq!(blob.id, "sha256:aa");
        assert_eq!(blob.size, 3);
    }

    #[test]
    fn a_plain_argument_is_not_mistaken_for_a_blob() {
        let arguments = json!({ "input": "just text" });
        assert!(blob_argument(&arguments, "input").unwrap().is_none());
        assert!(blob_argument(&arguments, "missing").unwrap().is_none());
    }

    #[test]
    fn the_writer_stays_inside_the_incoming_directory() {
        let root = std::env::temp_dir().join(format!(
            "svcd-blob-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SVCD_TEMP_DIR", &root);
        let context = Context::from_env();

        let mut writer = BlobWriter::create(&context, "../escape.txt", "text/plain").expect("writer");
        assert!(writer.path().starts_with(root.join("incoming")));
        writer.write_all(b"hi").expect("write");
        match writer.finish() {
            ToolOutcome::Blob { name, .. } => assert_eq!(name, "../escape.txt"),
            _ => panic!("expected a blob outcome"),
        }
        let _ = fs::remove_dir_all(root);
    }
}
