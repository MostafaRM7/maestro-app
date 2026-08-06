use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use argon2::Argon2;
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

const KEY_BYTES: usize = 32;
const MAX_ENVELOPE_BYTES: usize = 16 * 1024;
const TEMP_FILE_ATTEMPTS: usize = 128;
const WRAPPING_AAD: &[u8] = b"com.maestroai.app/database-key-v1";

// The keyring API has no compare-and-set primitive. This mutex prevents
// duplicate key creation inside one process. Cross-process initialization must
// still be routed through Maestro's per-user daemon singleton.
static PROCESS_KEY_CREATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub struct DatabaseKey(Zeroizing<Vec<u8>>);

impl std::fmt::Debug for DatabaseKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DatabaseKey([REDACTED])")
    }
}

impl DatabaseKey {
    pub fn generate() -> Self {
        let mut bytes = vec![0_u8; KEY_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        Self(Zeroizing::new(bytes))
    }

    /// Creates a database key from exactly 256 bits of key material.
    ///
    /// # Errors
    ///
    /// Returns [`KeyStoreError::InvalidKeyLength`] for any other length.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, KeyStoreError> {
        if bytes.len() != KEY_BYTES {
            return Err(KeyStoreError::InvalidKeyLength(bytes.len()));
        }
        Ok(Self(Zeroizing::new(bytes)))
    }

    pub fn expose(&self) -> &[u8] {
        self.0.as_slice()
    }

    fn encode(&self) -> String {
        STANDARD_NO_PAD.encode(self.expose())
    }

    fn decode(value: &str) -> Result<Self, KeyStoreError> {
        let bytes = STANDARD_NO_PAD
            .decode(value)
            .map_err(|error| KeyStoreError::InvalidEncoding(error.to_string()))?;
        Self::from_bytes(bytes)
    }
}

pub trait DatabaseKeyStore: Send + Sync {
    /// Loads the existing database key or creates and durably protects a new one.
    ///
    /// # Errors
    ///
    /// Returns [`KeyStoreError`] when secure storage, key derivation,
    /// encryption, decoding, or persistence fails.
    fn load_or_create(&self) -> Result<DatabaseKey, KeyStoreError>;
}

#[derive(Debug, Clone)]
/// Database-key storage backed by the operating system credential service.
///
/// Creation is serialized within this process. Because the platform keyring
/// interface does not expose compare-and-set, callers must route initialization
/// through Maestro's per-user daemon singleton to prevent a cross-process
/// get-then-set race.
pub struct OsKeyStore {
    service: String,
    account: String,
}

impl OsKeyStore {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }
}

impl Default for OsKeyStore {
    fn default() -> Self {
        Self::new("com.maestroai.app", "database-key-v1")
    }
}

impl DatabaseKeyStore for OsKeyStore {
    fn load_or_create(&self) -> Result<DatabaseKey, KeyStoreError> {
        let _creation_guard = lock_key_creation();
        let entry = keyring::Entry::new(&self.service, &self.account)
            .map_err(|error| KeyStoreError::SecureStore(error.to_string()))?;

        match entry.get_password() {
            Ok(encoded) => DatabaseKey::decode(&encoded),
            Err(keyring::Error::NoEntry) => {
                let key = DatabaseKey::generate();
                entry
                    .set_password(&key.encode())
                    .map_err(|error| KeyStoreError::SecureStore(error.to_string()))?;
                Ok(key)
            }
            Err(error) => Err(KeyStoreError::SecureStore(error.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PassphraseKeyStore {
    wrapping_file: PathBuf,
    passphrase: Zeroizing<String>,
}

impl PassphraseKeyStore {
    /// Creates a passphrase-backed key store without persisting the passphrase.
    ///
    /// # Errors
    ///
    /// Returns [`KeyStoreError::EmptyPassphrase`] when no passphrase is
    /// supplied.
    pub fn new(
        wrapping_file: impl Into<PathBuf>,
        passphrase: String,
    ) -> Result<Self, KeyStoreError> {
        if passphrase.is_empty() {
            return Err(KeyStoreError::EmptyPassphrase);
        }
        Ok(Self {
            wrapping_file: wrapping_file.into(),
            passphrase: Zeroizing::new(passphrase),
        })
    }

    fn create_wrapped_key(
        &self,
        path: &Path,
        parent: &VerifiedParent,
    ) -> Result<DatabaseKey, KeyStoreError> {
        let key = DatabaseKey::generate();
        let mut salt = [0_u8; 16];
        let mut nonce = [0_u8; 24];
        rand::rng().fill_bytes(&mut salt);
        rand::rng().fill_bytes(&mut nonce);

        let wrapping_key = derive_wrapping_key(&self.passphrase, &salt)?;
        let cipher = XChaCha20Poly1305::new_from_slice(wrapping_key.as_ref())
            .map_err(|error| KeyStoreError::Encryption(error.to_string()))?;
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: key.expose(),
                    aad: WRAPPING_AAD,
                },
            )
            .map_err(|error| KeyStoreError::Encryption(error.to_string()))?;
        let envelope = WrappedKeyEnvelope {
            version: 1,
            salt: STANDARD_NO_PAD.encode(salt),
            nonce: STANDARD_NO_PAD.encode(nonce),
            ciphertext: STANDARD_NO_PAD.encode(ciphertext),
        };
        match publish_atomically(path, parent, &serde_json::to_vec(&envelope)?)? {
            Publication::Published => Ok(key),
            Publication::Existing => {
                // Do not return key material that lost the publication race.
                // DatabaseKey zeroizes its allocation when dropped.
                drop(key);
                self.decrypt_wrapped_key(path)
            }
        }
    }

    fn decrypt_wrapped_key(&self, path: &Path) -> Result<DatabaseKey, KeyStoreError> {
        let bytes = read_envelope(path)?;
        let envelope: WrappedKeyEnvelope = serde_json::from_slice(&bytes)?;
        if envelope.version != 1 {
            return Err(KeyStoreError::UnsupportedEnvelopeVersion(envelope.version));
        }

        let salt = decode_fixed::<16>("salt", &envelope.salt)?;
        let nonce = decode_fixed::<24>("nonce", &envelope.nonce)?;
        let ciphertext = STANDARD_NO_PAD
            .decode(&envelope.ciphertext)
            .map_err(|error| KeyStoreError::InvalidEncoding(error.to_string()))?;
        let wrapping_key = derive_wrapping_key(&self.passphrase, &salt)?;
        let cipher = XChaCha20Poly1305::new_from_slice(wrapping_key.as_ref())
            .map_err(|error| KeyStoreError::Encryption(error.to_string()))?;
        let plaintext = cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: WRAPPING_AAD,
                },
            )
            .map_err(|_| KeyStoreError::IncorrectPassphraseOrCorruptEnvelope)?;
        DatabaseKey::from_bytes(plaintext)
    }
}

impl DatabaseKeyStore for PassphraseKeyStore {
    fn load_or_create(&self) -> Result<DatabaseKey, KeyStoreError> {
        let _creation_guard = lock_key_creation();
        let (parent, path) = verified_wrapping_path(&self.wrapping_file)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => self.decrypt_wrapped_key(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.create_wrapped_key(&path, &parent)
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WrappedKeyEnvelope {
    version: u8,
    salt: String,
    nonce: String,
    ciphertext: String,
}

fn derive_wrapping_key(
    passphrase: &str,
    salt: &[u8],
) -> Result<Zeroizing<[u8; KEY_BYTES]>, KeyStoreError> {
    let mut output = Zeroizing::new([0_u8; KEY_BYTES]);
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, output.as_mut())
        .map_err(|error| KeyStoreError::KeyDerivation(error.to_string()))?;
    Ok(output)
}

fn decode_fixed<const SIZE: usize>(
    name: &'static str,
    value: &str,
) -> Result<[u8; SIZE], KeyStoreError> {
    let bytes = STANDARD_NO_PAD
        .decode(value)
        .map_err(|error| KeyStoreError::InvalidEncoding(error.to_string()))?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| KeyStoreError::InvalidEnvelopeField {
            name,
            expected: SIZE,
            actual: bytes.len(),
        })
}

fn lock_key_creation() -> MutexGuard<'static, ()> {
    PROCESS_KEY_CREATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
struct VerifiedParent {
    path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl VerifiedParent {
    fn verify_unchanged(&self) -> Result<(), KeyStoreError> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(KeyStoreError::UnsafeFilesystemPath(
                self.path.display().to_string(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.dev() != self.device || metadata.ino() != self.inode {
                return Err(KeyStoreError::FilesystemPathChanged(
                    self.path.display().to_string(),
                ));
            }
        }
        Ok(())
    }
}

fn verified_wrapping_path(path: &Path) -> Result<(VerifiedParent, PathBuf), KeyStoreError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| KeyStoreError::UnsafeFilesystemPath(path.display().to_string()))?;
    let original_parent = path.parent().filter(|value| !value.as_os_str().is_empty());
    let original_parent = original_parent.unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(original_parent)?;

    let unresolved_metadata = fs::symlink_metadata(original_parent)?;
    if unresolved_metadata.file_type().is_symlink() || !unresolved_metadata.is_dir() {
        return Err(KeyStoreError::UnsafeFilesystemPath(
            original_parent.display().to_string(),
        ));
    }

    let parent_path = fs::canonicalize(original_parent)?;
    let metadata = fs::symlink_metadata(&parent_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(KeyStoreError::UnsafeFilesystemPath(
            parent_path.display().to_string(),
        ));
    }
    restrict_directory(&parent_path)?;

    #[cfg(unix)]
    let parent = {
        use std::os::unix::fs::MetadataExt;
        VerifiedParent {
            path: parent_path.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    };
    #[cfg(not(unix))]
    let parent = VerifiedParent {
        path: parent_path.clone(),
    };
    parent.verify_unchanged()?;

    let resolved_path = parent_path.join(file_name);
    Ok((parent, resolved_path))
}

fn read_envelope(path: &Path) -> Result<Vec<u8>, KeyStoreError> {
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(KeyStoreError::UnsafeFilesystemPath(
            path.display().to_string(),
        ));
    }
    if path_metadata.len() > MAX_ENVELOPE_BYTES as u64 {
        return Err(KeyStoreError::EnvelopeTooLarge {
            actual: path_metadata.len(),
            maximum: MAX_ENVELOPE_BYTES,
        });
    }

    let file = OpenOptions::new().read(true).open(path)?;
    let opened_metadata = file.metadata()?;
    if !same_file(&path_metadata, &opened_metadata) {
        return Err(KeyStoreError::FilesystemPathChanged(
            path.display().to_string(),
        ));
    }

    let capacity = usize::try_from(path_metadata.len()).unwrap_or(MAX_ENVELOPE_BYTES);
    let mut bytes = Vec::with_capacity(capacity);
    file.take((MAX_ENVELOPE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(KeyStoreError::EnvelopeTooLarge {
            actual: bytes.len() as u64,
            maximum: MAX_ENVELOPE_BYTES,
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file(first: &fs::Metadata, second: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    first.dev() == second.dev() && first.ino() == second.ino()
}

#[cfg(not(unix))]
fn same_file(_first: &fs::Metadata, _second: &fs::Metadata) -> bool {
    true
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum Publication {
    Published,
    Existing,
}

fn publish_atomically(
    path: &Path,
    parent: &VerifiedParent,
    contents: &[u8],
) -> Result<Publication, KeyStoreError> {
    parent.verify_unchanged()?;
    let (temporary, mut file) = create_unique_temporary(&parent.path)?;
    let cleanup = TemporaryCleanup::new(temporary);
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);

    parent.verify_unchanged()?;
    match fs::hard_link(&cleanup.path, path) {
        Ok(()) => {
            let source_metadata = fs::symlink_metadata(&cleanup.path)?;
            let published_metadata = fs::symlink_metadata(path)?;
            if !same_file(&source_metadata, &published_metadata) {
                return Err(KeyStoreError::FilesystemPathChanged(
                    path.display().to_string(),
                ));
            }
            cleanup.remove()?;
            sync_directory(&parent.path)?;
            Ok(Publication::Published)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            cleanup.remove()?;
            sync_directory(&parent.path)?;
            Ok(Publication::Existing)
        }
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug)]
struct TemporaryCleanup {
    path: PathBuf,
    armed: bool,
}

impl TemporaryCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn remove(mut self) -> Result<(), std::io::Error> {
        fs::remove_file(&self.path)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for TemporaryCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_unique_temporary(parent: &Path) -> Result<(PathBuf, fs::File), KeyStoreError> {
    for _ in 0..TEMP_FILE_ATTEMPTS {
        let mut random = [0_u8; 16];
        rand::rng().fill_bytes(&mut random);
        let temporary = parent.join(format!(".maestro-key-{}.tmp", hex::encode(random)));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(KeyStoreError::TemporaryFileExhausted)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum KeyStoreError {
    #[error("a non-empty passphrase is required")]
    EmptyPassphrase,
    #[error("database key encryption failed: {0}")]
    Encryption(String),
    #[error("database key derivation failed: {0}")]
    KeyDerivation(String),
    #[error("incorrect passphrase or corrupt key envelope")]
    IncorrectPassphraseOrCorruptEnvelope,
    #[error("wrapped key envelope is {actual} bytes; maximum is {maximum} bytes")]
    EnvelopeTooLarge { actual: u64, maximum: usize },
    #[error("secure storage filesystem path changed while it was in use: {0}")]
    FilesystemPathChanged(String),
    #[error("wrapped key field {name} has length {actual}; expected {expected}")]
    InvalidEnvelopeField {
        name: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("database key has invalid length {0}; expected 32 bytes")]
    InvalidKeyLength(usize),
    #[error("database key encoding is invalid: {0}")]
    InvalidEncoding(String),
    #[error("OS secure storage is unavailable: {0}; passphrase unlock is required")]
    SecureStore(String),
    #[error("could not allocate a unique secure temporary key file")]
    TemporaryFileExhausted,
    #[error("refusing unsafe secure storage filesystem path: {0}")]
    UnsafeFilesystemPath(String),
    #[error("wrapped key envelope version {0} is unsupported")]
    UnsupportedEnvelopeVersion(u8),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("wrapped key serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        sync::{Arc, Barrier},
        time::{Duration, Instant},
    };

    use tempfile::tempdir;

    use super::{
        DatabaseKey, DatabaseKeyStore, KEY_BYTES, KeyStoreError, MAX_ENVELOPE_BYTES,
        PassphraseKeyStore, Publication, publish_atomically, verified_wrapping_path,
    };

    const SUBPROCESS_ROOT: &str = "MAESTRO_STORAGE_TEST_ROOT";
    const SUBPROCESS_ID: &str = "MAESTRO_STORAGE_TEST_ID";

    #[test]
    fn generated_keys_have_required_entropy_width() {
        let first = DatabaseKey::generate();
        let second = DatabaseKey::generate();

        assert_eq!(first.expose().len(), 32);
        assert_ne!(first.expose(), second.expose());
        assert_eq!(format!("{first:?}"), "DatabaseKey([REDACTED])");
    }

    #[test]
    fn passphrase_store_round_trips_without_persisting_key() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("database-key.json");
        let original = PassphraseKeyStore::new(&path, "correct horse battery staple".into())
            .expect("valid key store")
            .load_or_create()
            .expect("key created");
        let restored = PassphraseKeyStore::new(&path, "correct horse battery staple".into())
            .expect("valid key store")
            .load_or_create()
            .expect("key restored");

        assert_eq!(original.expose(), restored.expose());
        let envelope = std::fs::read(path).expect("envelope exists");
        assert!(
            !envelope
                .windows(KEY_BYTES)
                .any(|window| window == original.expose())
        );
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("database-key.json");
        PassphraseKeyStore::new(&path, "first passphrase".into())
            .expect("valid key store")
            .load_or_create()
            .expect("key created");

        let result = PassphraseKeyStore::new(&path, "different passphrase".into())
            .expect("valid key store")
            .load_or_create();
        assert!(result.is_err());
    }

    #[test]
    fn concurrent_creators_observe_one_key() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("database-key.json");
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();

        for _ in 0..8 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                PassphraseKeyStore::new(path, "shared passphrase".into())
                    .expect("valid key store")
                    .load_or_create()
                    .expect("key loads")
            }));
        }

        let keys: Vec<DatabaseKey> = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker completes"))
            .collect();
        assert!(
            keys.iter()
                .all(|candidate| candidate.expose() == keys[0].expose())
        );
    }

    #[test]
    fn cross_process_creators_return_only_the_persisted_winner() {
        let directory = tempdir().expect("temporary directory");
        let executable = std::env::current_exe().expect("current test executable");
        let worker_count = 6;
        let mut workers = Vec::new();

        for worker_id in 0..worker_count {
            let child = Command::new(&executable)
                .arg("key::tests::cross_process_key_creator_helper")
                .arg("--exact")
                .arg("--ignored")
                .arg("--test-threads=1")
                .env(SUBPROCESS_ROOT, directory.path())
                .env(SUBPROCESS_ID, worker_id.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("worker subprocess starts");
            workers.push(child);
        }

        wait_until(
            || {
                (0..worker_count)
                    .all(|worker_id| directory.path().join(format!("ready-{worker_id}")).exists())
            },
            "subprocesses did not reach the publication barrier",
        );
        std::fs::write(directory.path().join("start"), b"go").expect("barrier releases");

        for worker in workers {
            let output = worker.wait_with_output().expect("worker can be joined");
            assert!(
                output.status.success(),
                "worker failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let returned_keys: Vec<String> = (0..worker_count)
            .map(|worker_id| {
                std::fs::read_to_string(directory.path().join(format!("result-{worker_id}")))
                    .expect("worker returned a key")
            })
            .collect();
        assert!(returned_keys.iter().all(|key| key == &returned_keys[0]));

        let persisted = PassphraseKeyStore::new(
            directory.path().join("database-key.json"),
            "shared passphrase".into(),
        )
        .expect("valid key store")
        .load_or_create()
        .expect("winner reloads");
        assert_eq!(returned_keys[0], persisted.encode());
    }

    #[test]
    #[ignore = "subprocess helper invoked explicitly by the cross-process race test"]
    fn cross_process_key_creator_helper() {
        let Some(root) = std::env::var_os(SUBPROCESS_ROOT) else {
            return;
        };
        let worker_id = std::env::var(SUBPROCESS_ID).expect("worker id is set");
        let root = std::path::PathBuf::from(root);
        std::fs::write(root.join(format!("ready-{worker_id}")), b"ready")
            .expect("worker becomes ready");
        wait_until(
            || root.join("start").exists(),
            "parent did not release subprocess barrier",
        );

        let key =
            PassphraseKeyStore::new(root.join("database-key.json"), "shared passphrase".into())
                .expect("valid key store")
                .load_or_create()
                .expect("competing key loads");
        std::fs::write(root.join(format!("result-{worker_id}")), key.encode())
            .expect("worker records returned key");
    }

    #[test]
    fn oversized_envelope_is_rejected_before_unbounded_allocation() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("database-key.json");
        std::fs::write(&path, vec![b'x'; MAX_ENVELOPE_BYTES + 1]).expect("oversized file");

        let result = PassphraseKeyStore::new(&path, "passphrase".into())
            .expect("valid key store")
            .load_or_create();
        assert!(matches!(
            result,
            Err(KeyStoreError::EnvelopeTooLarge {
                actual,
                maximum: MAX_ENVELOPE_BYTES,
            }) if actual == (MAX_ENVELOPE_BYTES + 1) as u64
        ));
    }

    #[cfg(unix)]
    #[test]
    fn wrapping_file_symlink_is_rejected_without_touching_target() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let victim = directory.path().join("victim");
        let path = directory.path().join("database-key.json");
        std::fs::write(&victim, b"do not change").expect("victim exists");
        symlink(&victim, &path).expect("symlink exists");

        let result = PassphraseKeyStore::new(&path, "passphrase".into())
            .expect("valid key store")
            .load_or_create();
        assert!(matches!(
            result,
            Err(KeyStoreError::UnsafeFilesystemPath(_))
        ));
        assert_eq!(
            std::fs::read(&victim).expect("victim readable"),
            b"do not change"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_direct_parent_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let real_parent = directory.path().join("real");
        let linked_parent = directory.path().join("linked");
        std::fs::create_dir(&real_parent).expect("real parent");
        symlink(&real_parent, &linked_parent).expect("linked parent");

        let result =
            PassphraseKeyStore::new(linked_parent.join("database-key.json"), "passphrase".into())
                .expect("valid key store")
                .load_or_create();
        assert!(matches!(
            result,
            Err(KeyStoreError::UnsafeFilesystemPath(_))
        ));
        assert!(!real_parent.join("database-key.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_predictable_temp_symlink_cannot_redirect_creation() {
        use std::os::unix::fs::{MetadataExt, symlink};

        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("database-key.json");
        let old_shared_temp = path.with_extension("tmp");
        let victim = directory.path().join("victim");
        std::fs::write(&victim, b"do not change").expect("victim exists");
        symlink(&victim, &old_shared_temp).expect("stale malicious temp exists");

        PassphraseKeyStore::new(&path, "passphrase".into())
            .expect("valid key store")
            .load_or_create()
            .expect("key created through unique temp");

        assert_eq!(
            std::fs::read(&victim).expect("victim readable"),
            b"do not change"
        );
        let metadata = std::fs::metadata(&path).expect("envelope metadata");
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_ne!(metadata.ino(), std::fs::metadata(&victim).unwrap().ino());
    }

    #[cfg(unix)]
    #[test]
    fn no_clobber_publication_cannot_replace_hostile_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("database-key.json");
        let victim = directory.path().join("victim");
        std::fs::write(&victim, b"do not change").expect("victim exists");
        let (parent, resolved_path) = verified_wrapping_path(&path).expect("verified parent");
        symlink(&victim, &resolved_path).expect("hostile destination exists");

        let publication = publish_atomically(&resolved_path, &parent, b"complete envelope")
            .expect("existing path is a non-clobber loss");

        assert_eq!(publication, Publication::Existing);
        assert!(
            std::fs::symlink_metadata(&resolved_path)
                .expect("symlink remains")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(&victim).expect("victim readable"),
            b"do not change"
        );
    }

    #[test]
    fn stale_unique_temp_from_interrupted_write_does_not_block_creation() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("database-key.json");
        let stale = directory
            .path()
            .join(".maestro-key-00000000000000000000000000000000.tmp");
        std::fs::write(&stale, b"partial envelope").expect("stale temp");

        PassphraseKeyStore::new(&path, "passphrase".into())
            .expect("valid key store")
            .load_or_create()
            .expect("key created despite stale temp");

        assert!(path.exists());
        assert_eq!(
            std::fs::read(stale).expect("stale temp remains isolated"),
            b"partial envelope"
        );
    }

    fn wait_until(mut predicate: impl FnMut() -> bool, failure: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !predicate() {
            assert!(Instant::now() < deadline, "{failure}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
