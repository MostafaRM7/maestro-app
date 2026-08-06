use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use rustix::fs::{AtFlags, Mode, OFlags, fsync, openat, renameat, unlinkat};
use sha2::{Digest, Sha256};

use crate::{ProjectError, WorkspaceRoots, auth::AuthorizedDirectory};

pub const DEFAULT_MAXIMUM_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const MAXIMUM_TEXT_BYTES: usize = 64 * 1024 * 1024;
static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct FileFingerprint([u8; 32]);

impl std::fmt::Debug for FileFingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FileFingerprint(")?;
        for byte in &self.0[..8] {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str("…)")
    }
}

impl FileFingerprint {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TextFile {
    pub path: PathBuf,
    pub text: String,
    pub fingerprint: FileFingerprint,
    pub bytes: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SaveResult {
    pub fingerprint: FileFingerprint,
    pub bytes: usize,
}

#[derive(Debug, Clone)]
pub struct FileService {
    roots: Arc<WorkspaceRoots>,
    maximum_text_bytes: usize,
}

impl FileService {
    pub fn new(roots: Arc<WorkspaceRoots>) -> Self {
        Self {
            roots,
            maximum_text_bytes: DEFAULT_MAXIMUM_TEXT_BYTES,
        }
    }

    /// Creates a file service with a caller-selected text size limit.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::InvalidLimit`] when the limit is zero or above
    /// the supported 64 MiB ceiling.
    pub fn with_maximum_text_bytes(
        roots: Arc<WorkspaceRoots>,
        maximum_text_bytes: usize,
    ) -> Result<Self, ProjectError> {
        if maximum_text_bytes == 0 || maximum_text_bytes > MAXIMUM_TEXT_BYTES {
            return Err(ProjectError::InvalidLimit);
        }
        Ok(Self {
            roots,
            maximum_text_bytes,
        })
    }

    /// Reads a bounded, regular UTF-8 text file within the authorized roots.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the path is unauthorized or unsafe, the
    /// file is not regular UTF-8 text, the size limit is exceeded, the file
    /// changes during the read, or local I/O fails.
    pub fn read_text(&self, path: &Path) -> Result<TextFile, ProjectError> {
        let (bytes, _) = self.read_verified_bytes(path, self.maximum_text_bytes)?;
        if looks_binary(&bytes) {
            return Err(ProjectError::BinaryFile);
        }
        let text = String::from_utf8(bytes).map_err(|_| ProjectError::InvalidUtf8)?;
        let bytes = text.len();
        Ok(TextFile {
            path: self.roots.authorize(path)?.canonical,
            fingerprint: fingerprint(text.as_bytes()),
            text,
            bytes,
        })
    }

    /// Atomically replaces a text file when its current fingerprint matches.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the path is unauthorized or unsafe, the
    /// replacement exceeds the size limit, the file changed after it was
    /// opened, or the read/write operation fails.
    pub fn save_text(
        &self,
        path: &Path,
        text: &str,
        expected: FileFingerprint,
    ) -> Result<SaveResult, ProjectError> {
        if text.len() > self.maximum_text_bytes {
            return Err(ProjectError::FileTooLarge {
                actual: text.len() as u64,
                maximum: self.maximum_text_bytes,
            });
        }
        let (parent, name) = self.roots.open_parent(path)?;
        let (current, current_metadata) = read_from_parent(
            &parent,
            &name,
            self.maximum_text_bytes,
            "reading file before save",
        )?;
        if fingerprint(&current) != expected {
            return Err(ProjectError::SaveConflict);
        }

        let temporary_name = unique_temporary_name();
        let temporary_fd = openat(
            &parent.file,
            temporary_name.as_os_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| rustix_error("creating atomic save file", error))?;
        let mut temporary = File::from(temporary_fd);
        let result = (|| {
            temporary
                .set_permissions(current_metadata.permissions())
                .map_err(|error| ProjectError::io("preserving file permissions", error))?;
            temporary
                .write_all(text.as_bytes())
                .map_err(|error| ProjectError::io("writing atomic save file", error))?;
            temporary
                .sync_all()
                .map_err(|error| ProjectError::io("syncing atomic save file", error))?;

            let (latest, _) = read_from_parent(
                &parent,
                &name,
                self.maximum_text_bytes,
                "rechecking file before save",
            )?;
            if fingerprint(&latest) != expected {
                return Err(ProjectError::SaveConflict);
            }

            renameat(
                &parent.file,
                temporary_name.as_os_str(),
                &parent.file,
                name.as_os_str(),
            )
            .map_err(|error| rustix_error("publishing atomic save", error))?;
            fsync(&parent.file).map_err(|error| rustix_error("syncing save directory", error))?;
            Ok(SaveResult {
                fingerprint: fingerprint(text.as_bytes()),
                bytes: text.len(),
            })
        })();
        drop(temporary);
        if result.is_err() {
            let _ = unlinkat(&parent.file, temporary_name.as_os_str(), AtFlags::empty());
        }
        result
    }

    pub(crate) fn read_search_bytes(
        &self,
        path: &Path,
        maximum: usize,
    ) -> Result<Vec<u8>, ProjectError> {
        let (bytes, _) = self.read_verified_bytes(path, maximum)?;
        if looks_binary(&bytes) {
            Err(ProjectError::BinaryFile)
        } else {
            Ok(bytes)
        }
    }

    fn read_verified_bytes(
        &self,
        path: &Path,
        maximum: usize,
    ) -> Result<(Vec<u8>, fs::Metadata), ProjectError> {
        if maximum == 0 || maximum > MAXIMUM_TEXT_BYTES {
            return Err(ProjectError::InvalidLimit);
        }
        let (parent, name) = self.roots.open_parent(path)?;
        let (bytes, initial) =
            read_from_parent(&parent, &name, maximum, "reading authorized file")?;
        let reopened = open_regular(&parent, &name, "reopening authorized file")?;
        let reopened_metadata = reopened
            .metadata()
            .map_err(|error| ProjectError::io("reading reopened file metadata", error))?;
        if !same_file(&initial, &reopened_metadata)
            || metadata_changed(&initial, &reopened_metadata)
        {
            return Err(ProjectError::FileChangedDuringRead);
        }
        Ok((bytes, initial))
    }
}

fn read_from_parent(
    parent: &AuthorizedDirectory,
    name: &OsStr,
    maximum: usize,
    operation: &'static str,
) -> Result<(Vec<u8>, fs::Metadata), ProjectError> {
    let mut file = open_regular(parent, name, operation)?;
    let before = file
        .metadata()
        .map_err(|error| ProjectError::io("reading file metadata", error))?;
    if before.len() > maximum as u64 {
        return Err(ProjectError::FileTooLarge {
            actual: before.len(),
            maximum,
        });
    }
    let capacity = usize::try_from(before.len())
        .unwrap_or(maximum)
        .min(maximum);
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| ProjectError::io(operation, error))?;
    if bytes.len() > maximum {
        return Err(ProjectError::FileTooLarge {
            actual: bytes.len() as u64,
            maximum,
        });
    }
    let after = file
        .metadata()
        .map_err(|error| ProjectError::io("rechecking file metadata", error))?;
    if metadata_changed(&before, &after) {
        return Err(ProjectError::FileChangedDuringRead);
    }
    Ok((bytes, after))
}

fn open_regular(
    parent: &AuthorizedDirectory,
    name: &OsStr,
    operation: &'static str,
) -> Result<File, ProjectError> {
    let descriptor = openat(
        &parent.file,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::LOOP {
            ProjectError::UnsafePath
        } else {
            rustix_error(operation, error)
        }
    })?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(|error| ProjectError::io("reading file metadata", error))?
        .is_file()
    {
        return Err(ProjectError::NotRegularFile);
    }
    Ok(file)
}

fn fingerprint(bytes: &[u8]) -> FileFingerprint {
    FileFingerprint(Sha256::digest(bytes).into())
}

fn looks_binary(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return true;
    }
    let controls = bytes
        .iter()
        .filter(|byte| **byte < 0x20 && !matches!(**byte, b'\n' | b'\r' | b'\t' | 0x0c))
        .count();
    !bytes.is_empty() && controls.saturating_mul(10) > bytes.len() * 3
}

fn unique_temporary_name() -> std::ffi::OsString {
    let counter = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(".maestro-save-{}-{counter}.tmp", std::process::id()).into()
}

#[cfg(unix)]
fn same_file(first: &fs::Metadata, second: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    first.dev() == second.dev() && first.ino() == second.ino()
}

#[cfg(unix)]
fn metadata_changed(first: &fs::Metadata, second: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    !same_file(first, second)
        || first.len() != second.len()
        || first.mtime() != second.mtime()
        || first.mtime_nsec() != second.mtime_nsec()
}

fn rustix_error(operation: &'static str, error: rustix::io::Errno) -> ProjectError {
    ProjectError::io(
        operation,
        std::io::Error::from_raw_os_error(error.raw_os_error()),
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    use super::FileService;
    use crate::{ProjectError, WorkspaceRoots};

    #[test]
    fn bounded_text_read_distinguishes_text_binary_utf8_and_size() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("text.txt"), "hello ✓").unwrap();
        fs::write(root.join("binary.bin"), b"abc\0def").unwrap();
        fs::write(root.join("invalid.txt"), [0xff, 0xfe]).unwrap();
        fs::write(root.join("large.txt"), b"12345").unwrap();
        let roots = Arc::new(WorkspaceRoots::new([&root]).unwrap());
        let files = FileService::with_maximum_text_bytes(roots, 4).unwrap();

        assert!(matches!(
            files.read_text(&root.join("text.txt")),
            Err(ProjectError::FileTooLarge { .. })
        ));
        let files = FileService::new(Arc::new(WorkspaceRoots::new([&root]).unwrap()));
        assert_eq!(
            files.read_text(&root.join("text.txt")).unwrap().text,
            "hello ✓"
        );
        assert!(matches!(
            files.read_text(&root.join("binary.bin")),
            Err(ProjectError::BinaryFile)
        ));
        assert!(matches!(
            files.read_text(&root.join("invalid.txt")),
            Err(ProjectError::InvalidUtf8)
        ));
    }

    #[test]
    fn atomic_save_detects_concurrent_content_change_and_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempdir().unwrap();
        let root = temporary.path().join("root");
        let path = root.join("file.txt");
        fs::create_dir(&root).unwrap();
        fs::write(&path, "original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let files = FileService::new(Arc::new(WorkspaceRoots::new([&root]).unwrap()));
        let opened = files.read_text(&path).unwrap();

        fs::write(&path, "external change").unwrap();
        assert!(matches!(
            files.save_text(&path, "overwrite", opened.fingerprint),
            Err(ProjectError::SaveConflict)
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "external change");

        let reopened = files.read_text(&path).unwrap();
        let saved = files
            .save_text(&path, "maestro change", reopened.fingerprint)
            .unwrap();
        assert_eq!(saved.bytes, 14);
        assert_eq!(fs::read_to_string(&path).unwrap(), "maestro change");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".maestro-save-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn traversal_and_leaf_or_parent_symlink_escapes_are_rejected() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), "secret").unwrap();
        symlink(outside.join("secret"), root.join("leaf-link")).unwrap();
        symlink(&outside, root.join("parent-link")).unwrap();
        let files = FileService::new(Arc::new(WorkspaceRoots::new([&root]).unwrap()));

        assert!(matches!(
            files.read_text(&root.join("../outside/secret")),
            Err(ProjectError::UnauthorizedPath)
        ));
        assert!(matches!(
            files.read_text(&root.join("leaf-link")),
            Err(ProjectError::UnsafePath)
        ));
        assert!(matches!(
            files.read_text(&root.join("parent-link/secret")),
            Err(ProjectError::UnsafePath | ProjectError::NotDirectory)
        ));
    }
}
