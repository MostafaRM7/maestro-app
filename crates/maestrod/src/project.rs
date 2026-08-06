use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use maestro_domain::{ErrorCode, MaestroError, ProjectId, RequestId};
use maestro_project::{
    BranchState, CancellationFlag, DiffScope, DirectoryCursor, DirectoryEntryKind,
    DirectoryService, FileFingerprint, FileService, GitPath, GitService, GitStatusKind,
    ListingOptions, ProjectError, RepositorySearch, SearchMode, SearchOptions, WorkspaceRoots,
};
use maestro_protocol::{
    ProjectBranchState, ProjectDiffScope, ProjectDirectoryEntry, ProjectDirectoryEntryKind,
    ProjectDirectoryPage, ProjectFileSaved, ProjectGitDiff, ProjectGitPath, ProjectGitStatusEntry,
    ProjectGitStatusKind, ProjectRegistered, ProjectSearchMatch, ProjectSearchMode,
    ProjectSearchOptions, ProjectSearchResult, ProjectSearchSummary, ProjectTextFile,
    ProjectWorktree,
};

#[derive(Debug, Default)]
pub(crate) struct ProjectManager {
    projects: RwLock<HashMap<ProjectId, Arc<ProjectServices>>>,
    registrations: Mutex<()>,
    active_searches: Mutex<HashMap<RequestId, CancellationFlag>>,
}

#[derive(Debug)]
struct ProjectServices {
    roots: Arc<WorkspaceRoots>,
    directories: DirectoryService,
    files: FileService,
    search: RepositorySearch,
    git: GitService,
}

impl ProjectServices {
    fn new(roots: WorkspaceRoots) -> Self {
        let roots = Arc::new(roots);
        Self {
            directories: DirectoryService::new(Arc::clone(&roots)),
            files: FileService::new(Arc::clone(&roots)),
            search: RepositorySearch::new(Arc::clone(&roots)),
            git: GitService::new(Arc::clone(&roots)),
            roots,
        }
    }
}

impl ProjectManager {
    pub(crate) fn register_with_persistence<F>(
        &self,
        project_id: ProjectId,
        display_name: String,
        roots: &[String],
        persist: F,
    ) -> Result<ProjectRegistered, MaestroError>
    where
        F: FnOnce(&ProjectRegistered) -> Result<ProjectId, MaestroError>,
    {
        if display_name.trim().is_empty() || display_name.len() > 512 {
            return Err(invalid_request("The project display name is invalid."));
        }
        let roots = WorkspaceRoots::new(roots.iter().map(PathBuf::from))
            .map_err(|error| project_error(&error))?;
        let services = Arc::new(ProjectServices::new(roots));
        let canonical_roots = services
            .roots
            .canonical_roots()
            .iter()
            .map(|path| path_string(path))
            .collect();
        let mut registered = ProjectRegistered {
            project_id,
            display_name,
            canonical_roots,
        };
        let _registration = self.registrations.lock().map_err(|_| internal_error())?;
        registered.project_id = persist(&registered)?;
        self.projects
            .write()
            .map_err(|_| internal_error())?
            .insert(registered.project_id, services);
        Ok(registered)
    }

    pub(crate) fn list_directory(
        &self,
        project_id: ProjectId,
        directory: &str,
        cursor: u64,
        maximum_entries: usize,
        include_hidden: bool,
    ) -> Result<ProjectDirectoryPage, MaestroError> {
        let project = self.project(project_id)?;
        let page = project
            .directories
            .list(
                Path::new(directory),
                DirectoryCursor(cursor),
                ListingOptions {
                    maximum_entries,
                    include_hidden,
                },
            )
            .map_err(|error| project_error(&error))?;
        Ok(ProjectDirectoryPage {
            directory: path_string(&page.directory),
            entries: page
                .entries
                .into_iter()
                .map(|entry| ProjectDirectoryEntry {
                    path: path_string(&entry.path),
                    display_name: entry.display_name,
                    kind: match entry.kind {
                        DirectoryEntryKind::Directory => ProjectDirectoryEntryKind::Directory,
                        DirectoryEntryKind::File => ProjectDirectoryEntryKind::File,
                        DirectoryEntryKind::Symlink => ProjectDirectoryEntryKind::Symlink,
                        DirectoryEntryKind::Other => ProjectDirectoryEntryKind::Other,
                    },
                    bytes: entry.bytes,
                })
                .collect(),
            next_cursor: page.next_cursor.map(|next| next.0),
        })
    }

    pub(crate) fn read_file(
        &self,
        project_id: ProjectId,
        path: &str,
    ) -> Result<ProjectTextFile, MaestroError> {
        let file = self
            .project(project_id)?
            .files
            .read_text(Path::new(path))
            .map_err(|error| project_error(&error))?;
        Ok(ProjectTextFile {
            path: path_string(&file.path),
            text: file.text,
            fingerprint: file.fingerprint.as_bytes().to_vec(),
            bytes: file.bytes,
        })
    }

    pub(crate) fn save_file(
        &self,
        project_id: ProjectId,
        path: &str,
        text: &str,
        expected_fingerprint: &[u8],
    ) -> Result<ProjectFileSaved, MaestroError> {
        let expected = <[u8; 32]>::try_from(expected_fingerprint)
            .map(FileFingerprint::from_bytes)
            .map_err(|_| invalid_request("The file fingerprint is invalid."))?;
        let saved = self
            .project(project_id)?
            .files
            .save_text(Path::new(path), text, expected)
            .map_err(|error| project_error(&error))?;
        Ok(ProjectFileSaved {
            fingerprint: saved.fingerprint.as_bytes().to_vec(),
            bytes: saved.bytes,
        })
    }

    pub(crate) fn search(
        &self,
        project_id: ProjectId,
        search_id: RequestId,
        options: &ProjectSearchOptions,
    ) -> Result<ProjectSearchResult, MaestroError> {
        let project = self.project(project_id)?;
        let cancellation = CancellationFlag::default();
        self.active_searches
            .lock()
            .map_err(|_| internal_error())?
            .insert(search_id, cancellation.clone());
        let options = SearchOptions {
            pattern: options.pattern.clone(),
            mode: match options.mode {
                ProjectSearchMode::Literal => SearchMode::Literal,
                ProjectSearchMode::Regex => SearchMode::Regex,
            },
            case_sensitive: options.case_sensitive,
            include_hidden: options.include_hidden,
            maximum_results: options.maximum_results,
            maximum_file_bytes: options.maximum_file_bytes,
        };
        let mut matches = Vec::with_capacity(options.maximum_results.min(1_000));
        let result = project.search.run(&options, &cancellation, |found| {
            matches.push(ProjectSearchMatch {
                path: path_string(&found.path),
                line: found.line,
                byte_column: found.byte_column,
                byte_length: found.byte_length,
                excerpt: found.excerpt.clone(),
            });
            true
        });
        self.active_searches
            .lock()
            .map_err(|_| internal_error())?
            .remove(&search_id);
        let summary = result.map_err(|error| project_error(&error))?;
        Ok(ProjectSearchResult {
            matches,
            summary: ProjectSearchSummary {
                scanned_files: summary.scanned_files,
                skipped_files: summary.skipped_files,
                matches: summary.matches,
                limit_reached: summary.limit_reached,
                cancelled: summary.cancelled,
                consumer_stopped: summary.consumer_stopped,
            },
        })
    }

    pub(crate) fn cancel_search(&self, search_id: RequestId) -> Result<(), MaestroError> {
        if let Some(cancellation) = self
            .active_searches
            .lock()
            .map_err(|_| internal_error())?
            .get(&search_id)
        {
            cancellation.cancel();
        }
        Ok(())
    }

    pub(crate) fn git_status(
        &self,
        project_id: ProjectId,
        repository: &str,
    ) -> Result<Vec<ProjectGitStatusEntry>, MaestroError> {
        self.project(project_id)?
            .git
            .status(Path::new(repository))
            .map(|status| {
                status
                    .entries
                    .into_iter()
                    .map(|entry| ProjectGitStatusEntry {
                        path: git_path(entry.path),
                        original_path: entry.original_path.map(git_path),
                        index_status: entry.index_status,
                        worktree_status: entry.worktree_status,
                        kind: match entry.kind {
                            GitStatusKind::Ordinary => ProjectGitStatusKind::Ordinary,
                            GitStatusKind::RenamedOrCopied => ProjectGitStatusKind::RenamedOrCopied,
                            GitStatusKind::Unmerged => ProjectGitStatusKind::Unmerged,
                            GitStatusKind::Untracked => ProjectGitStatusKind::Untracked,
                            GitStatusKind::Ignored => ProjectGitStatusKind::Ignored,
                        },
                    })
                    .collect()
            })
            .map_err(|error| project_error(&error))
    }

    pub(crate) fn git_branch(
        &self,
        project_id: ProjectId,
        repository: &str,
    ) -> Result<ProjectBranchState, MaestroError> {
        self.project(project_id)?
            .git
            .current_branch(Path::new(repository))
            .map(|branch| match branch {
                BranchState::Branch(name) => ProjectBranchState::Branch(name),
                BranchState::Unborn(name) => ProjectBranchState::Unborn(name),
                BranchState::Detached { commit } => ProjectBranchState::Detached { commit },
            })
            .map_err(|error| project_error(&error))
    }

    pub(crate) fn git_diff(
        &self,
        project_id: ProjectId,
        repository: &str,
        scope: ProjectDiffScope,
        maximum_bytes: usize,
    ) -> Result<ProjectGitDiff, MaestroError> {
        self.project(project_id)?
            .git
            .diff(
                Path::new(repository),
                match scope {
                    ProjectDiffScope::WorkingTree => DiffScope::WorkingTree,
                    ProjectDiffScope::Staged => DiffScope::Staged,
                },
                maximum_bytes,
            )
            .map(|diff| ProjectGitDiff {
                text: diff.text,
                truncated: diff.truncated,
                contains_binary_changes: diff.contains_binary_changes,
            })
            .map_err(|error| project_error(&error))
    }

    pub(crate) fn git_worktrees(
        &self,
        project_id: ProjectId,
        repository: &str,
    ) -> Result<Vec<ProjectWorktree>, MaestroError> {
        self.project(project_id)?
            .git
            .worktrees(Path::new(repository))
            .map(|worktrees| {
                worktrees
                    .into_iter()
                    .map(|worktree| ProjectWorktree {
                        path: path_string(&worktree.path),
                        head: worktree.head,
                        branch: worktree.branch,
                        detached: worktree.detached,
                        bare: worktree.bare,
                        locked_reason: worktree.locked_reason,
                        prunable_reason: worktree.prunable_reason,
                    })
                    .collect()
            })
            .map_err(|error| project_error(&error))
    }

    fn project(&self, project_id: ProjectId) -> Result<Arc<ProjectServices>, MaestroError> {
        self.projects
            .read()
            .map_err(|_| internal_error())?
            .get(&project_id)
            .cloned()
            .ok_or_else(|| {
                MaestroError::new(
                    ErrorCode::PermissionDenied,
                    "The project capability is not registered with the daemon.",
                )
            })
    }

    pub(crate) fn primary_root(&self, project_id: ProjectId) -> Result<PathBuf, MaestroError> {
        self.project(project_id)?
            .roots
            .canonical_roots()
            .first()
            .cloned()
            .ok_or_else(|| invalid_request("The project has no workspace roots."))
    }

    pub(crate) fn terminal_cwd(
        &self,
        project_id: ProjectId,
        requested: &str,
    ) -> Result<PathBuf, MaestroError> {
        if !Path::new(requested).is_absolute() {
            return Err(MaestroError::new(
                ErrorCode::InvalidPath,
                "terminal working directory must be an existing absolute directory",
            ));
        }
        self.project(project_id)?
            .roots
            .canonical_directory(Path::new(requested))
            .map_err(|error| project_error(&error))
    }
}

fn git_path(path: GitPath) -> ProjectGitPath {
    ProjectGitPath {
        bytes: path.bytes,
        display: path.display,
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn invalid_request(message: &str) -> MaestroError {
    MaestroError::new(ErrorCode::InvalidRequest, message)
}

fn internal_error() -> MaestroError {
    MaestroError::new(
        ErrorCode::Internal,
        "The daemon project service is temporarily unavailable.",
    )
}

fn project_error(error: &ProjectError) -> MaestroError {
    let code = match error {
        ProjectError::UnauthorizedPath | ProjectError::UnsafePath => ErrorCode::PermissionDenied,
        ProjectError::GitUnavailable => ErrorCode::CliNotInstalled,
        ProjectError::InvalidLimit | ProjectError::InvalidRegex => ErrorCode::InvalidRequest,
        ProjectError::RootNotAbsolute { .. }
        | ProjectError::RootUnavailable { .. }
        | ProjectError::RootNotDirectory { .. }
        | ProjectError::RootUnreadable { .. }
        | ProjectError::DuplicateRoot { .. }
        | ProjectError::NestedRoots { .. }
        | ProjectError::EmptyRoots
        | ProjectError::NotDirectory
        | ProjectError::NotRegularFile
        | ProjectError::BinaryFile
        | ProjectError::InvalidUtf8
        | ProjectError::FileTooLarge { .. }
        | ProjectError::NotGitRepository => ErrorCode::InvalidPath,
        ProjectError::SaveConflict
        | ProjectError::FileChangedDuringRead
        | ProjectError::GitFailed
        | ProjectError::GitOutputTooLarge
        | ProjectError::MalformedGitOutput
        | ProjectError::Io { .. } => ErrorCode::Internal,
    };
    let mut result = MaestroError::new(code, error.to_string());
    result.retryable = matches!(
        error,
        ProjectError::SaveConflict | ProjectError::FileChangedDuringRead | ProjectError::Io { .. }
    );
    if matches!(error, ProjectError::SaveConflict) {
        result.user_action =
            Some("Reload the file, review the external changes, and try again.".to_owned());
    }
    result
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        sync::{Arc, Mutex, mpsc},
    };

    use maestro_domain::{ErrorCode, MaestroError, ProjectId, RequestId};
    use maestro_protocol::{ProjectSearchMode, ProjectSearchOptions};

    use super::ProjectManager;

    #[test]
    fn registered_capability_drives_file_listing_read_save_and_search() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("project");
        fs::create_dir(&root).expect("root creates");
        let file = root.join("hello.txt");
        fs::write(&file, "hello maestro\n").expect("fixture writes");
        let manager = ProjectManager::default();
        let project_id = ProjectId::new();
        let registered = manager
            .register_with_persistence(
                project_id,
                "Fixture".to_owned(),
                &[root.to_string_lossy().into_owned()],
                |registered| Ok(registered.project_id),
            )
            .expect("project registers");

        let page = manager
            .list_directory(project_id, &registered.canonical_roots[0], 0, 20, false)
            .expect("directory lists");
        assert_eq!(page.entries.len(), 1);
        let opened = manager
            .read_file(project_id, &file.to_string_lossy())
            .expect("file reads");
        manager
            .save_file(
                project_id,
                &file.to_string_lossy(),
                "updated maestro\n",
                &opened.fingerprint,
            )
            .expect("file saves");
        let result = manager
            .search(
                project_id,
                RequestId::new(),
                &ProjectSearchOptions {
                    pattern: "updated".to_owned(),
                    mode: ProjectSearchMode::Literal,
                    case_sensitive: true,
                    include_hidden: false,
                    maximum_results: 20,
                    maximum_file_bytes: 1024,
                },
            )
            .expect("search succeeds");
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn unregistered_project_ids_fail_closed() {
        let manager = ProjectManager::default();
        assert!(manager.read_file(ProjectId::new(), "/tmp/forged").is_err());
    }

    #[test]
    fn failed_persistence_does_not_publish_a_project_capability() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("project");
        fs::create_dir(&root).expect("root creates");
        let manager = ProjectManager::default();
        let project_id = ProjectId::new();

        let error = manager
            .register_with_persistence(
                project_id,
                "Fixture".to_owned(),
                &[root.to_string_lossy().into_owned()],
                |_| {
                    Err(MaestroError::new(
                        ErrorCode::DatabaseUnavailable,
                        "storage unavailable",
                    ))
                },
            )
            .expect_err("failed persistence rejects registration");

        assert_eq!(error.code, ErrorCode::DatabaseUnavailable);
        assert_eq!(
            manager
                .read_file(project_id, &root.join("missing").to_string_lossy())
                .expect_err("capability is not published")
                .code,
            ErrorCode::PermissionDenied
        );
    }

    #[test]
    fn delayed_completion_and_retry_converge_on_one_registered_capability() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("project");
        fs::create_dir(&root).expect("root creates");
        let root = root.to_string_lossy().into_owned();
        let manager = Arc::new(ProjectManager::default());
        let persisted = Arc::new(Mutex::new(HashMap::<Vec<String>, ProjectId>::new()));
        let (first_started, first_started_rx) = mpsc::sync_channel(0);
        let (complete_first, complete_first_rx) = mpsc::sync_channel(0);
        let first_id = ProjectId::new();
        let first_manager = Arc::clone(&manager);
        let first_persisted = Arc::clone(&persisted);
        let first_root = root.clone();
        let first = std::thread::spawn(move || {
            first_manager.register_with_persistence(
                first_id,
                "First".to_owned(),
                &[first_root],
                |registered| {
                    first_started.send(()).expect("start is observed");
                    complete_first_rx.recv().expect("completion is released");
                    let key = registered.canonical_roots.clone();
                    Ok(*first_persisted
                        .lock()
                        .expect("persistence locks")
                        .entry(key)
                        .or_insert(registered.project_id))
                },
            )
        });
        first_started_rx.recv().expect("first registration starts");

        let retry_id = ProjectId::new();
        let retry_manager = Arc::clone(&manager);
        let retry_persisted = Arc::clone(&persisted);
        let (retry_started, retry_started_rx) = mpsc::sync_channel(0);
        let retry = std::thread::spawn(move || {
            retry_started.send(()).expect("retry is observed");
            retry_manager.register_with_persistence(
                retry_id,
                "Retry".to_owned(),
                &[root],
                |registered| {
                    let key = registered.canonical_roots.clone();
                    Ok(*retry_persisted
                        .lock()
                        .expect("persistence locks")
                        .entry(key)
                        .or_insert(registered.project_id))
                },
            )
        });
        retry_started_rx
            .recv()
            .expect("retry starts before completion");
        complete_first.send(()).expect("first completion releases");

        let first = first
            .join()
            .expect("first thread joins")
            .expect("first registers");
        let retry = retry
            .join()
            .expect("retry thread joins")
            .expect("retry registers");
        assert_ne!(first_id, retry_id);
        assert_eq!(first.project_id, first_id);
        assert_eq!(retry.project_id, first_id);
        assert_eq!(persisted.lock().expect("persistence locks").len(), 1);
        assert!(manager.project(first_id).is_ok());
        assert!(manager.project(retry_id).is_err());
    }
}
