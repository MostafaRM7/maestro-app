use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};

use crate::{FileService, ProjectError, WorkspaceRoots, auth::path_name};

pub const DEFAULT_MAXIMUM_SEARCH_RESULTS: usize = 1_000;
pub const MAXIMUM_SEARCH_RESULTS: usize = 10_000;
pub const DEFAULT_MAXIMUM_SEARCH_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAXIMUM_PATTERN_BYTES: usize = 4 * 1024;
pub const MAXIMUM_EXCERPT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SearchMode {
    Literal,
    Regex,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SearchOptions {
    pub pattern: String,
    pub mode: SearchMode,
    pub case_sensitive: bool,
    pub include_hidden: bool,
    pub maximum_results: usize,
    pub maximum_file_bytes: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            mode: SearchMode::Literal,
            case_sensitive: true,
            include_hidden: false,
            maximum_results: DEFAULT_MAXIMUM_SEARCH_RESULTS,
            maximum_file_bytes: DEFAULT_MAXIMUM_SEARCH_FILE_BYTES,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SearchMatch {
    pub path: PathBuf,
    pub line: u64,
    pub byte_column: usize,
    pub byte_length: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct SearchSummary {
    pub scanned_files: u64,
    pub skipped_files: u64,
    pub matches: usize,
    pub limit_reached: bool,
    pub cancelled: bool,
    pub consumer_stopped: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationFlag(Arc<AtomicBool>);

impl CancellationFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
pub struct RepositorySearch {
    roots: Arc<WorkspaceRoots>,
    files: FileService,
}

impl RepositorySearch {
    pub fn new(roots: Arc<WorkspaceRoots>) -> Self {
        Self {
            files: FileService::new(Arc::clone(&roots)),
            roots,
        }
    }

    /// Streams bounded matches to `emit`. Returning `false` from `emit` stops
    /// the search without scheduling further file reads.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when an option is invalid, a regular
    /// expression cannot be compiled, an authorized root becomes unsafe, or a
    /// non-skippable local operation fails.
    pub fn run<F>(
        &self,
        options: &SearchOptions,
        cancellation: &CancellationFlag,
        mut emit: F,
    ) -> Result<SearchSummary, ProjectError>
    where
        F: FnMut(&SearchMatch) -> bool,
    {
        validate_options(options)?;
        let expression = compile_expression(options)?;
        let mut summary = SearchSummary::default();

        'roots: for root in self.roots.canonical_roots() {
            let mut builder = WalkBuilder::new(&root);
            builder
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
            for result in builder.build() {
                if cancellation.is_cancelled() {
                    summary.cancelled = true;
                    break 'roots;
                }
                let Ok(entry) = result else {
                    summary.skipped_files = summary.skipped_files.saturating_add(1);
                    continue;
                };
                if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                    continue;
                }
                summary.scanned_files = summary.scanned_files.saturating_add(1);
                let bytes = match self
                    .files
                    .read_search_bytes(entry.path(), options.maximum_file_bytes)
                {
                    Ok(bytes) => bytes,
                    Err(
                        ProjectError::BinaryFile
                        | ProjectError::InvalidUtf8
                        | ProjectError::FileTooLarge { .. }
                        | ProjectError::NotRegularFile
                        | ProjectError::FileChangedDuringRead
                        | ProjectError::Io { .. },
                    ) => {
                        summary.skipped_files = summary.skipped_files.saturating_add(1);
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if bytes.contains(&0) {
                    summary.skipped_files = summary.skipped_files.saturating_add(1);
                    continue;
                }
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    summary.skipped_files = summary.skipped_files.saturating_add(1);
                    continue;
                };
                if !search_file(
                    entry.path(),
                    text,
                    &expression,
                    options.maximum_results,
                    cancellation,
                    &mut summary,
                    &mut emit,
                ) {
                    break 'roots;
                }
            }
        }
        Ok(summary)
    }
}

fn validate_options(options: &SearchOptions) -> Result<(), ProjectError> {
    if options.pattern.is_empty()
        || options.pattern.len() > MAXIMUM_PATTERN_BYTES
        || options.maximum_results == 0
        || options.maximum_results > MAXIMUM_SEARCH_RESULTS
        || options.maximum_file_bytes == 0
        || options.maximum_file_bytes > crate::file::MAXIMUM_TEXT_BYTES
    {
        return Err(ProjectError::InvalidLimit);
    }
    Ok(())
}

fn compile_expression(options: &SearchOptions) -> Result<Regex, ProjectError> {
    let pattern = match options.mode {
        SearchMode::Literal => regex::escape(&options.pattern),
        SearchMode::Regex => options.pattern.clone(),
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!options.case_sensitive)
        .build()
        .map_err(|_| ProjectError::InvalidRegex)
}

fn search_file<F>(
    path: &Path,
    text: &str,
    expression: &Regex,
    maximum_results: usize,
    cancellation: &CancellationFlag,
    summary: &mut SearchSummary,
    emit: &mut F,
) -> bool
where
    F: FnMut(&SearchMatch) -> bool,
{
    for (line_index, line) in text.split_inclusive('\n').enumerate() {
        if cancellation.is_cancelled() {
            summary.cancelled = true;
            return false;
        }
        for found in expression.find_iter(line) {
            if summary.matches == maximum_results {
                summary.limit_reached = true;
                return false;
            }
            let result = SearchMatch {
                path: path.to_path_buf(),
                line: u64::try_from(line_index + 1).unwrap_or(u64::MAX),
                byte_column: found.start(),
                byte_length: found.len(),
                excerpt: bounded_excerpt(line),
            };
            summary.matches += 1;
            if !emit(&result) {
                summary.consumer_stopped = true;
                return false;
            }
            if cancellation.is_cancelled() {
                summary.cancelled = true;
                return false;
            }
        }
    }
    true
}

fn bounded_excerpt(line: &str) -> String {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.len() <= MAXIMUM_EXCERPT_BYTES {
        return line.to_owned();
    }
    let mut boundary = MAXIMUM_EXCERPT_BYTES;
    while !line.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…", &line[..boundary])
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    use super::{CancellationFlag, RepositorySearch, SearchMode, SearchOptions};
    use crate::WorkspaceRoots;

    #[test]
    fn literal_and_regex_search_are_bounded_and_ignore_aware() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("repo");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.join("one.txt"), "needle one\nNEEDLE two\n").unwrap();
        fs::write(root.join("two.txt"), "needle three\n").unwrap();
        fs::write(root.join("ignored.txt"), "needle ignored\n").unwrap();
        fs::write(root.join(".hidden.txt"), "needle hidden\n").unwrap();
        let search = RepositorySearch::new(Arc::new(WorkspaceRoots::new([&root]).unwrap()));
        let mut matches = Vec::new();
        let summary = search
            .run(
                &SearchOptions {
                    pattern: "needle".to_owned(),
                    case_sensitive: false,
                    maximum_results: 2,
                    ..SearchOptions::default()
                },
                &CancellationFlag::default(),
                |result| {
                    matches.push(result.clone());
                    true
                },
            )
            .unwrap();
        assert_eq!(matches.len(), 2);
        assert!(summary.limit_reached);
        assert!(
            matches
                .iter()
                .all(|item| !item.path.ends_with("ignored.txt"))
        );

        let mut regex_matches = Vec::new();
        search
            .run(
                &SearchOptions {
                    pattern: "n[e]{2}dle\\s+(one|three)".to_owned(),
                    mode: SearchMode::Regex,
                    maximum_results: 10,
                    ..SearchOptions::default()
                },
                &CancellationFlag::default(),
                |result| {
                    regex_matches.push(result.clone());
                    true
                },
            )
            .unwrap();
        assert_eq!(regex_matches.len(), 2);
    }

    #[test]
    fn cancellation_stops_streaming_without_scanning_the_repository() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("repo");
        fs::create_dir(&root).unwrap();
        for index in 0..500 {
            fs::write(root.join(format!("file-{index}.txt")), "needle\n").unwrap();
        }
        let search = RepositorySearch::new(Arc::new(WorkspaceRoots::new([&root]).unwrap()));
        let cancellation = CancellationFlag::default();
        let callback_flag = cancellation.clone();
        let summary = search
            .run(
                &SearchOptions {
                    pattern: "needle".to_owned(),
                    maximum_results: 1_000,
                    ..SearchOptions::default()
                },
                &cancellation,
                |_| {
                    callback_flag.cancel();
                    true
                },
            )
            .unwrap();

        assert!(summary.cancelled);
        assert_eq!(summary.matches, 1);
        assert!(summary.scanned_files < 500);
    }

    #[cfg(unix)]
    #[test]
    fn repository_walk_never_follows_an_outside_symlink() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("repo");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "unique-secret-needle").unwrap();
        symlink(&outside, root.join("outside-link")).unwrap();
        let search = RepositorySearch::new(Arc::new(WorkspaceRoots::new([&root]).unwrap()));
        let summary = search
            .run(
                &SearchOptions {
                    pattern: "unique-secret-needle".to_owned(),
                    maximum_results: 10,
                    ..SearchOptions::default()
                },
                &CancellationFlag::default(),
                |_| true,
            )
            .unwrap();
        assert_eq!(summary.matches, 0);
    }
}
