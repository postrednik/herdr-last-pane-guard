use crate::{Error, Result};
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::DirBuilderExt, fs::MetadataExt, fs::OpenOptionsExt};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
const MAX_STATE_BYTES: u64 = 64 * 1024 * 1024;

/// POSIX cksum includes the input length, unlike the usual CRC-32 checksum.
pub fn session_key(bytes: &[u8]) -> String {
    fn feed(mut crc: u32, byte: u8) -> u32 {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
        crc
    }
    let mut crc = 0;
    for byte in bytes {
        crc = feed(crc, *byte);
    }
    let mut length = bytes.len();
    while length != 0 {
        crc = feed(crc, length.to_le_bytes()[0]);
        length >>= 8;
    }
    format!("{}-{}", !crc, bytes.len())
}

pub fn generation(socket: &Path) -> Result<String> {
    #[cfg(unix)]
    {
        match fs::metadata(socket) {
            Ok(metadata) => Ok(metadata.ino().to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(Error::Stopped),
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        Err(Error::Invalid(
            "native transport is not implemented on this platform",
        ))
    }
}

fn private_dir(path: &Path, recursive: bool) -> std::io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)
}

fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

/// A directory lock intentionally interoperates with the Bash implementation.
pub struct Lock {
    directory: PathBuf,
    owner: String,
}

impl Lock {
    pub fn acquire(directory: &Path, socket: &Path, timeout: Duration) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        let lock_dir = directory.join("lock");
        loop {
            crate::check_interrupted()?;
            match private_dir(&lock_dir, false) {
                Ok(()) => {
                    let lock = Self {
                        directory: lock_dir,
                        owner: format!("{}\n", std::process::id()),
                    };
                    match private_file(&lock.directory.join("owner")) {
                        Ok(mut file) => {
                            if let Err(error) = file.write_all(lock.owner.as_bytes()) {
                                // The directory was created by this acquisition.
                                if let Err(cleanup) = fs::remove_file(lock.directory.join("owner"))
                                {
                                    crate::log(&format!(
                                        "lock owner cleanup failed ({:?})",
                                        cleanup.kind()
                                    ));
                                }
                                if let Err(cleanup) = fs::remove_dir(&lock.directory) {
                                    crate::log(&format!(
                                        "lock cleanup failed ({:?})",
                                        cleanup.kind()
                                    ));
                                }
                                std::mem::forget(lock);
                                return Err(error.into());
                            }
                            return Ok(lock);
                        }
                        Err(error) => {
                            if let Err(cleanup) = fs::remove_dir(&lock.directory) {
                                crate::log(&format!("lock cleanup failed ({:?})", cleanup.kind()));
                            }
                            std::mem::forget(lock);
                            return Err(error.into());
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    generation(socket)?;
                    if Instant::now() >= deadline {
                        return Err(Error::Timeout);
                    }
                    std::thread::sleep(
                        Duration::from_millis(50)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let owner_path = self.directory.join("owner");
        match fs::read_to_string(&owner_path) {
            Ok(owner) if owner == self.owner => {
                if fs::remove_file(owner_path)
                    .and_then(|()| fs::remove_dir(&self.directory))
                    .is_err()
                {
                    crate::log("state lock cleanup failed");
                }
            }
            _ => crate::log("state lock ownership changed; lock left untouched"),
        }
    }
}

pub struct Store {
    pub directory: PathBuf,
    pub socket: PathBuf,
    pub key: String,
    pub generation: String,
}

impl Store {
    pub fn new(root: &Path, socket: PathBuf) -> Result<Self> {
        let current = generation(&socket)?;
        #[cfg(unix)]
        let key = session_key(socket.as_os_str().as_bytes());
        #[cfg(not(unix))]
        let key = session_key(socket.to_string_lossy().as_bytes());
        let directory = root.join(&key);
        private_dir(&directory, true)?;
        Ok(Self {
            directory,
            socket,
            key,
            generation: current,
        })
    }

    pub fn lock(&self) -> Result<Lock> {
        let lock = Lock::acquire(&self.directory, &self.socket, Duration::from_secs(20))?;
        self.check_generation()?;
        Ok(lock)
    }

    pub fn check_generation(&self) -> Result<()> {
        if generation(&self.socket)? == self.generation {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    }

    pub fn state_path(&self) -> PathBuf {
        self.directory.join("state.json")
    }

    pub fn load(&self) -> Result<Option<Value>> {
        let mut options = OpenOptions::new();
        options.read(true);
        // Refuse special files without blocking on a FIFO substituted at open time.
        #[cfg(unix)]
        options.custom_flags(libc::O_NONBLOCK);
        let file = match options.open(self.state_path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A dangling state symlink must not be replaced as missing state.
                match fs::symlink_metadata(self.state_path()) {
                    Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(None)
                    }
                    _ => {
                        return Err(Error::Invalid(
                            "state file is unavailable; refusing to overwrite it",
                        ))
                    }
                }
            }
            Err(error) => return Err(error.into()),
        };
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid("state is not a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(Error::Invalid("state file exceeds size limit"));
        }
        let mut state: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Invalid("invalid state file; refusing to overwrite it"))?;
        if state["version"] != 1
            || [
                "panes",
                "workspaces",
                "tabs",
                "handled",
                "restored",
                "closed",
            ]
            .iter()
            .any(|key| !state[*key].is_object())
        {
            return Err(Error::Invalid(
                "invalid state file; refusing to overwrite it",
            ));
        }
        if state["socket"].as_str() != self.socket.to_str() {
            return Err(Error::Invalid(
                "session key collision; refusing another session state",
            ));
        }
        let valid_generation = state["generation"].as_str().is_some_and(|value| {
            value
                .parse::<u64>()
                .ok()
                .is_some_and(|inode| inode.to_string() == value)
        });
        if !valid_generation {
            return Err(Error::Invalid(
                "invalid state generation; refusing to overwrite it",
            ));
        }
        if state.get("claims").is_none() {
            state["claims"] = json!({});
        }
        if !state["claims"].is_object()
            || ["workspaces", "tabs", "panes"]
                .iter()
                .any(|key| !state["closed"][*key].is_object())
        {
            return Err(Error::Invalid(
                "invalid recovery maps; refusing to overwrite state",
            ));
        }
        Ok(Some(state))
    }

    pub fn prepare(&self) -> Result<Value> {
        self.check_generation()?;
        if let Some(state) = self.load()? {
            if state["generation"] == self.generation {
                return Ok(state);
            }
        }
        let socket = self
            .socket
            .to_str()
            .ok_or(Error::Invalid("socket path is not valid Unicode"))?;
        let state = json!({"version":1,"socket":socket,"generation":self.generation,
            "workspaces":{},"tabs":{},"panes":{},"handled":{},"claims":{},"restored":{},
            "closed":{"workspaces":{},"tabs":{},"panes":{}}});
        self.save(&state)?;
        Ok(state)
    }

    /// Caller holds the session lock for the complete read-modify-write sequence.
    pub fn save(&self, state: &Value) -> Result<()> {
        self.check_generation()?;
        let bytes =
            serde_json::to_vec(state).map_err(|_| Error::Invalid("cannot serialize state"))?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(Error::Invalid("state file exceeds size limit"));
        }
        for _ in 0..128 {
            let file_path = self.directory.join(format!(
                ".state.{}.{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = match private_file(&file_path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            };
            let result = (|| {
                file.write_all(&bytes)?;
                file.sync_all()?;
                self.check_generation()?;
                fs::rename(&file_path, self.state_path())?;
                Ok(())
            })();
            if result.is_err() {
                if let Err(error) = fs::remove_file(&file_path) {
                    crate::log(&format!(
                        "temporary state cleanup failed ({:?})",
                        error.kind()
                    ));
                }
            }
            return result;
        }
        Err(Error::Invalid("cannot allocate temporary state file"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_checksum_vectors() {
        assert_eq!(session_key(b""), "4294967295-0");
        assert_eq!(session_key(b"abc"), "1219131554-3");
        assert_ne!(session_key(b"abc"), session_key(b"abc\n"));
    }

    #[cfg(unix)]
    #[test]
    fn state_round_trip_preserves_unknown_fields_and_claims() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("socket");
        File::create(&socket).unwrap();
        let store = Store::new(&temp.path().join("state"), socket).unwrap();
        let _lock = store.lock().unwrap();
        let mut state = store.prepare().unwrap();
        state["extension"] = json!({"future":true});
        state.as_object_mut().unwrap().remove("claims");
        store.save(&state).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded["claims"], json!({}));
        assert_eq!(loaded["extension"], state["extension"]);
        let old = fs::read(store.state_path()).unwrap();
        state["version"] = json!(2);
        fs::write(store.state_path(), serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(store.prepare().is_err());
        assert_ne!(fs::read(store.state_path()).unwrap(), old);
    }
}
