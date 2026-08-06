//! Capability-scoped local project services for Maestro's lightweight file and
//! Git review experience.
//!
//! This crate is intentionally synchronous. The daemon must execute blocking
//! file walks, file I/O, searches, and Git commands on its blocking pool and
//! must apply its own IPC authorization before constructing these services.

mod auth;
mod error;
mod file;
mod git;
mod listing;
mod search;

pub use auth::WorkspaceRoots;
pub use error::ProjectError;
pub use file::{FileFingerprint, FileService, SaveResult, TextFile};
pub use git::{
    BranchState, DiffScope, GitDiff, GitPath, GitService, GitStatus, GitStatusEntry, GitStatusKind,
    WorktreeInfo,
};
pub use listing::{
    DirectoryCursor, DirectoryEntry, DirectoryEntryKind, DirectoryPage, DirectoryService,
    ListingOptions,
};
pub use search::{
    CancellationFlag, RepositorySearch, SearchMatch, SearchMode, SearchOptions, SearchSummary,
};
