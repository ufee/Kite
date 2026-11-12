//! Files in the app data folder: servers, settings, session and terminal history.
//!
//! Every write is atomic: a temporary file next to the target, flushed to disk, then renamed over it.
//! A file that no longer parses is moved aside as `<name>.bad` and treated as missing,
//! so one broken file never blocks the start.
//!
//! Settings and session are owned by the UI and stored as opaque JSON. Servers are typed here:
//! the core will connect with them, and unknown fields (a password by mistake) never reach the disk.

pub mod import;
mod servers;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use servers::{KeyMode, Server, ServerConfig};

/// Terminal history is capped by the UI at 1000 lines; this guards the disk against a runaway tab.
const TERM_MAX_BYTES: usize = 4 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Doc {
    Settings,
    Session,
}

impl Doc {
    fn file(self) -> &'static str {
        match self {
            Doc::Settings => "settings.json",
            Doc::Session => "session.json",
        }
    }
}

pub struct Store {
    dir: PathBuf,
    /// One writer at a time: the session timer and the close handler may race.
    write: Mutex<()>,
}

impl Store {
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(dir.join("terms"))?;
        Ok(Self { dir, write: Mutex::new(()) })
    }

    pub fn load_doc(&self, doc: Doc) -> Option<Value> {
        self.read_json(&self.dir.join(doc.file()))
    }

    pub fn save_doc(&self, doc: Doc, data: &Value) -> std::io::Result<()> {
        self.write_json(&self.dir.join(doc.file()), data)
    }

    /// The transfer queue: the core owns it, so it survives a UI reload and a crash of the window.
    pub fn load_transfers<T: DeserializeOwned>(&self) -> Option<T> {
        self.read_json(&self.dir.join("transfers.json"))
    }

    pub fn save_transfers<T: Serialize>(&self, data: &T) -> std::io::Result<()> {
        self.write_json(&self.dir.join("transfers.json"), data)
    }

    pub fn load_servers(&self) -> Option<ServerConfig> {
        self.read_json(&self.dir.join("servers.json"))
    }

    pub fn save_servers(&self, cfg: &ServerConfig) -> std::io::Result<()> {
        self.write_json(&self.dir.join("servers.json"), cfg)
    }

    pub fn load_term(&self, id: u32) -> Option<String> {
        fs::read_to_string(self.term_path(id)).ok()
    }

    pub fn save_term(&self, id: u32, data: &str) -> std::io::Result<()> {
        if data.len() > TERM_MAX_BYTES {
            return Err(std::io::Error::other("terminal history is too large"));
        }
        let _guard = self.write.lock().unwrap_or_else(|e| e.into_inner());
        write_atomic(&self.term_path(id), data.as_bytes())
    }

    /// Removes the history of every tab that is not in `keep`.
    pub fn prune_terms(&self, keep: &[u32]) -> std::io::Result<()> {
        let _guard = self.write.lock().unwrap_or_else(|e| e.into_inner());
        for entry in fs::read_dir(self.dir.join("terms"))? {
            let path = entry?.path();
            let id = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u32>().ok());
            if id.is_none_or(|id| !keep.contains(&id)) {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(())
    }

    fn term_path(&self, id: u32) -> PathBuf {
        self.dir.join("terms").join(format!("{id}.ansi"))
    }

    fn read_json<T: DeserializeOwned>(&self, path: &Path) -> Option<T> {
        let text = fs::read_to_string(path).ok()?;
        match serde_json::from_str(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                // Keep the broken file for a look later, start as if it were missing.
                eprintln!("kite: {} is unreadable ({e}), moved aside", path.display());
                let _ = fs::rename(path, path.with_extension("bad"));
                None
            }
        }
    }

    fn write_json<T: Serialize + ?Sized>(&self, path: &Path, data: &T) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(data)?;
        let _guard = self.write.lock().unwrap_or_else(|e| e.into_inner());
        write_atomic(path, &bytes)
    }
}

/// Temporary file, flush, rename: a crash leaves either the old file or the new one, never half of it.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // On Windows std::fs::rename replaces an existing target (MoveFileExW with REPLACE_EXISTING).
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_store(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kite-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (Store::new(dir.clone()).unwrap(), dir)
    }

    #[test]
    fn doc_round_trip_and_overwrite() {
        let (store, dir) = temp_store("doc");
        assert!(store.load_doc(Doc::Session).is_none());
        store.save_doc(Doc::Session, &json!({ "v": 1, "a": 1 })).unwrap();
        store.save_doc(Doc::Session, &json!({ "v": 1, "a": 2 })).unwrap();
        assert_eq!(store.load_doc(Doc::Session).unwrap()["a"], 2);
        assert!(!dir.join("session.json.tmp").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn broken_file_is_moved_aside() {
        let (store, dir) = temp_store("bad");
        fs::write(dir.join("settings.json"), "{ not json").unwrap();
        assert!(store.load_doc(Doc::Settings).is_none());
        assert!(dir.join("settings.bad").exists());
        assert!(!dir.join("settings.json").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn servers_drop_unknown_fields() {
        let (store, dir) = temp_store("srv");
        let cfg: ServerConfig = serde_json::from_value(json!({
            "servers": [{ "id": "s-1", "code": "web1", "host": "203.0.113.5", "password": "hunter2",
                          "users": [{ "name": "root", "key": "auto", "secret": "x" }], "defaultUser": "root" }],
            "groups": ["Web"]
        }))
        .unwrap();
        store.save_servers(&cfg).unwrap();
        let text = fs::read_to_string(dir.join("servers.json")).unwrap();
        assert!(!text.contains("hunter2") && !text.contains("secret"));
        let back = store.load_servers().unwrap();
        assert_eq!(back.servers[0].port, 22);
        assert_eq!(back.servers[0].keepalive, 30);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_keeps_only_listed_terms() {
        let (store, dir) = temp_store("terms");
        for id in [1, 2, 3] {
            store.save_term(id, "line\r\n").unwrap();
        }
        store.prune_terms(&[2]).unwrap();
        assert!(store.load_term(1).is_none() && store.load_term(3).is_none());
        assert_eq!(store.load_term(2).as_deref(), Some("line\r\n"));
        let _ = fs::remove_dir_all(dir);
    }
}
