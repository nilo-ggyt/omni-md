//! Publish a sorted directory page only after a complete scan, retaining at most
//! K+1 candidates rather than exposing an arbitrary filesystem enumeration prefix.

use super::*;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

const MAX_SCAN_ENTRIES: usize = 512;
const MAX_PAGE_ENTRIES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// At each tree depth, directories sort before notes; intermediate path segments
/// necessarily represent directories regardless of the final entry kind.
pub enum MarkdownWorkspaceEntryKind {
    Directory,
    Note,
}

/// Order parents before descendants, then directories before notes at each depth.
/// Compare same-kind display names by Unicode scalar order and break ties with
/// the full path. This preserves original names without normalization or pinyin collation.
pub fn compare_markdown_workspace_entries(
    left: &str,
    left_kind: MarkdownWorkspaceEntryKind,
    right: &str,
    right_kind: MarkdownWorkspaceEntryKind,
) -> Ordering {
    let mut left_parts = left.split('/').peekable();
    let mut right_parts = right.split('/').peekable();
    loop {
        // Borrow one segment at a time; path depth does not allocate sorting keys or name copies.
        match (left_parts.next(), right_parts.next()) {
            (Some(left_name), Some(right_name)) => {
                let left_directory = left_parts.peek().is_some()
                    || left_kind == MarkdownWorkspaceEntryKind::Directory;
                let right_directory = right_parts.peek().is_some()
                    || right_kind == MarkdownWorkspaceEntryKind::Directory;
                let order = right_directory.cmp(&left_directory).then_with(|| {
                    // Directory dots remain significant; a note's final .md suffix
                    // is excluded from its display stem. UTF-8 order agrees with scalar order.
                    workspace_display_name(left_name, left_directory)
                        .cmp(workspace_display_name(right_name, right_directory))
                });
                if order != Ordering::Equal {
                    // The first differing depth decides the order of the entire subtree.
                    return order;
                }
            }
            (None, Some(_)) => {
                // The shorter identical prefix is the ancestor and sorts first.
                return Ordering::Less;
            }
            (Some(_), None) => {
                // A descendant cannot precede its own parent.
                return Ordering::Greater;
            }
            (None, None) => {
                // Equal display titles still need a strict total order so the
                // continuation key cannot skip distinct real filesystem entries.
                return left.cmp(right).then_with(|| {
                    (left_kind == MarkdownWorkspaceEntryKind::Note)
                        .cmp(&(right_kind == MarkdownWorkspaceEntryKind::Note))
                });
            }
        }
    }
}

/// Borrow a display name without allocating another string for sorting.
fn workspace_display_name(name: &str, directory: bool) -> &str {
    if directory {
        // A directory's dots are ordinary parts of its name.
        name
    } else {
        // A validated note displays its stem. Preserve a hidden name when removing
        // its suffix would leave no nonempty stem.
        name.rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map_or(name, |(stem, _)| stem)
    }
}

/// Own one iterator in the depth-first path stack, never a cached directory layer.
struct DirectoryCursor {
    path: PathBuf,
    entries: fs::ReadDir,
}

#[derive(Clone)]
/// Bind page authority to the original root, session generation, and user collapse
/// intent without owning note content or native objects.
pub struct MarkdownWorkspacePageAuthority(Arc<WorkspacePageAuthority>);

struct WorkspacePageAuthority {
    root: PathBuf,
    root_identity: (u64, u64),
    generation: Arc<AtomicU64>,
    expected_generation: u64,
    collapsed: HashSet<String>,
}

impl MarkdownWorkspacePageAuthority {
    /// Check installation authority without I/O. Each scan step and final publication
    /// independently recheck the original root inode through the filesystem.
    pub fn is_current(&self, session: &MarkdownSessionAccess, collapsed: &HashSet<String>) -> bool {
        self.0.root == session.canonical_root
            && Arc::ptr_eq(&self.0.generation, &session.browse_generation)
            && self.0.expected_generation == self.0.generation.load(AtomicOrdering::Acquire)
            && self.0.collapsed == *collapsed
    }
}

/// Retain at most K+1 private candidates; only full-scan EOF permits public output.
enum WorkspaceCandidate {
    Directory(String),
    Note(MarkdownNoteSummary),
}

impl WorkspaceCandidate {
    /// Compare borrowed existing paths without opening content or copying titles.
    fn position(&self) -> (&str, MarkdownWorkspaceEntryKind) {
        match self {
            // A directory candidate owns only its relative identifier.
            Self::Directory(path) => (path, MarkdownWorkspaceEntryKind::Directory),
            // A note reuses the identifier already owned by its compact summary.
            Self::Note(note) => (&note.relative_path, MarkdownWorkspaceEntryKind::Note),
        }
    }
}

/// Keep only the previous page's final sorting key, not its entries or a full catalog.
struct WorkspacePosition {
    path: String,
    kind: MarkdownWorkspaceEntryKind,
}

/// Own K+1 candidates, the current depth stack, and the user's collapsed set,
/// rather than a vault-wide cache.
pub struct MarkdownWorkspacePager {
    authority: MarkdownWorkspacePageAuthority,
    collapse_bound: bool,
    stack: Vec<DirectoryCursor>,
    candidates: Vec<WorkspaceCandidate>,
    after: Option<WorkspacePosition>,
    page_limit: Option<usize>,
    exhausted: bool,
}

/// A finite page after complete traversal. The virtual root is absent; its
/// presentation adapter must reserve a separate row if that root is displayed.
pub struct MarkdownWorkspacePage {
    pub snapshot: MarkdownWorkspaceSnapshot,
    pub has_more: bool,
    pub authority: MarkdownWorkspacePageAuthority,
}

/// Pending carries no displayable partial page; Ready may be the final empty page.
pub enum MarkdownWorkspacePageProgress {
    Pending,
    Ready(MarkdownWorkspacePage),
}

impl MarkdownSessionAccess {
    /// Starting a directory traversal revokes older browsing without cancelling
    /// the independently owned search that may still be displayed.
    ///
    /// # Errors
    /// A missing/replaced root or unavailable metadata returns its storage error
    /// without publishing a cursor.
    pub fn open_workspace_pager(&self) -> Result<MarkdownWorkspacePager, MarkdownWorkspaceError> {
        let root = resolve_existing_directory(&self.canonical_root, "")?;
        let metadata =
            fs::metadata(&root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        let expected_generation = self
            .browse_generation
            .fetch_add(1, AtomicOrdering::AcqRel)
            .wrapping_add(1);
        Ok(MarkdownWorkspacePager {
            authority: MarkdownWorkspacePageAuthority(Arc::new(WorkspacePageAuthority {
                root,
                root_identity: (metadata.dev(), metadata.ino()),
                generation: Arc::clone(&self.browse_generation),
                expected_generation,
                collapsed: HashSet::new(),
            })),
            collapse_bound: false,
            stack: Vec::new(),
            candidates: Vec::new(),
            after: None,
            page_limit: None,
            exhausted: false,
        })
    }
}

impl MarkdownWorkspacePager {
    /// Advance at most 512 enumeration attempts. Before complete traversal EOF,
    /// expose only Pending, never an arbitrarily ordered filesystem prefix.
    ///
    /// Bind page width (1..=64) and collapse intent on the first step. Rescanning
    /// each page retains O(K + depth + collapsed entries) metadata and opens no
    /// note bodies. This is live continuation: new earlier-sorting entries require
    /// refresh, rather than an immutable whole-vault snapshot guarantee.
    ///
    /// # Errors
    /// Cancellation, structural publication, new browsing, or closure supersedes
    /// the request; same-path body saves preserve it. Changing width/collapse intent
    /// invalidates the cursor. Failure releases partial candidates and iterators
    /// permanently, so resetting a cancellation flag cannot revive old output.
    pub fn next_page_cancellable(
        &mut self,
        requested_limit: usize,
        scanned_entry_limit: usize,
        collapsed: &HashSet<String>,
        cancelled: &AtomicBool,
    ) -> Result<MarkdownWorkspacePageProgress, MarkdownWorkspaceError> {
        let result = self.scan_step(requested_limit, scanned_entry_limit, collapsed, cancelled);
        if result.is_err() {
            // Release capacity and close authority; a later flag reset cannot let
            // this failed cursor continue publishing its old partial page.
            self.stack = Vec::new();
            self.candidates = Vec::new();
            self.exhausted = true;
            self.close();
        }
        result
    }

    /// Recheck cancellation/generation and the original root inode before work
    /// and again before publishing a complete page.
    fn validate(&self, cancelled: &AtomicBool) -> Result<(), MarkdownWorkspaceError> {
        if cancelled.load(AtomicOrdering::Acquire)
            || self.authority.0.generation.load(AtomicOrdering::Acquire)
                != self.authority.0.expected_generation
        {
            // Revocation precedes I/O; an older owner cannot publish for a newer session.
            return Err(MarkdownWorkspaceError::WorkspaceSuperseded);
        }
        let root = resolve_existing_directory(&self.authority.0.root, "")?;
        let metadata =
            fs::metadata(root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        if (metadata.dev(), metadata.ino()) != self.authority.0.root_identity {
            // A recreated directory under the same path is not the original root.
            return Err(MarkdownWorkspaceError::InvalidWorkspaceCursor);
        }
        Ok(())
    }

    /// Read metadata only and retain unfinished candidates privately inside the pager.
    fn scan_step(
        &mut self,
        requested_limit: usize,
        scanned_entry_limit: usize,
        collapsed: &HashSet<String>,
        cancelled: &AtomicBool,
    ) -> Result<MarkdownWorkspacePageProgress, MarkdownWorkspaceError> {
        self.validate(cancelled)?;
        let limit = requested_limit.clamp(1, MAX_PAGE_ENTRIES);
        if requested_limit == 0
            || scanned_entry_limit == 0
            || self.page_limit.is_some_and(|previous| previous != limit)
            || (self.collapse_bound && self.authority.0.collapsed != *collapsed)
        {
            // A previous final key cannot authorize a different width or collapse
            // intent; the caller must explicitly start a refreshed traversal.
            return Err(MarkdownWorkspaceError::InvalidWorkspaceCursor);
        }
        if !self.collapse_bound {
            // No authority has escaped on the first step. Bind collapse intent
            // through the sole Arc owner and require exact equality thereafter.
            Arc::get_mut(&mut self.authority.0)
                .expect("首步authority由pager独占")
                .collapsed = collapsed.clone();
            self.collapse_bound = true;
            self.page_limit = Some(limit);
        }
        if self.exhausted {
            // Once the terminal page is proved, later calls open no directories.
            return Ok(MarkdownWorkspacePageProgress::Ready(self.empty_page()));
        }
        if self.stack.is_empty() {
            // Rescan the complete tree for each logical page, excluding the
            // previous final key before retaining any candidate.
            self.candidates = Vec::with_capacity(limit + 1);
            let root = self.authority.0.root.clone();
            let entries =
                fs::read_dir(&root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            self.stack.push(DirectoryCursor {
                path: root,
                entries,
            });
        }
        for _ in 0..scanned_entry_limit.min(MAX_SCAN_ENTRIES) {
            // Charge iterator EOF too, so chains of empty directories cannot
            // bypass the 512-iteration work bound and cancellation checkpoints.
            if cancelled.load(AtomicOrdering::Acquire)
                || self.authority.0.generation.load(AtomicOrdering::Acquire)
                    != self.authority.0.expected_generation
            {
                // An operation that revoked browsing cannot deliver an unfinished
                // page even when this step still has enumeration budget remaining.
                return Err(MarkdownWorkspaceError::WorkspaceSuperseded);
            }
            let Some(cursor) = self.stack.last_mut() else {
                // Only complete traversal EOF proves the K+1 sentinel and terminal status.
                break;
            };
            let relative = relative_workspace_path(&self.authority.0.root, &cursor.path)?;
            resolve_existing_directory(&self.authority.0.root, &relative)?;
            let Some(entry) = cursor.entries.next() else {
                // Release an exhausted iterator immediately before returning to its parent.
                self.stack.pop();
                continue;
            };
            let entry = entry.map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            let kind = entry
                .file_type()
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            if kind.is_symlink() {
                // Links are neither visible notes nor authority to traverse an external tree.
                continue;
            }
            if cursor.path == self.authority.0.root
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME))
            {
                // The root attachment tree stores assets, not sortable note entries.
                continue;
            }
            let path = entry.path();
            let (relative, entry_kind) = if kind.is_dir() {
                let relative = relative_workspace_path(&self.authority.0.root, &path)?;
                if !collapsed.contains(&relative) {
                    // A collapsed directory itself remains visible; acquire a child
                    // iterator only when its descendants are part of the user's intent.
                    let checked = resolve_existing_directory(&self.authority.0.root, &relative)?;
                    let entries = fs::read_dir(&checked)
                        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
                    self.stack.push(DirectoryCursor {
                        path: checked,
                        entries,
                    });
                }
                (relative, MarkdownWorkspaceEntryKind::Directory)
            } else if kind.is_file() && has_markdown_extension(&path) {
                // Obtain only this entry's path scratch first. Entries that lose
                // candidate admission need neither a metadata stat nor a title allocation.
                (
                    relative_markdown_path(&self.authority.0.root, &path)?,
                    MarkdownWorkspaceEntryKind::Note,
                )
            } else {
                // Unsupported entries consume scan work but no displayable result slot.
                continue;
            };
            let Some(index) = self.admit_candidate(&relative, entry_kind, limit) else {
                // Earlier, duplicate, and worse entries release their current
                // path without constructing another summary record.
                continue;
            };
            let candidate = match entry_kind {
                MarkdownWorkspaceEntryKind::Directory => {
                    // Move the existing path into its sole directory record;
                    // admission already released a former worst candidate if needed.
                    WorkspaceCandidate::Directory(relative)
                }
                MarkdownWorkspaceEntryKind::Note => {
                    // Stat only admitted regular Markdown candidates; their body
                    // is never opened or mapped for directory presentation.
                    let metadata = entry
                        .metadata()
                        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
                    WorkspaceCandidate::Note(MarkdownNoteSummary {
                        relative_path: relative,
                        title: markdown_title(&path)?,
                        byte_count: metadata.len(),
                        modified_at_ms: modified_at_ms(&metadata),
                    })
                }
            };
            self.candidates.insert(index, candidate);
        }
        self.validate(cancelled)?;
        if !self.stack.is_empty() {
            // The complete ordering is not yet known, so this step can expose only Pending.
            return Ok(MarkdownWorkspacePageProgress::Pending);
        }
        let has_more = self.candidates.len() > limit;
        self.candidates.truncate(limit);
        self.after = self.candidates.last().map(|candidate| {
            // Derive continuation from the last actual returned entry, never
            // from the discarded K+1 sentinel.
            let (path, kind) = candidate.position();
            WorkspacePosition {
                path: path.to_owned(),
                kind,
            }
        });
        self.exhausted = !has_more;
        let mut page = self.empty_page();
        page.has_more = has_more;
        for candidate in std::mem::take(&mut self.candidates) {
            // Move finite metadata only; the native adapter uses the same public
            // comparator to reconstruct mixed directory/note tree order.
            match candidate {
                WorkspaceCandidate::Directory(path) => page.snapshot.directories.push(path),
                WorkspaceCandidate::Note(note) => page.snapshot.notes.push(note),
            }
        }
        Ok(MarkdownWorkspacePageProgress::Ready(page))
    }

    /// Admit a path before building its summary and evict the old worst entry
    /// before replacement, keeping at most K+1 candidate records. The current
    /// enumeration path and one continuation key still own their separate finite storage.
    fn admit_candidate(
        &mut self,
        path: &str,
        kind: MarkdownWorkspaceEntryKind,
        limit: usize,
    ) -> Option<usize> {
        if self.after.as_ref().is_some_and(|after| {
            compare_markdown_workspace_entries(path, kind, &after.path, after.kind)
                != Ordering::Greater
        }) {
            // Live rescans do not repeat the earlier sorted prefix; newly created
            // entries before this key become visible after an explicit refresh.
            return None;
        }
        let index = match self.candidates.binary_search_by(|existing| {
            let (existing_path, existing_kind) = existing.position();
            compare_markdown_workspace_entries(existing_path, existing_kind, path, kind)
        }) {
            Ok(_) => {
                // Concurrent filesystem changes can repeat an enumerated path;
                // retain that identity at most once in this candidate page.
                return None;
            }
            // The failed lookup provides the exact insertion position in total order.
            Err(index) => index,
        };
        if index > limit {
            // Do not insert a worse entry even transiently, avoiding K+2 candidates.
            return None;
        }
        if self.candidates.len() > limit {
            // Release the old sentinel before insertion to maintain the K+1 object bound.
            self.candidates.pop();
        }
        Some(index)
    }

    /// Even an empty terminal page carries authority: a late terminal result
    /// must be rejectable under the same installation checks as a populated page.
    fn empty_page(&self) -> MarkdownWorkspacePage {
        MarkdownWorkspacePage {
            snapshot: MarkdownWorkspaceSnapshot {
                directories: Vec::new(),
                notes: Vec::new(),
            },
            has_more: false,
            authority: self.authority.clone(),
        }
    }

    /// Revoke only this generation and release its iterators, candidate capacity,
    /// and continuation key without affecting newer browsing. Shared authority
    /// and collapse intent remain alive until their final page/pager owner drops.
    pub fn close(&mut self) {
        let _ = self.authority.0.generation.compare_exchange(
            self.authority.0.expected_generation,
            self.authority.0.expected_generation.wrapping_add(1),
            AtomicOrdering::AcqRel,
            AtomicOrdering::Acquire,
        );
        self.stack = Vec::new();
        self.candidates = Vec::new();
        self.after = None;
        self.exhausted = true;
    }
}

#[cfg(test)]
mod tests;
