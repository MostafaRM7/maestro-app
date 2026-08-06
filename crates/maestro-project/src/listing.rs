use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use ignore::WalkBuilder;

use crate::{ProjectError, WorkspaceRoots, auth::path_name};

pub const DEFAULT_DIRECTORY_PAGE_SIZE: usize = 200;
pub const MAXIMUM_DIRECTORY_PAGE_SIZE: usize = 1_000;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DirectoryCursor(pub u64);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DirectoryEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub path: PathBuf,
    pub display_name: String,
    pub kind: DirectoryEntryKind,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ListingOptions {
    pub maximum_entries: usize,
    pub include_hidden: bool,
}

impl Default for ListingOptions {
    fn default() -> Self {
        Self {
            maximum_entries: DEFAULT_DIRECTORY_PAGE_SIZE,
            include_hidden: false,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirectoryPage {
    pub directory: PathBuf,
    pub entries: Vec<DirectoryEntry>,
    pub next_cursor: Option<DirectoryCursor>,
}

#[derive(Debug, Clone)]
pub struct DirectoryService {
    roots: Arc<WorkspaceRoots>,
}

impl DirectoryService {
    pub fn new(roots: Arc<WorkspaceRoots>) -> Self {
        Self { roots }
    }

    /// Lists one directory level without reading file contents or building an
    /// index. Pagination restarts the bounded walker and skips the opaque
    /// number of already emitted, ignore-filtered entries.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the page size is invalid, the directory
    /// is unauthorized or unsafe, the directory changes during listing, or
    /// metadata and traversal operations fail.
    pub fn list(
        &self,
        directory: &Path,
        cursor: DirectoryCursor,
        options: ListingOptions,
    ) -> Result<DirectoryPage, ProjectError> {
        if options.maximum_entries == 0 || options.maximum_entries > MAXIMUM_DIRECTORY_PAGE_SIZE {
            return Err(ProjectError::InvalidLimit);
        }
        let authorized = self.roots.open_directory(directory)?;
        let before = authorized
            .file
            .metadata()
            .map_err(|error| ProjectError::io("reading directory metadata", error))?;

        let mut builder = WalkBuilder::new(&authorized.canonical);
        builder
            .max_depth(Some(1))
            .follow_links(false)
            .hidden(!options.include_hidden)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(false)
            .parents(true)
            .ignore(true)
            .filter_entry(|entry| {
                entry.depth() == 0 || path_name(entry.path()) != std::ffi::OsStr::new(".git")
            });

        let offset = usize::try_from(cursor.0).map_err(|_| ProjectError::InvalidLimit)?;
        let mut seen = 0_usize;
        let mut entries = Vec::with_capacity(options.maximum_entries);
        let mut has_more = false;
        for result in builder.build() {
            let entry = result.map_err(|error| {
                let kind = error
                    .io_error()
                    .map_or(std::io::ErrorKind::Other, std::io::Error::kind);
                ProjectError::Io {
                    operation: "listing directory",
                    kind,
                }
            })?;
            if entry.depth() == 0 {
                continue;
            }
            if seen < offset {
                seen += 1;
                continue;
            }
            if entries.len() == options.maximum_entries {
                has_more = true;
                break;
            }
            seen += 1;
            entries.push(directory_entry(&entry)?);
        }

        let after = authorized
            .file
            .metadata()
            .map_err(|error| ProjectError::io("rechecking directory metadata", error))?;
        if !same_directory(&before, &after) {
            return Err(ProjectError::FileChangedDuringRead);
        }
        let next_cursor = has_more.then(|| {
            DirectoryCursor(
                cursor
                    .0
                    .saturating_add(u64::try_from(entries.len()).unwrap_or(u64::MAX)),
            )
        });
        Ok(DirectoryPage {
            directory: authorized.canonical,
            entries,
            next_cursor,
        })
    }
}

fn directory_entry(entry: &ignore::DirEntry) -> Result<DirectoryEntry, ProjectError> {
    let file_type = entry.file_type();
    let kind = match file_type {
        Some(value) if value.is_dir() => DirectoryEntryKind::Directory,
        Some(value) if value.is_file() => DirectoryEntryKind::File,
        Some(value) if value.is_symlink() => DirectoryEntryKind::Symlink,
        _ => DirectoryEntryKind::Other,
    };
    let bytes = if kind == DirectoryEntryKind::File {
        Some(
            fs::symlink_metadata(entry.path())
                .map_err(|error| ProjectError::io("reading directory entry metadata", error))?
                .len(),
        )
    } else {
        None
    };
    Ok(DirectoryEntry {
        path: entry.path().to_path_buf(),
        display_name: path_name(entry.path()).to_string_lossy().into_owned(),
        kind,
        bytes,
    })
}

#[cfg(unix)]
fn same_directory(first: &fs::Metadata, second: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    first.dev() == second.dev() && first.ino() == second.ino()
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    use super::{DirectoryCursor, DirectoryEntryKind, DirectoryService, ListingOptions};
    use crate::WorkspaceRoots;

    #[test]
    fn lazy_pages_are_bounded_ignore_aware_and_preserve_malicious_names() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("repo");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.join("ignored.txt"), "ignored").unwrap();
        fs::write(root.join(".hidden"), "hidden").unwrap();
        fs::write(root.join("normal.txt"), "normal").unwrap();
        fs::write(root.join("<script>\n--name.txt"), "hostile name").unwrap();
        for index in 0..25 {
            fs::write(root.join(format!("file-{index:02}.txt")), "content").unwrap();
        }
        let service = DirectoryService::new(Arc::new(WorkspaceRoots::new([&root]).unwrap()));

        let first = service
            .list(
                &root,
                DirectoryCursor::default(),
                ListingOptions {
                    maximum_entries: 5,
                    include_hidden: false,
                },
            )
            .unwrap();
        assert_eq!(first.entries.len(), 5);
        assert!(first.next_cursor.is_some());

        let mut names = first
            .entries
            .iter()
            .map(|entry| entry.display_name.clone())
            .collect::<Vec<_>>();
        let mut cursor = first.next_cursor;
        while let Some(next) = cursor {
            let page = service
                .list(
                    &root,
                    next,
                    ListingOptions {
                        maximum_entries: 5,
                        include_hidden: false,
                    },
                )
                .unwrap();
            names.extend(page.entries.iter().map(|entry| entry.display_name.clone()));
            cursor = page.next_cursor;
        }
        assert!(!names.iter().any(|name| name == "ignored.txt"));
        assert!(!names.iter().any(|name| name == ".hidden" || name == ".git"));
        assert!(names.iter().any(|name| name == "<script>\n--name.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn outside_symlink_is_visible_but_never_expandable() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("linked-outside")).unwrap();
        let roots = Arc::new(WorkspaceRoots::new([&root]).unwrap());
        let service = DirectoryService::new(Arc::clone(&roots));

        let page = service
            .list(&root, DirectoryCursor::default(), ListingOptions::default())
            .unwrap();
        assert!(page.entries.iter().any(|entry| {
            entry.display_name == "linked-outside" && entry.kind == DirectoryEntryKind::Symlink
        }));
        assert!(roots.open_directory(&root.join("linked-outside")).is_err());
    }
}
