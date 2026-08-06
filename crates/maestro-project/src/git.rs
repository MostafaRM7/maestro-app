use std::{
    ffi::{OsStr, OsString},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use crate::{ProjectError, WorkspaceRoots};

const MAXIMUM_GIT_METADATA_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_GIT_STDERR_BYTES: usize = 64 * 1024;
const MAXIMUM_DIFF_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GitPath {
    pub bytes: Vec<u8>,
    pub display: String,
}

impl GitPath {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            display: String::from_utf8_lossy(bytes).into_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GitStatusKind {
    Ordinary,
    RenamedOrCopied,
    Unmerged,
    Untracked,
    Ignored,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GitStatusEntry {
    pub path: GitPath,
    pub original_path: Option<GitPath>,
    pub index_status: char,
    pub worktree_status: char,
    pub kind: GitStatusKind,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GitStatus {
    pub entries: Vec<GitStatusEntry>,
}

impl GitStatus {
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum BranchState {
    Branch(String),
    Unborn(String),
    Detached { commit: String },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DiffScope {
    WorkingTree,
    Staged,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GitDiff {
    /// Untrusted plain text. Consumers must render this as text, never HTML.
    pub text: String,
    pub truncated: bool,
    pub contains_binary_changes: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked_reason: Option<String>,
    pub prunable_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GitService {
    roots: Arc<WorkspaceRoots>,
    executable: PathBuf,
}

impl GitService {
    pub fn new(roots: Arc<WorkspaceRoots>) -> Self {
        Self {
            roots,
            executable: PathBuf::from("git"),
        }
    }

    pub fn with_executable(roots: Arc<WorkspaceRoots>, executable: PathBuf) -> Self {
        Self { roots, executable }
    }

    /// Returns the machine-readable status of an authorized repository.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the repository is unauthorized, Git is
    /// unavailable or fails, or its bounded status output cannot be parsed.
    pub fn status(&self, repository: &Path) -> Result<GitStatus, ProjectError> {
        let repository = self.authorized_repository(repository)?;
        let arguments = [
            OsStr::new("-c"),
            OsStr::new("core.quotepath=false"),
            OsStr::new("status"),
            OsStr::new("--porcelain=v2"),
            OsStr::new("-z"),
            OsStr::new("--ignored=matching"),
            OsStr::new("--untracked-files=all"),
            OsStr::new("--"),
        ];
        let output = self.run(&repository, &arguments, MAXIMUM_GIT_METADATA_BYTES)?;
        require_success(&output)?;
        if output.truncated {
            return Err(ProjectError::GitOutputTooLarge);
        }
        parse_status(&output.stdout)
    }

    /// Reports the current attached, unborn, or detached branch state.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the repository is unauthorized, is not a
    /// Git repository, or the required Git commands cannot be completed.
    pub fn current_branch(&self, repository: &Path) -> Result<BranchState, ProjectError> {
        let repository = self.authorized_repository(repository)?;
        let symbolic = self.run(
            &repository,
            &[
                OsStr::new("symbolic-ref"),
                OsStr::new("--quiet"),
                OsStr::new("--short"),
                OsStr::new("HEAD"),
            ],
            MAXIMUM_GIT_STDERR_BYTES,
        )?;
        if symbolic.success {
            let branch = trimmed_lossy(&symbolic.stdout);
            let verified = self.run(
                &repository,
                &[
                    OsStr::new("rev-parse"),
                    OsStr::new("--verify"),
                    OsStr::new("HEAD"),
                ],
                MAXIMUM_GIT_STDERR_BYTES,
            )?;
            return if verified.success {
                Ok(BranchState::Branch(branch))
            } else {
                Ok(BranchState::Unborn(branch))
            };
        }
        if is_not_repository(&symbolic.stderr) {
            return Err(ProjectError::NotGitRepository);
        }
        let detached = self.run(
            &repository,
            &[
                OsStr::new("rev-parse"),
                OsStr::new("--short=12"),
                OsStr::new("HEAD"),
            ],
            MAXIMUM_GIT_STDERR_BYTES,
        )?;
        require_success(&detached)?;
        Ok(BranchState::Detached {
            commit: trimmed_lossy(&detached.stdout),
        })
    }

    /// Reads a bounded working-tree or staged diff as untrusted plain text.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the byte limit is invalid, the repository
    /// is unauthorized, or Git is unavailable or fails before producing a
    /// bounded result.
    pub fn diff(
        &self,
        repository: &Path,
        scope: DiffScope,
        maximum_bytes: usize,
    ) -> Result<GitDiff, ProjectError> {
        if maximum_bytes == 0 || maximum_bytes > MAXIMUM_DIFF_BYTES {
            return Err(ProjectError::InvalidLimit);
        }
        let repository = self.authorized_repository(repository)?;
        let mut arguments = vec![
            OsString::from("-c"),
            OsString::from("core.quotepath=false"),
            OsString::from("diff"),
            OsString::from("--no-ext-diff"),
            OsString::from("--no-color"),
            OsString::from("--find-renames"),
        ];
        if scope == DiffScope::Staged {
            arguments.push(OsString::from("--cached"));
        }
        arguments.push(OsString::from("--"));
        let borrowed = arguments
            .iter()
            .map(OsString::as_os_str)
            .collect::<Vec<_>>();
        let output = self.run(&repository, &borrowed, maximum_bytes)?;
        if !output.success && !output.truncated {
            require_success(&output)?;
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(GitDiff {
            contains_binary_changes: text.contains("Binary files ")
                || text.contains("GIT binary patch"),
            text,
            truncated: output.truncated,
        })
    }

    /// Discovers worktrees already registered with an authorized repository.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the repository is unauthorized, Git is
    /// unavailable or fails, or its bounded output cannot be parsed.
    pub fn worktrees(&self, repository: &Path) -> Result<Vec<WorktreeInfo>, ProjectError> {
        let repository = self.authorized_repository(repository)?;
        let output = self.run(
            &repository,
            &[
                OsStr::new("worktree"),
                OsStr::new("list"),
                OsStr::new("--porcelain"),
                OsStr::new("-z"),
            ],
            MAXIMUM_GIT_METADATA_BYTES,
        )?;
        require_success(&output)?;
        if output.truncated {
            return Err(ProjectError::GitOutputTooLarge);
        }
        parse_worktrees(&output.stdout)
    }

    fn authorized_repository(&self, repository: &Path) -> Result<PathBuf, ProjectError> {
        self.roots
            .open_directory(repository)
            .map(|directory| directory.canonical)
    }

    fn run(
        &self,
        repository: &Path,
        arguments: &[&OsStr],
        maximum_stdout: usize,
    ) -> Result<GitCommandOutput, ProjectError> {
        let mut child = Command::new(&self.executable)
            .args(arguments)
            .current_dir(repository)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| ProjectError::GitUnavailable)?;
        let stdout = child.stdout.take().ok_or(ProjectError::GitFailed)?;
        let stderr = child.stderr.take().ok_or(ProjectError::GitFailed)?;
        let stderr_task =
            std::thread::spawn(move || read_bounded(stderr, MAXIMUM_GIT_STDERR_BYTES));
        let (stdout, truncated) = read_bounded(stdout, maximum_stdout)?;
        if truncated {
            let _ = child.kill();
        }
        let status = child
            .wait()
            .map_err(|error| ProjectError::io("waiting for Git", error))?;
        let (stderr, _) = stderr_task.join().map_err(|_| ProjectError::GitFailed)??;
        Ok(GitCommandOutput {
            success: status.success(),
            stdout,
            stderr,
            truncated,
        })
    }
}

#[derive(Debug)]
struct GitCommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

fn read_bounded<R: Read>(mut reader: R, maximum: usize) -> Result<(Vec<u8>, bool), ProjectError> {
    let mut bytes = Vec::with_capacity(maximum.min(64 * 1024));
    reader
        .by_ref()
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| ProjectError::io("reading Git output", error))?;
    let truncated = bytes.len() > maximum;
    if truncated {
        bytes.truncate(maximum);
    }
    Ok((bytes, truncated))
}

fn require_success(output: &GitCommandOutput) -> Result<(), ProjectError> {
    if output.success {
        Ok(())
    } else if is_not_repository(&output.stderr) {
        Err(ProjectError::NotGitRepository)
    } else {
        Err(ProjectError::GitFailed)
    }
}

fn is_not_repository(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr)
        .to_ascii_lowercase()
        .contains("not a git repository")
}

fn parse_status(bytes: &[u8]) -> Result<GitStatus, ProjectError> {
    let records = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut index = 0_usize;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.is_empty() || record.starts_with(b"# ") {
            continue;
        }
        match record[0] {
            b'1' => entries.push(parse_ordinary(record)?),
            b'2' => {
                let original = records.get(index).ok_or(ProjectError::MalformedGitOutput)?;
                index += 1;
                entries.push(parse_rename(record, original)?);
            }
            b'u' => entries.push(parse_unmerged(record)?),
            b'?' => entries.push(parse_simple(record, GitStatusKind::Untracked, '?')?),
            b'!' => entries.push(parse_simple(record, GitStatusKind::Ignored, '!')?),
            _ => return Err(ProjectError::MalformedGitOutput),
        }
    }
    Ok(GitStatus { entries })
}

fn parse_ordinary(record: &[u8]) -> Result<GitStatusEntry, ProjectError> {
    let fields = record.splitn(9, |byte| *byte == b' ').collect::<Vec<_>>();
    if fields.len() != 9 {
        return Err(ProjectError::MalformedGitOutput);
    }
    status_entry(fields[1], fields[8], None, GitStatusKind::Ordinary)
}

fn parse_rename(record: &[u8], original: &[u8]) -> Result<GitStatusEntry, ProjectError> {
    let fields = record.splitn(10, |byte| *byte == b' ').collect::<Vec<_>>();
    if fields.len() != 10 || original.is_empty() {
        return Err(ProjectError::MalformedGitOutput);
    }
    status_entry(
        fields[1],
        fields[9],
        Some(GitPath::from_bytes(original)),
        GitStatusKind::RenamedOrCopied,
    )
}

fn parse_unmerged(record: &[u8]) -> Result<GitStatusEntry, ProjectError> {
    let fields = record.splitn(11, |byte| *byte == b' ').collect::<Vec<_>>();
    if fields.len() != 11 {
        return Err(ProjectError::MalformedGitOutput);
    }
    status_entry(fields[1], fields[10], None, GitStatusKind::Unmerged)
}

fn parse_simple(
    record: &[u8],
    kind: GitStatusKind,
    status: char,
) -> Result<GitStatusEntry, ProjectError> {
    let path = record
        .get(2..)
        .filter(|path| !path.is_empty())
        .ok_or(ProjectError::MalformedGitOutput)?;
    Ok(GitStatusEntry {
        path: GitPath::from_bytes(path),
        original_path: None,
        index_status: status,
        worktree_status: status,
        kind,
    })
}

fn status_entry(
    status: &[u8],
    path: &[u8],
    original_path: Option<GitPath>,
    kind: GitStatusKind,
) -> Result<GitStatusEntry, ProjectError> {
    if status.len() != 2 || path.is_empty() {
        return Err(ProjectError::MalformedGitOutput);
    }
    Ok(GitStatusEntry {
        path: GitPath::from_bytes(path),
        original_path,
        index_status: char::from(status[0]),
        worktree_status: char::from(status[1]),
        kind,
    })
}

fn parse_worktrees(bytes: &[u8]) -> Result<Vec<WorktreeInfo>, ProjectError> {
    let mut worktrees = Vec::new();
    let mut current: Option<WorktreeInfo> = None;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            continue;
        }
        if let Some(path) = field.strip_prefix(b"worktree ") {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            current = Some(WorktreeInfo {
                path: path_from_bytes(path),
                head: None,
                branch: None,
                detached: false,
                bare: false,
                locked_reason: None,
                prunable_reason: None,
            });
            continue;
        }
        let worktree = current.as_mut().ok_or(ProjectError::MalformedGitOutput)?;
        if let Some(head) = field.strip_prefix(b"HEAD ") {
            worktree.head = Some(String::from_utf8_lossy(head).into_owned());
        } else if let Some(branch) = field.strip_prefix(b"branch ") {
            worktree.branch = Some(
                String::from_utf8_lossy(branch.strip_prefix(b"refs/heads/").unwrap_or(branch))
                    .into_owned(),
            );
        } else if field == b"detached" {
            worktree.detached = true;
        } else if field == b"bare" {
            worktree.bare = true;
        } else if let Some(reason) = field.strip_prefix(b"locked") {
            worktree.locked_reason = nonempty_reason(reason);
        } else if let Some(reason) = field.strip_prefix(b"prunable") {
            worktree.prunable_reason = nonempty_reason(reason);
        } else {
            return Err(ProjectError::MalformedGitOutput);
        }
    }
    if let Some(worktree) = current {
        worktrees.push(worktree);
    }
    if worktrees.is_empty() {
        return Err(ProjectError::MalformedGitOutput);
    }
    Ok(worktrees)
}

fn nonempty_reason(reason: &[u8]) -> Option<String> {
    let reason = reason.strip_prefix(b" ").unwrap_or(reason);
    (!reason.is_empty()).then(|| String::from_utf8_lossy(reason).into_owned())
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

fn trimmed_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        fs,
        path::{Path, PathBuf},
        process::Command,
        sync::Arc,
    };

    use tempfile::tempdir;

    use super::{BranchState, DiffScope, GitService, GitStatusKind};
    use crate::{ProjectError, WorkspaceRoots};

    fn git(repository: &Path, arguments: &[&OsStr]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(repository)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("Git starts");
        assert!(status.success(), "Git fixture command failed");
    }

    fn initialize(repository: &Path) {
        fs::create_dir_all(repository).unwrap();
        git(
            repository,
            &[
                OsStr::new("init"),
                OsStr::new("-q"),
                OsStr::new("-b"),
                OsStr::new("main"),
            ],
        );
        git(
            repository,
            &[
                OsStr::new("config"),
                OsStr::new("user.name"),
                OsStr::new("Maestro Test"),
            ],
        );
        git(
            repository,
            &[
                OsStr::new("config"),
                OsStr::new("user.email"),
                OsStr::new("maestro@example.invalid"),
            ],
        );
    }

    fn commit_all(repository: &Path, message: &str) {
        git(
            repository,
            &[OsStr::new("add"), OsStr::new("--"), OsStr::new(".")],
        );
        git(
            repository,
            &[
                OsStr::new("commit"),
                OsStr::new("-q"),
                OsStr::new("-m"),
                OsStr::new(message),
            ],
        );
    }

    #[test]
    fn status_parses_staged_modified_untracked_ignored_rename_and_hostile_names() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        initialize(&repository);
        fs::write(repository.join("tracked.txt"), "base\n").unwrap();
        fs::write(repository.join("rename-me.txt"), "rename\n").unwrap();
        fs::write(repository.join(".gitignore"), "ignored.txt\n").unwrap();
        commit_all(&repository, "base");

        fs::write(repository.join("tracked.txt"), "changed\n").unwrap();
        fs::write(repository.join("staged.txt"), "staged\n").unwrap();
        git(
            &repository,
            &[
                OsStr::new("add"),
                OsStr::new("--"),
                OsStr::new("staged.txt"),
            ],
        );
        git(
            &repository,
            &[
                OsStr::new("mv"),
                OsStr::new("--"),
                OsStr::new("rename-me.txt"),
                OsStr::new("renamed\n<script>.txt"),
            ],
        );
        fs::write(repository.join("ignored.txt"), "ignored\n").unwrap();
        let hostile = "--upload-pack=$(touch injected)\n<script>.txt";
        fs::write(repository.join(hostile), "hostile\n").unwrap();

        let service = GitService::new(Arc::new(WorkspaceRoots::new([&repository]).unwrap()));
        let status = service.status(&repository).unwrap();
        assert!(!status.is_clean());
        assert!(
            status
                .entries
                .iter()
                .any(|entry| entry.path.display == "tracked.txt" && entry.worktree_status == 'M')
        );
        assert!(
            status
                .entries
                .iter()
                .any(|entry| entry.path.display == "staged.txt" && entry.index_status == 'A')
        );
        assert!(
            status
                .entries
                .iter()
                .any(|entry| entry.kind == GitStatusKind::RenamedOrCopied)
        );
        assert!(status.entries.iter().any(
            |entry| entry.kind == GitStatusKind::Ignored && entry.path.display == "ignored.txt"
        ));
        assert!(
            status
                .entries
                .iter()
                .any(|entry| entry.path.display == hostile)
        );
        assert!(!repository.join("injected").exists());
    }

    #[test]
    fn branch_reports_unborn_attached_and_detached_states() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        initialize(&repository);
        let service = GitService::new(Arc::new(WorkspaceRoots::new([&repository]).unwrap()));
        assert_eq!(
            service.current_branch(&repository).unwrap(),
            BranchState::Unborn("main".to_owned())
        );

        fs::write(repository.join("file"), "base").unwrap();
        commit_all(&repository, "base");
        assert_eq!(
            service.current_branch(&repository).unwrap(),
            BranchState::Branch("main".to_owned())
        );
        git(
            &repository,
            &[
                OsStr::new("checkout"),
                OsStr::new("-q"),
                OsStr::new("--detach"),
            ],
        );
        assert!(matches!(
            service.current_branch(&repository).unwrap(),
            BranchState::Detached { .. }
        ));
    }

    #[test]
    fn diff_is_bounded_and_marks_binary_changes_as_untrusted_text() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        initialize(&repository);
        fs::write(repository.join("file.txt"), "base\n").unwrap();
        fs::write(repository.join("binary.bin"), [0_u8, 1, 2]).unwrap();
        commit_all(&repository, "base");
        fs::write(
            repository.join("file.txt"),
            format!("<script>{}</script>\n", "x".repeat(10_000)),
        )
        .unwrap();
        fs::write(repository.join("binary.bin"), [0_u8, 9, 2]).unwrap();

        let service = GitService::new(Arc::new(WorkspaceRoots::new([&repository]).unwrap()));
        let diff = service
            .diff(&repository, DiffScope::WorkingTree, 512)
            .unwrap();
        assert!(diff.truncated);
        let full = service
            .diff(&repository, DiffScope::WorkingTree, 64 * 1024)
            .unwrap();
        assert!(full.contains_binary_changes);
        assert!(full.text.contains("<script>"));
    }

    #[test]
    fn existing_worktrees_are_discovered_without_mutation_api() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        let linked = temporary.path().join("linked-worktree");
        initialize(&repository);
        fs::write(repository.join("file"), "base").unwrap();
        commit_all(&repository, "base");
        git(
            &repository,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("-q"),
                OsStr::new("-b"),
                OsStr::new("linked"),
                linked.as_os_str(),
            ],
        );

        let service = GitService::new(Arc::new(WorkspaceRoots::new([&repository]).unwrap()));
        let worktrees = service.worktrees(&repository).unwrap();
        let linked = fs::canonicalize(linked).unwrap();
        assert_eq!(worktrees.len(), 2);
        assert!(worktrees.iter().any(
            |worktree| worktree.path == linked && worktree.branch.as_deref() == Some("linked")
        ));
    }

    #[test]
    fn non_repository_and_missing_git_are_stable_errors() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        let roots = Arc::new(WorkspaceRoots::new([&root]).unwrap());
        assert!(matches!(
            GitService::new(Arc::clone(&roots)).status(&root),
            Err(ProjectError::NotGitRepository)
        ));
        assert!(matches!(
            GitService::with_executable(roots, PathBuf::from("definitely-missing-git"))
                .status(&root),
            Err(ProjectError::GitUnavailable)
        ));
    }
}
