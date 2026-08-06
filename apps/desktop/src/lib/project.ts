import { invoke } from "@tauri-apps/api/core";

export type DirectoryEntryKind = "directory" | "file" | "symlink" | "other";

export interface DirectoryEntry {
  path: string;
  displayName: string;
  kind: DirectoryEntryKind;
  bytes: number | null;
}

export interface DirectoryPage {
  directory: string;
  entries: DirectoryEntry[];
  nextCursor: number | null;
}

export interface TextFile {
  path: string;
  text: string;
  fingerprint: number[];
  bytes: number;
}

export interface FileSaved {
  fingerprint: number[];
  bytes: number;
}

export interface SearchOptions {
  pattern: string;
  mode: "literal" | "regex";
  caseSensitive: boolean;
  includeHidden: boolean;
  maximumResults: number;
  maximumFileBytes: number;
}

export interface SearchMatch {
  path: string;
  line: number;
  byteColumn: number;
  byteLength: number;
  excerpt: string;
}

export interface SearchResult {
  matches: SearchMatch[];
  summary: {
    scannedFiles: number;
    skippedFiles: number;
    matches: number;
    limitReached: boolean;
    cancelled: boolean;
    consumerStopped: boolean;
  };
}

export interface GitStatusEntry {
  path: { bytes: number[]; display: string };
  originalPath: { bytes: number[]; display: string } | null;
  indexStatus: string;
  worktreeStatus: string;
  kind: "ordinary" | "renamed_or_copied" | "unmerged" | "untracked" | "ignored";
}

export type BranchState =
  | { state: "branch"; data: string }
  | { state: "unborn"; data: string }
  | { state: "detached"; data: { commit: string } };

export interface GitDiff {
  text: string;
  truncated: boolean;
  containsBinaryChanges: boolean;
}

export interface Worktree {
  path: string;
  head: string | null;
  branch: string | null;
  detached: boolean;
  bare: boolean;
  lockedReason: string | null;
  prunableReason: string | null;
}

export interface ProjectClient {
  listDirectory(projectGrant: string, directory: string, cursor?: number): Promise<DirectoryPage>;
  readFile(projectGrant: string, path: string): Promise<TextFile>;
  openFileExternal(projectGrant: string, path: string): Promise<void>;
  saveFile(
    projectGrant: string,
    path: string,
    text: string,
    expectedFingerprint: number[],
  ): Promise<FileSaved>;
  search(
    projectGrant: string,
    searchId: string,
    options: SearchOptions,
  ): Promise<SearchResult>;
  cancelSearch(projectGrant: string, searchId: string): Promise<void>;
  gitStatus(projectGrant: string, repository: string): Promise<GitStatusEntry[]>;
  gitBranch(projectGrant: string, repository: string): Promise<BranchState>;
  gitDiff(
    projectGrant: string,
    repository: string,
    scope: "working_tree" | "staged",
  ): Promise<GitDiff>;
  gitWorktrees(projectGrant: string, repository: string): Promise<Worktree[]>;
}

export const tauriProjectClient: ProjectClient = {
  listDirectory(projectGrant, directory, cursor = 0) {
    return invoke<DirectoryPage>("project_directory_list", {
      projectGrant,
      directory,
      cursor,
      maximumEntries: 500,
      includeHidden: false,
    });
  },
  readFile(projectGrant, path) {
    return invoke<TextFile>("project_file_read", { projectGrant, path });
  },
  openFileExternal(projectGrant, path) {
    return invoke<void>("project_file_open_external", { projectGrant, path });
  },
  saveFile(projectGrant, path, text, expectedFingerprint) {
    return invoke<FileSaved>("project_file_save", {
      projectGrant,
      path,
      text,
      expectedFingerprint,
    });
  },
  search(projectGrant, searchId, options) {
    return invoke<SearchResult>("project_search", { projectGrant, searchId, options });
  },
  cancelSearch(projectGrant, searchId) {
    return invoke<void>("project_search_cancel", { projectGrant, searchId });
  },
  gitStatus(projectGrant, repository) {
    return invoke<GitStatusEntry[]>("project_git_status", { projectGrant, repository });
  },
  gitBranch(projectGrant, repository) {
    return invoke<BranchState>("project_git_branch", { projectGrant, repository });
  },
  gitDiff(projectGrant, repository, scope) {
    return invoke<GitDiff>("project_git_diff", {
      projectGrant,
      repository,
      scope,
      maximumBytes: 8 * 1024 * 1024,
    });
  },
  gitWorktrees(projectGrant, repository) {
    return invoke<Worktree[]>("project_git_worktrees", { projectGrant, repository });
  },
};
