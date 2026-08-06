use std::io;

use thiserror::Error;

/// Stable, frontend-safe failures from project-scoped local operations.
#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("at least one workspace root is required")]
    EmptyRoots,
    #[error("workspace root {index} must be absolute")]
    RootNotAbsolute { index: usize },
    #[error("workspace root {index} is unavailable")]
    RootUnavailable { index: usize },
    #[error("workspace root {index} is not a directory")]
    RootNotDirectory { index: usize },
    #[error("workspace root {index} is not readable and searchable")]
    RootUnreadable { index: usize },
    #[error("workspace roots {first} and {second} resolve to the same directory")]
    DuplicateRoot { first: usize, second: usize },
    #[error("workspace roots {outer} and {inner} are nested")]
    NestedRoots { outer: usize, inner: usize },
    #[error("the requested path is outside the authorized workspace roots")]
    UnauthorizedPath,
    #[error("the requested path traverses a symlink or unsafe component")]
    UnsafePath,
    #[error("the requested path is not a directory")]
    NotDirectory,
    #[error("the requested path is not a regular file")]
    NotRegularFile,
    #[error("the file is binary and cannot be opened as text")]
    BinaryFile,
    #[error("the file is not valid UTF-8 text")]
    InvalidUtf8,
    #[error("the file has {actual} bytes, exceeding the {maximum}-byte limit")]
    FileTooLarge { actual: u64, maximum: usize },
    #[error("the file changed or was removed while it was being read")]
    FileChangedDuringRead,
    #[error("the file changed after it was opened; save was not performed")]
    SaveConflict,
    #[error("the requested operation limit is outside the supported range")]
    InvalidLimit,
    #[error("the regular expression is invalid")]
    InvalidRegex,
    #[error("Git is not installed or cannot be launched")]
    GitUnavailable,
    #[error("the selected directory is not a Git repository")]
    NotGitRepository,
    #[error("the installed Git command failed")]
    GitFailed,
    #[error("Git output exceeded the bounded parser limit")]
    GitOutputTooLarge,
    #[error("Git returned malformed machine-readable output")]
    MalformedGitOutput,
    #[error("local {operation} failed with {kind:?}")]
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
}

impl ProjectError {
    pub(crate) fn io(operation: &'static str, error: impl Into<io::Error>) -> Self {
        Self::Io {
            operation,
            kind: error.into().kind(),
        }
    }
}
