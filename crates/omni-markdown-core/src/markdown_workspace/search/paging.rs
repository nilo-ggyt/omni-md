//! Continue search with only the query, session identity, and final sorting key.
//! Rescan each page instead of caching the vault or every matching result.

use super::*;

/// Opaque active-query authority whose private fields cannot be forged by callers.
/// Local mutations, refresh, and closure revoke its original generation.
///
/// This is a live query: external renames/writes may change later rankings, and
/// refresh starts again. Continuation requires the same query, session, and real
/// root inode; these credentials retain neither document bodies nor file handles.
#[derive(Clone, Debug)]
pub struct MarkdownSearchAuthority(Arc<SearchAuthority>);

/// Share immutable finite authority metadata across pages/cursors rather than
/// repeatedly copying the bounded query and original root path.
#[derive(Debug)]
struct SearchAuthority {
    query: String,
    root: PathBuf,
    root_identity: (u64, u64),
    authority: Arc<AtomicU64>,
    generation: u64,
}

/// Pair the opaque last-result key with its producing query authority; callers
/// cannot fabricate a sorting position independently.
#[derive(Debug)]
pub struct MarkdownSearchCursor {
    authority: MarkdownSearchAuthority,
    after: SearchPosition,
}

/// A finite search page. A continuation exists only after observing a real
/// additional match, never merely because the returned page is full.
#[derive(Debug)]
pub struct MarkdownSearchPage {
    pub results: Vec<MarkdownSearchResult>,
    pub next: Option<MarkdownSearchCursor>,
    /// Terminal pages retain authority too: queued delivery must check whether
    /// local saves revoked this query even when there is no next cursor.
    pub authority: MarkdownSearchAuthority,
}

impl MarkdownSearchAuthority {
    /// Check current query authority without I/O. Concurrent external writes
    /// retain the live-query contract and require revalidation on selection.
    pub fn is_current(&self, session: &MarkdownSessionAccess, query: &str) -> bool {
        // Reject an overlong raw query through a bounded prefix check before
        // trim could scan an arbitrarily large whitespace-only input.
        query.chars().take(MAX_QUERY_CHARACTERS + 1).count() <= MAX_QUERY_CHARACTERS
            && self.0.query == query.trim()
            && self.0.root == session.canonical_root
            && Arc::ptr_eq(&self.0.authority, &session.search_generation)
            && session.search_is_current(self.0.generation)
    }
}

impl MarkdownSessionAccess {
    /// Return the next title-first, path-ordered page while retaining at most
    /// K+1 candidates and one bounded query.
    ///
    /// Each page costs one current-vault scan, O(K) retained results, and at most
    /// one transient candidate backing. External concurrent changes do not gain
    /// an atomic whole-vault snapshot guarantee; refresh revisits the updated ranking.
    ///
    /// # Errors
    /// Cancellation/local mutations supersede the request; wrong query, session,
    /// root identity, or credentials invalidate continuation. Nonempty queries
    /// contain at most 128 scalars. Actual file/encoding/storage failures retain
    /// their typed outcome and publish no partial results.
    pub fn search_notes_page_cancellable(
        &self,
        query: &str,
        requested_limit: usize,
        cursor: Option<&MarkdownSearchCursor>,
        cancelled: &AtomicBool,
    ) -> Result<MarkdownSearchPage, MarkdownWorkspaceError> {
        if cancelled.load(AtomicOrdering::Acquire) {
            // Pre-start cancellation neither allocates a generation nor lets
            // this old request revoke a later valid search.
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        if query.chars().take(MAX_QUERY_CHARACTERS + 1).count() > MAX_QUERY_CHARACTERS {
            // Apply the raw scalar budget before trim; padding cannot bypass
            // the bounded entry point or advance the search generation.
            return Err(MarkdownWorkspaceError::InvalidSearchCursor);
        }
        let query = query.trim();
        if query.is_empty() {
            // The native owner restores directory browsing for an empty query;
            // do not issue empty-query credentials or allocate another generation.
            return Err(MarkdownWorkspaceError::InvalidSearchCursor);
        }
        let root = resolve_existing_directory(&self.canonical_root, "")?;
        let metadata =
            fs::metadata(&root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        let root_identity = (metadata.dev(), metadata.ino());
        let authority = if let Some(cursor) = cursor {
            let previous = &cursor.authority.0;
            if previous.query != query
                || previous.root != root
                || previous.root_identity != root_identity
                || !Arc::ptr_eq(&previous.authority, &self.search_generation)
            {
                // Equal path strings cannot prove the same session/root inode.
                // Reject cross-vault, recreated-root, and changed-query credentials.
                return Err(MarkdownWorkspaceError::InvalidSearchCursor);
            }
            if !self.search_is_current(previous.generation) {
                // A local mutation, refresh, newer query, or closure already
                // revoked this query; its old final key cannot be revived.
                return Err(MarkdownWorkspaceError::SearchSuperseded);
            }
            cursor.authority.clone()
        } else {
            // Only a new query allocates a generation. Continuation reuses the
            // original generation instead of revoking its own opaque credentials.
            let generation = self
                .search_generation
                .fetch_add(1, AtomicOrdering::AcqRel)
                .wrapping_add(1);
            MarkdownSearchAuthority(Arc::new(SearchAuthority {
                query: query.to_owned(),
                root,
                root_identity,
                authority: Arc::clone(&self.search_generation),
                generation,
            }))
        };
        let is_current = || {
            // The scan obeys both session-generation authority and this worker's
            // separately owned cancellation token.
            self.search_is_current(authority.0.generation)
                && !cancelled.load(AtomicOrdering::Acquire)
        };
        let limit = requested_limit.clamp(1, MAX_SEARCH_RESULTS);
        let scanned = self.scan_ranked_search(
            query,
            limit + 1,
            cursor.map(|value| &value.after),
            &is_current,
        );
        if !is_current() {
            // Recheck after the scan: cancellation must not return a partial
            // page or obsolete continuation authority to the UI.
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        let mut ranked = scanned?;
        let has_more = ranked.len() > limit;
        ranked.truncate(limit);
        let next = if has_more {
            // Only a real K+1 match proves continuation. Its key comes from the
            // last retained result, never the discarded sentinel.
            let last = ranked.last().expect("非空有限页才可能有后续匹配");
            Some(MarkdownSearchCursor {
                authority: authority.clone(),
                after: SearchPosition {
                    score: last.score,
                    relative_path: last.result.relative_path.clone(),
                },
            })
        } else {
            // Exactly K matches may already exhaust the query. No continuation
            // should direct the user to a fictional empty next page.
            None
        };
        Ok(MarkdownSearchPage {
            results: ranked
                .into_iter()
                .map(|item| {
                    // The ranking container is temporary. Transfer only finite
                    // public metadata/snippets while the cursor keeps its one final key.
                    item.result
                })
                .collect(),
            next,
            authority,
        })
    }
}

#[cfg(test)]
mod tests;
