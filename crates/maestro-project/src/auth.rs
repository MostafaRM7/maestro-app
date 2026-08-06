use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use rustix::fs::{Mode, OFlags, openat};

use crate::ProjectError;

#[derive(Debug)]
struct RootCapability {
    requested: PathBuf,
    canonical: PathBuf,
    directory: Arc<File>,
    input_index: usize,
}

/// Canonical, non-overlapping workspace roots backed by open directory
/// capabilities. Paths are resolved relative to these capabilities rather than
/// trusted frontend strings.
#[derive(Debug)]
pub struct WorkspaceRoots {
    roots: Vec<RootCapability>,
}

#[derive(Debug)]
pub(crate) struct AuthorizedPath<'a> {
    root: &'a RootCapability,
    pub(crate) canonical: PathBuf,
    components: Vec<OsString>,
}

#[derive(Debug)]
pub(crate) struct AuthorizedDirectory {
    pub(crate) canonical: PathBuf,
    pub(crate) file: File,
}

impl WorkspaceRoots {
    /// Canonicalizes and opens a deterministic set of non-overlapping roots.
    /// Symlinked root arguments are resolved once to their canonical target;
    /// duplicate or nested canonical roots are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when no roots are supplied or a root is
    /// relative, unavailable, unreadable, not a directory, duplicated, or
    /// nested inside another root.
    pub fn new<I, P>(roots: I) -> Result<Self, ProjectError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let requested = roots
            .into_iter()
            .map(|path| path.as_ref().to_path_buf())
            .collect::<Vec<_>>();
        if requested.is_empty() {
            return Err(ProjectError::EmptyRoots);
        }

        let mut capabilities = Vec::with_capacity(requested.len());
        for (index, path) in requested.iter().enumerate() {
            if !path.is_absolute() {
                return Err(ProjectError::RootNotAbsolute { index });
            }
            let requested = normalize_absolute(path)?;
            let canonical =
                fs::canonicalize(path).map_err(|_| ProjectError::RootUnavailable { index })?;
            let metadata =
                fs::metadata(&canonical).map_err(|_| ProjectError::RootUnavailable { index })?;
            if !metadata.is_dir() {
                return Err(ProjectError::RootNotDirectory { index });
            }
            if !root_has_access(&metadata) || fs::read_dir(&canonical).is_err() {
                return Err(ProjectError::RootUnreadable { index });
            }
            let directory = File::open(&canonical)
                .map_err(|error| ProjectError::io("opening workspace root", error))?;
            capabilities.push(RootCapability {
                requested,
                canonical,
                directory: Arc::new(directory),
                input_index: index,
            });
        }

        capabilities.sort_by(|left, right| left.canonical.cmp(&right.canonical));
        for first in 0..capabilities.len() {
            for second in (first + 1)..capabilities.len() {
                let left = &capabilities[first];
                let right = &capabilities[second];
                if left.canonical == right.canonical {
                    return Err(ProjectError::DuplicateRoot {
                        first: left.input_index.min(right.input_index),
                        second: left.input_index.max(right.input_index),
                    });
                }
                if right.canonical.starts_with(&left.canonical) {
                    return Err(ProjectError::NestedRoots {
                        outer: left.input_index,
                        inner: right.input_index,
                    });
                }
            }
        }

        Ok(Self {
            roots: capabilities,
        })
    }

    pub fn canonical_roots(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .map(|root| root.canonical.clone())
            .collect()
    }

    /// Opens an existing directory through the workspace-root capability and
    /// returns the canonical path that is safe to pass to a child process.
    /// Symlinks in the requested relative path are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the path is outside the workspace roots,
    /// is not an existing directory, or cannot be opened without following a
    /// symlink.
    #[cfg(unix)]
    pub fn canonical_directory(&self, requested: &Path) -> Result<PathBuf, ProjectError> {
        self.open_directory(requested)
            .map(|directory| directory.canonical)
    }

    pub(crate) fn authorize(&self, requested: &Path) -> Result<AuthorizedPath<'_>, ProjectError> {
        if !requested.is_absolute() {
            return Err(ProjectError::UnauthorizedPath);
        }
        let normalized = normalize_absolute(requested)?;
        let mut authorized_root = None;
        for root in &self.roots {
            for prefix in [root.canonical.as_path(), root.requested.as_path()] {
                if normalized.starts_with(prefix) {
                    let depth = prefix.components().count();
                    if authorized_root
                        .as_ref()
                        .is_none_or(|(_, _, matched_depth)| depth > *matched_depth)
                    {
                        authorized_root = Some((root, prefix, depth));
                    }
                }
            }
        }
        let (root, matched_prefix, _) = authorized_root.ok_or(ProjectError::UnauthorizedPath)?;
        let relative = normalized
            .strip_prefix(matched_prefix)
            .map_err(|_| ProjectError::UnauthorizedPath)?;
        let components = relative
            .components()
            .map(|component| match component {
                Component::Normal(value) => Ok(value.to_os_string()),
                _ => Err(ProjectError::UnsafePath),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AuthorizedPath {
            root,
            canonical: root.canonical.join(relative),
            components,
        })
    }

    #[cfg(unix)]
    pub(crate) fn open_directory(
        &self,
        requested: &Path,
    ) -> Result<AuthorizedDirectory, ProjectError> {
        let authorized = self.authorize(requested)?;
        let mut directory = openat(
            authorized.root.directory.as_ref(),
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| map_open_error("opening authorized directory", error))?;
        for component in &authorized.components {
            directory = openat(
                &directory,
                component.as_os_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| map_open_error("opening authorized directory", error))?;
        }
        let file = File::from(directory);
        if !file
            .metadata()
            .map_err(|error| ProjectError::io("reading directory metadata", error))?
            .is_dir()
        {
            return Err(ProjectError::NotDirectory);
        }
        Ok(AuthorizedDirectory {
            canonical: authorized.canonical,
            file,
        })
    }

    #[cfg(unix)]
    pub(crate) fn open_parent(
        &self,
        requested: &Path,
    ) -> Result<(AuthorizedDirectory, OsString), ProjectError> {
        let authorized = self.authorize(requested)?;
        let (file_name, parents) = authorized
            .components
            .split_last()
            .ok_or(ProjectError::NotRegularFile)?;
        let mut directory = openat(
            authorized.root.directory.as_ref(),
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| map_open_error("opening authorized parent", error))?;
        for component in parents {
            directory = openat(
                &directory,
                component.as_os_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| map_open_error("opening authorized parent", error))?;
        }
        let parent_path = authorized
            .canonical
            .parent()
            .ok_or(ProjectError::UnauthorizedPath)?
            .to_path_buf();
        Ok((
            AuthorizedDirectory {
                canonical: parent_path,
                file: File::from(directory),
            },
            file_name.clone(),
        ))
    }
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, ProjectError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(ProjectError::UnauthorizedPath);
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    if normalized.is_absolute() {
        Ok(normalized)
    } else {
        Err(ProjectError::UnauthorizedPath)
    }
}

#[cfg(unix)]
fn root_has_access(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o555 != 0
}

#[cfg(not(unix))]
fn root_has_access(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn map_open_error(operation: &'static str, error: rustix::io::Errno) -> ProjectError {
    use rustix::io::Errno;
    if matches!(error, Errno::LOOP) {
        ProjectError::UnsafePath
    } else if matches!(error, Errno::NOTDIR) {
        ProjectError::NotDirectory
    } else {
        ProjectError::io(
            operation,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    }
}

pub(crate) fn path_name(path: &Path) -> &OsStr {
    path.file_name().unwrap_or_else(|| OsStr::new(""))
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::tempdir;

    use super::WorkspaceRoots;
    use crate::ProjectError;

    #[test]
    fn roots_are_canonical_sorted_and_non_overlapping() {
        let temporary = tempdir().expect("temporary directory");
        let first = temporary.path().join("z-root");
        let second = temporary.path().join("a-root");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        let roots = WorkspaceRoots::new([&first, &second]).expect("valid roots");
        assert_eq!(
            roots.canonical_roots(),
            vec![
                fs::canonicalize(second).unwrap(),
                fs::canonicalize(first).unwrap()
            ]
        );
    }

    #[test]
    fn relative_duplicate_nested_missing_and_file_roots_are_rejected() {
        let temporary = tempdir().expect("temporary directory");
        let root = temporary.path().join("root");
        let nested = root.join("nested");
        let file = temporary.path().join("file");
        fs::create_dir_all(&nested).unwrap();
        fs::write(&file, b"file").unwrap();

        assert!(matches!(
            WorkspaceRoots::new(["relative"]),
            Err(ProjectError::RootNotAbsolute { .. })
        ));
        assert!(matches!(
            WorkspaceRoots::new([&root, &root]),
            Err(ProjectError::DuplicateRoot { .. })
        ));
        assert!(matches!(
            WorkspaceRoots::new([&nested, &root]),
            Err(ProjectError::NestedRoots { .. })
        ));
        assert!(matches!(
            WorkspaceRoots::new([temporary.path().join("missing")]),
            Err(ProjectError::RootUnavailable { .. })
        ));
        assert!(matches!(
            WorkspaceRoots::new([file]),
            Err(ProjectError::RootNotDirectory { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_roots_canonicalize_but_symlink_escape_is_not_authorized() {
        let temporary = tempdir().expect("temporary directory");
        let real = temporary.path().join("real");
        let alias = temporary.path().join("alias");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(&real).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&real, &alias).unwrap();
        symlink(&outside, real.join("escape")).unwrap();

        let roots = WorkspaceRoots::new([&alias]).expect("symlink root canonicalizes");
        let canonical_real = fs::canonicalize(&real).unwrap();
        assert_eq!(roots.canonical_roots(), vec![canonical_real.clone()]);
        assert_eq!(
            roots.open_directory(&alias).unwrap().canonical,
            canonical_real
        );
        assert!(matches!(
            roots.open_directory(&alias.join("escape")),
            Err(ProjectError::UnsafePath | ProjectError::NotDirectory)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_root_is_rejected_deterministically() {
        let temporary = tempdir().expect("temporary directory");
        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
        let result = WorkspaceRoots::new([&root]);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(matches!(result, Err(ProjectError::RootUnreadable { .. })));
    }
}
