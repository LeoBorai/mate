//! Versioned JSON persistence for sandbox sessions.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;
use crate::session::SandboxSession;

const FILE_NAME: &str = "sandboxes.json";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    sessions: Vec<SandboxSession>,
}

/// Reads and writes `sandboxes.json` in an injected directory.
#[derive(Debug, Clone)]
pub struct SandboxStore {
    dir: PathBuf,
}

fn store_err(what: &str, e: impl std::fmt::Display) -> SandboxError {
    SandboxError::Store(format!("{what}: {e}"))
}

impl SandboxStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// A missing file is an empty store.
    pub fn load(&self) -> Result<Vec<SandboxSession>, SandboxError> {
        let bytes = match fs::read(self.path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(store_err("read", e)),
        };
        let disk: OnDisk = serde_json::from_slice(&bytes).map_err(|e| store_err("parse", e))?;
        if disk.version != VERSION {
            return Err(SandboxError::Store(format!(
                "unsupported store version {}",
                disk.version
            )));
        }
        Ok(disk.sessions)
    }

    /// Write to a temp file in the same directory, fsync, then rename.
    pub fn save(&self, sessions: &[SandboxSession]) -> Result<(), SandboxError> {
        fs::create_dir_all(&self.dir).map_err(|e| store_err("create dir", e))?;
        let json = serde_json::to_vec_pretty(&OnDisk {
            version: VERSION,
            sessions: sessions.to_vec(),
        })
        .map_err(|e| store_err("serialize", e))?;
        let tmp = self.dir.join(format!("{FILE_NAME}.tmp"));
        write_synced(&tmp, &json).map_err(|e| store_err("write temp", e))?;
        fs::rename(&tmp, self.path()).map_err(|e| store_err("rename", e))
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::sample;

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            SandboxStore::new(dir.path()).load().unwrap().is_empty(),
            "no file yet means no sessions, not an error"
        );
    }

    #[test]
    fn save_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = SandboxStore::new(dir.path());
        store.save(&[sample()]).unwrap();
        assert_eq!(store.load().unwrap(), vec![sample()], "saved sessions load back identical");
    }

    #[test]
    fn leftover_temp_file_does_not_corrupt_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = SandboxStore::new(dir.path());
        store.save(&[sample()]).unwrap();
        fs::write(dir.path().join("sandboxes.json.tmp"), b"{ half written").unwrap();
        assert_eq!(
            store.load().unwrap(),
            vec![sample()],
            "an interrupted write leaves only the temp file damaged; the real file still loads"
        );
        store.save(&[]).unwrap();
        assert!(store.load().unwrap().is_empty(), "a later save overwrites the stale temp file");
    }
}
