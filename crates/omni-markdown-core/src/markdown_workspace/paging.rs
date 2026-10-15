//! Foreground-only, budgeted depth-first metadata pages for hierarchical vault navigation.
//!
//! The pager owns directory iterators only while the caller explicitly retains it. It never
//! opens note bodies, preloads a complete catalog, starts a worker, or creates persistent state.

use super::*;
use std::collections::HashSet;

/// One current-directory cursor; depth-first traversal bounds retained descriptors by path depth.
struct DirectoryCursor {
    path: PathBuf,
    entries: fs::ReadDir,
}

/// Session-owned traversal state released by dropping or explicitly closing the active pager.
///
/// Directory order follows the filesystem within each bounded page; callers can sort the returned
/// metadata for display. Refresh starts a new traversal, so concurrent mutations need no stale index.
pub struct MarkdownWorkspacePager {
    canonical_root: PathBuf,
    stack: Vec<DirectoryCursor>,
}

/// One bounded metadata result and whether another scan may still contain entries.
pub struct MarkdownWorkspacePage {
    /// Note/directory identities only; note bodies remain unopened.
    pub snapshot: MarkdownWorkspaceSnapshot,
    /// False only when all owned directory iterators have been exhausted or explicitly closed.
    pub has_more: bool,
}

impl MarkdownSessionAccess {
    /// Creates a pager only for an explicitly active foreground workspace.
    ///
    /// # Errors
    /// Returns the existing confinement/storage error if the authoritative root cannot be opened.
    pub fn open_workspace_pager(&self) -> Result<MarkdownWorkspacePager, MarkdownWorkspaceError> {
        // Repeat root confinement before acquiring the sole initial directory descriptor.
        let root = resolve_existing_directory(&self.canonical_root, "")?;
        let entries =
            fs::read_dir(&root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        Ok(MarkdownWorkspacePager {
            canonical_root: self.canonical_root.clone(),
            stack: vec![DirectoryCursor {
                path: root,
                entries,
            }],
        })
    }
}

impl MarkdownWorkspacePager {
    /// Reads at most the supplied result and filesystem-entry budgets, skipping collapsed subtrees.
    ///
    /// Callers must create a fresh pager after changing collapse state or mutating the vault. Empty
    /// pages can still have `has_more` when the scan budget encountered only ignored assets/files.
    ///
    /// # Errors
    /// Returns a confinement/storage error for an invalidated directory or unreadable entry.
    pub fn next_page(
        &mut self,
        requested_limit: usize,
        scanned_entry_limit: usize,
        collapsed_directories: &HashSet<String>,
    ) -> Result<MarkdownWorkspacePage, MarkdownWorkspaceError> {
        let mut snapshot = MarkdownWorkspaceSnapshot {
            directories: Vec::new(),
            notes: Vec::new(),
        };
        let mut scanned = 0;
        // Both result count and ignored entries are charged; unrelated files cannot monopolize a turn.
        while snapshot.directories.len() + snapshot.notes.len() < requested_limit
            && scanned < scanned_entry_limit
        {
            let Some(cursor) = self.stack.last_mut() else {
                // All iterators have been released; no hidden catalog or descriptor remains.
                break;
            };
            // Reject renamed/link-substituted directory identities before exposing another entry.
            let relative_directory = relative_workspace_path(&self.canonical_root, &cursor.path)?;
            resolve_existing_directory(&self.canonical_root, &relative_directory)?;
            let Some(entry) = cursor.entries.next() else {
                // Depth-first completion releases this descriptor before resuming its parent.
                self.stack.pop();
                continue;
            };
            scanned += 1;
            let entry = entry.map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            let kind = entry
                .file_type()
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            // A symlink is neither navigable metadata nor permission to traverse an external tree.
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            // The reserved asset tree is not a note subtree even when it contains .md files.
            if cursor.path == self.canonical_root
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME))
            {
                continue;
            }
            if kind.is_dir() {
                // Directory metadata is visible even when the user collapsed its descendants.
                let relative = relative_workspace_path(&self.canonical_root, &path)?;
                snapshot.directories.push(relative.clone());
                // Collapsed rows remain visible but acquire no descendant directory descriptor.
                if !collapsed_directories.contains(&relative) {
                    // Descend immediately instead of queuing every directory in a potentially huge vault.
                    let checked = resolve_existing_directory(&self.canonical_root, &relative)?;
                    let entries = fs::read_dir(&checked)
                        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
                    self.stack.push(DirectoryCursor {
                        path: checked,
                        entries,
                    });
                }
            } else if kind.is_file() && has_markdown_extension(&path) {
                // Collect only regular-note metadata; selecting a row later repeats authoritative validation.
                let metadata = entry
                    .metadata()
                    .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
                snapshot.notes.push(MarkdownNoteSummary {
                    relative_path: relative_markdown_path(&self.canonical_root, &path)?,
                    title: markdown_title(&path)?,
                    byte_count: metadata.len(),
                    modified_at_ms: modified_at_ms(&metadata),
                });
            }
            // Non-Markdown files and special filesystem nodes consume scan budget without rows.
        }
        // Sort only the current bounded page, without retaining a full-vault index across pages.
        snapshot.directories.sort_unstable();
        snapshot
            .notes
            .sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(MarkdownWorkspacePage {
            snapshot,
            has_more: !self.stack.is_empty(),
        })
    }

    /// Releases every active directory descriptor without reading the remainder of the vault.
    pub fn close(&mut self) {
        // The caller owns the traversal; close is idempotent and creates no asynchronous cleanup.
        self.stack.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pages reach every note without ever exceeding a per-turn metadata or scan budget.
    #[test]
    fn bounded_pages_cover_large_vault_and_close_iterators() {
        let temporary = tempfile::tempdir().expect("create isolated pager vault");
        // Enough files force several pages and verify that the old first-page ceiling cannot hide notes.
        for index in 0..43 {
            fs::write(temporary.path().join(format!("note-{index}.md")), "body")
                .expect("write fixture note");
        }
        let workspace =
            MarkdownWorkspace::new(temporary.path().to_owned(), temporary.path().join("data"));
        let session = workspace
            .open_session()
            .expect("open explicit workspace session");
        let mut pager = session.open_workspace_pager().expect("open pager");
        let mut paths = HashSet::new();
        // Exhaust the finite fixture through actual pager progress; each turn is independently
        // capped at seven returned rows and eleven scanned entries, including ignored directories.
        loop {
            let page = pager
                .next_page(7, 11, &HashSet::new())
                .expect("read bounded page");
            assert!(page.snapshot.notes.len() + page.snapshot.directories.len() <= 7);
            paths.extend(
                page.snapshot
                    .notes
                    .into_iter()
                    .map(|note| note.relative_path),
            );
            // Exhaustion is observed through the actual iterator state rather than a guessed total count.
            if !page.has_more {
                break;
            }
        }
        assert_eq!(paths.len(), 43);
        pager.close();
        assert!(
            !pager
                .next_page(7, 11, &HashSet::new())
                .expect("closed pager is empty")
                .has_more
        );
    }

    /// Collapsed and reserved subtrees remain excluded while directory identities stay visible.
    #[test]
    fn collapse_and_symlinks_do_not_expose_hidden_or_external_notes() {
        let temporary = tempfile::tempdir().expect("create isolated pager vault");
        let external = tempfile::tempdir().expect("create external sentinel directory");
        fs::create_dir(temporary.path().join("folder")).expect("create subtree");
        fs::create_dir(temporary.path().join("attachments")).expect("create reserved subtree");
        fs::write(temporary.path().join("folder/hidden.md"), "hidden")
            .expect("write collapsed note");
        fs::write(temporary.path().join("attachments/asset.md"), "asset")
            .expect("write reserved asset");
        fs::write(external.path().join("outside.md"), "outside").expect("write external sentinel");
        std::os::unix::fs::symlink(external.path(), temporary.path().join("linked"))
            .expect("link external directory");
        let workspace =
            MarkdownWorkspace::new(temporary.path().to_owned(), temporary.path().join("data"));
        let session = workspace.open_session().expect("open explicit session");
        let mut pager = session.open_workspace_pager().expect("open pager");
        let page = pager
            .next_page(20, 100, &HashSet::from(["folder".to_owned()]))
            .expect("read collapsed page");
        assert_eq!(page.snapshot.directories, ["folder"]);
        assert!(page.snapshot.notes.is_empty());
        assert!(!page.has_more);
    }
}
