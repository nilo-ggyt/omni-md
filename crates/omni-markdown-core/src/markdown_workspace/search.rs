//! Cancellable session-owned vault search with bounded ranking and streaming matching.

use super::*;
use crate::immutable_snapshot::SnapshotDirectory;

mod paging;
pub use paging::{MarkdownSearchAuthority, MarkdownSearchCursor, MarkdownSearchPage};

impl MarkdownSessionAccess {
    /// Search the active vault with generation cancellation and bounded retained results.
    pub fn search_notes(
        &self,
        query: &str,
        requested_limit: usize,
    ) -> Result<Vec<MarkdownSearchResult>, MarkdownWorkspaceError> {
        // Preserve the established synchronous API without allocating an external cancellation token.
        self.search_notes_with_cancellation(query, requested_limit, None)
    }

    /// Search with an additional caller-owned cancellation token checked before startup.
    ///
    /// A foreground owner sets the token before joining its worker. This closes the
    /// cancel-before-worker-start race without moving task ownership into the core.
    pub fn search_notes_cancellable(
        &self,
        query: &str,
        requested_limit: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<MarkdownSearchResult>, MarkdownWorkspaceError> {
        // Borrow the token only for this synchronous call; the caller owns its worker lifetime.
        self.search_notes_with_cancellation(query, requested_limit, Some(cancelled))
    }

    /// Coordinate bounded ranking while both session and caller cancellation remain authoritative.
    fn search_notes_with_cancellation(
        &self,
        query: &str,
        requested_limit: usize,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<MarkdownSearchResult>, MarkdownWorkspaceError> {
        // Check caller cancellation before generation allocation: close may precede worker startup.
        if cancelled.is_some_and(|token| {
            // Observe caller cancellation before publishing or continuing a search generation.
            token.load(AtomicOrdering::Acquire)
        }) {
            // A cancelled worker owns no scan and must not supersede a later valid request.
            return Ok(Vec::new());
        }
        // Keep the query scalar-aligned and bounded before allocating its streaming matcher.
        let query = query.chars().take(MAX_QUERY_CHARACTERS).collect::<String>();
        let query = query.trim();
        if query.is_empty() {
            // Clearing a query cancels older scans and intentionally displays no search results.
            self.search_generation.fetch_add(1, AtomicOrdering::AcqRel);
            self.browse_generation.fetch_add(1, AtomicOrdering::AcqRel);
            return Ok(Vec::new());
        }

        // Only a live nonempty request supersedes the previous session search generation.
        let generation = self
            .search_generation
            .fetch_add(1, AtomicOrdering::AcqRel)
            .wrapping_add(1);
        // Both cancellation domains remain authoritative after the scan starts.
        let is_current = || {
            // A search remains current only while both its generation and the caller-owned cancellation token permit it.
            self.search_is_current(generation)
                && !cancelled.is_some_and(|token| {
                    // Observe caller cancellation before publishing or continuing a search generation.
                    token.load(AtomicOrdering::Acquire)
                })
        };
        let result_limit = requested_limit.clamp(1, MAX_SEARCH_RESULTS);
        let scanned = self.scan_ranked_search(query, result_limit, None, &is_current);
        if !is_current() {
            // Preserve this complete-vector API's empty-result cancellation
            // contract; the paged API separately rejects superseded credentials.
            return Ok(Vec::new());
        }
        Ok(scanned?
            .into_iter()
            .map(|item| {
                // Transfer at most K metadata records without exposing internal rank keys.
                item.result
            })
            .collect())
    }

    /// Share one-candidate source acquisition and bounded ranking. The caller
    /// owns the complete-vector or paged API's distinct cancellation outcome.
    fn scan_ranked_search(
        &self,
        query: &str,
        result_limit: usize,
        after: Option<&SearchPosition>,
        is_current: &impl Fn() -> bool,
    ) -> Result<Vec<RankedSearchResult>, MarkdownWorkspaceError> {
        let pattern = SearchPattern::new(query.as_bytes());
        let mut ranked = Vec::<RankedSearchResult>::with_capacity(result_limit + 1);
        // Reuse only an empty private scratch directory during this scan. Each
        // candidate's backing and the directory owner end before this call returns.
        let mut snapshot_directory = None;
        let walk_result = walk_markdown_workspace(&self.canonical_root, |entry| {
            // Check cancellation for directory entries too; a directory-only tree
            // cannot continue traversal after its request authority ends.
            if !is_current() {
                // Stop traversal and release open directory iterators without
                // retaining a background cursor.
                return Ok(WalkControl::Stop);
            }
            let MarkdownFilesystemEntry::Note(path) = entry else {
                // Directories guide traversal but are neither body candidates nor note results.
                return Ok(WalkControl::Continue);
            };
            // Match the title first: title hits rank higher and need no body mapping.
            let title = markdown_title(path)?;
            let title_match = pattern.find(title.as_bytes(), is_current)?;
            // Exclude the already-returned rank prefix before body acquisition;
            // earlier pages need neither repeated body mappings nor new snippets.
            let score = if title_match.is_some() {
                // A title match already proves the highest rank without reading content.
                2
            } else {
                // Only the body can match; check its possible rank before deciding
                // whether this page may acquire that candidate's source.
                1
            };
            let relative_path = relative_markdown_path(&self.canonical_root, path)?;
            if after.is_some_and(|last| {
                // Keyset continuation must be strictly after the previous final
                // result; relative-path bytes break ties independently of read_dir order.
                compare_search_position(score, &relative_path, last) != Ordering::Greater
            }) {
                // An already-returned prefix cannot enter this page; leave its body unopened.
                return Ok(WalkControl::Continue);
            }
            let (score, snippet) = if title_match.is_some() {
                // A title hit produces no body snippet and acquires no content mapping.
                (2_u8, None)
            } else {
                // Acquire only the current candidate transiently; no borrowed
                // source text may escape this inspection callback.
                if snapshot_directory.is_none() {
                    // Acquire private scratch only at the first body candidate;
                    // a title-only search performs no snapshot I/O.
                    snapshot_directory = Some(SnapshotDirectory::new().map_err(|_| {
                        // Failure to establish private snapshot ownership is a
                        // storage error, never permission to map the mutable user file.
                        MarkdownWorkspaceError::StorageUnavailable
                    })?);
                }
                let body_match = inspect_bounded_markdown_text_cancellable(
                    path,
                    MAX_SEARCH_FILE_BYTES,
                    is_current,
                    |text| {
                        // Validate all candidate UTF-8 after the first hit too.
                        // Derive the bounded snippet only after valid real EOF.
                        let start = pattern.find_bounded(text, is_current)?;
                        start
                            .map(|start| {
                                bounded_file_search_snippet(
                                    text,
                                    start,
                                    pattern.byte_len(),
                                    is_current,
                                )
                            })
                            .transpose()
                    },
                    snapshot_directory.as_mut(),
                )?
                .value;
                let Some(snippet) = body_match else {
                    // A nonmatch occupies no result slot; inspection has already
                    // reclaimed its transient source before this branch continues.
                    return Ok(WalkControl::Continue);
                };
                (1_u8, Some(snippet))
            };
            // Retain only the current best bounded results. Path order resolves
            // equal scores and truncation immediately releases an evicted record.
            ranked.push(RankedSearchResult {
                score,
                result: MarkdownSearchResult {
                    relative_path,
                    title,
                    snippet,
                },
            });
            ranked.sort_unstable_by(compare_ranked_search_results);
            ranked.truncate(result_limit);
            Ok(WalkControl::Continue)
        });

        // Preserve actual traversal failures. The caller separately rechecks
        // cancellation before publication, never presenting a partial search page.
        walk_result?;
        Ok(ranked)
    }

    /// Invalidate the current generation without retaining a worker or a callback.
    pub fn cancel_search(&self) {
        // The foreground owner separately cancels its external token and joins its owned worker.
        self.cancel_active_search();
    }

    /// Compare a request with the current live session search generation.
    fn search_is_current(&self, generation: u64) -> bool {
        // Acquire pairs with invalidation so an obsolete scan cannot publish results.
        self.search_generation.load(AtomicOrdering::Acquire) == generation
    }

    /// After same-path content publication, revoke only searches that may depend
    /// on the old bytes. Path, kind, stem, root, and collapse intent are unchanged;
    /// a note inode replacement is not part of the directory cursor's identity.
    pub(super) fn invalidate_content_search(&self) {
        self.search_generation.fetch_add(1, AtomicOrdering::AcqRel);
    }

    /// Creation, path changes, explicit cancellation, and closure revoke both
    /// search and browsing. Same-path content saves use the narrower operation above.
    pub(super) fn cancel_active_search(&self) {
        self.invalidate_content_search();
        // Collection/order authority has ended; neither an old continuation key
        // nor an already queued directory page may continue delivery.
        self.browse_generation.fetch_add(1, AtomicOrdering::AcqRel);
    }
}

/// Query-sized KMP state, with ASCII folding that preserves original UTF-8 offsets.
struct SearchPattern {
    bytes: Vec<u8>,
    prefix_lengths: Vec<usize>,
    ascii_case_insensitive: bool,
}

impl SearchPattern {
    /// Build a reusable linear-time matcher without copying any candidate document.
    fn new(bytes: &[u8]) -> Self {
        let ascii_case_insensitive = bytes.is_ascii();
        let bytes = if ascii_case_insensitive {
            // ASCII normalization preserves byte width and is shared by every scanned candidate.
            bytes.iter().map(u8::to_ascii_lowercase).collect()
        } else {
            // Non-ASCII queries remain literal so byte offsets are valid scalar boundaries.
            bytes.to_vec()
        };
        let mut prefix_lengths = vec![0; bytes.len()];
        let mut prefix = 0;
        for index in 1..bytes.len() {
            // Compute borders once so repeated partial matches never restart a document scan.
            while prefix > 0 && bytes[index] != bytes[prefix] {
                // A mismatch falls back to a shorter valid border instead of revisiting source bytes.
                prefix = prefix_lengths[prefix - 1];
            }
            if bytes[index] == bytes[prefix] {
                // Extend the longest matching border used by later streaming comparisons.
                prefix += 1;
                prefix_lengths[index] = prefix;
            }
        }
        Self {
            bytes,
            prefix_lengths,
            ascii_case_insensitive,
        }
    }

    /// Return the original-width match length for bounded snippet construction.
    fn byte_len(&self) -> usize {
        // ASCII folding does not change this length, and Unicode queries were not folded.
        self.bytes.len()
    }

    /// Carry one KMP prefix across real 4 KiB leases. Record only the first hit,
    /// while still requiring the remaining candidate bytes to form valid UTF-8.
    fn find_bounded(
        &self,
        text: &BoundedMarkdownText,
        is_current: &impl Fn() -> bool,
    ) -> Result<Option<usize>, MarkdownWorkspaceError> {
        let mut matched = 0;
        let mut found = self.bytes.is_empty().then_some(0);
        text.visit_validated_bytes(is_current, |start, bytes| {
            if found.is_some() {
                // Matching can stop, but validation continues through the actual
                // tail; an invalid file cannot publish an otherwise useful prefix hit.
                return Ok(());
            }
            for (index, source_byte) in bytes.iter().copied().enumerate() {
                let byte = if self.ascii_case_insensitive {
                    // ASCII case folding preserves the original source byte offsets.
                    source_byte.to_ascii_lowercase()
                } else {
                    // Non-ASCII queries retain literal bytes and scalar alignment.
                    source_byte
                };
                while matched > 0 && byte != self.bytes[matched] {
                    // Reuse a shorter proven query border without rereading source bytes.
                    matched = self.prefix_lengths[matched - 1];
                }
                if byte == self.bytes[matched] {
                    // Advance the same cross-window query prefix until its first full match.
                    matched += 1;
                    if matched == self.bytes.len() {
                        found = Some(start + index + 1 - matched);
                        // Stop only this window's KMP work, not the enclosing UTF-8 scan.
                        break;
                    }
                }
            }
            Ok(())
        })?;
        Ok(found)
    }

    /// Scan one borrowed candidate, periodically yielding to both cancellation domains.
    fn find(
        &self,
        haystack: &[u8],
        is_current: impl Fn() -> bool,
    ) -> Result<Option<usize>, MarkdownWorkspaceError> {
        if self.bytes.is_empty() {
            // Empty literal matching is well-defined internally; public query coordination filters it.
            return Ok(Some(0));
        }
        let mut matched = 0;
        for (index, source_byte) in haystack.iter().copied().enumerate() {
            // Poll only at a bounded byte interval, including before the first candidate byte.
            if index % SEARCH_CANCELLATION_BYTE_INTERVAL == 0 && !is_current() {
                // Early cancellation unwinds the temporary mapping and bounded matcher state.
                return Err(MarkdownWorkspaceError::SearchSuperseded);
            }
            let byte = if self.ascii_case_insensitive {
                // Fold candidate ASCII in the same width-preserving space as the query.
                source_byte.to_ascii_lowercase()
            } else {
                // Literal Unicode bytes retain their original offset interpretation.
                source_byte
            };
            while matched > 0 && byte != self.bytes[matched] {
                // Preserve amortized linear progress by reusing an already validated border.
                matched = self.prefix_lengths[matched - 1];
            }
            if byte == self.bytes[matched] {
                // A matching byte advances the query prefix without allocating candidate text.
                matched += 1;
                if matched == self.bytes.len() {
                    // Stop at the first complete match; callers need one snippet per candidate.
                    return Ok(Some(index + 1 - matched));
                }
            }
        }
        // Exhaustion without a complete query is an ordinary nonmatching candidate.
        Ok(None)
    }
}

/// Bounded user-facing metadata plus its temporary title/body ranking score.
struct RankedSearchResult {
    score: u8,
    result: MarkdownSearchResult,
}

/// Keep only the last returned sorting key, without content, snippets, mappings,
/// or a live directory iterator.
#[derive(Debug)]
struct SearchPosition {
    score: u8,
    relative_path: String,
}

/// Admit a continuation candidate only when it sorts strictly after the last result.
fn compare_search_position(score: u8, path: &str, last: &SearchPosition) -> Ordering {
    last.score.cmp(&score).then_with(|| {
        // Use the first page's same byte order so ties neither repeat nor disappear.
        path.cmp(&last.relative_path)
    })
}

/// Prefer title hits, then a stable relative-path ordering independent from filesystem iteration.
fn compare_ranked_search_results(
    left: &RankedSearchResult,
    right: &RankedSearchResult,
) -> Ordering {
    // Reversing score order keeps the strongest bounded results before truncation.
    right.score.cmp(&left.score).then_with(|| {
        // Break otherwise equal search ranks by stable vault-relative path ordering.
        left.result.relative_path.cmp(&right.result.relative_path)
    })
}

/// After complete UTF-8 validation, borrow only the small interval surrounding
/// the hit that can satisfy the established scalar-count snippet context.
fn bounded_file_search_snippet(
    text: &BoundedMarkdownText,
    match_start: usize,
    match_bytes: usize,
    is_current: &impl Fn() -> bool,
) -> Result<String, MarkdownWorkspaceError> {
    let match_end = match_start
        .checked_add(match_bytes)
        .filter(|end| *end <= text.len())
        .ok_or(MarkdownWorkspaceError::StorageUnavailable)?;
    // Reverse nth(N) needs up to N+1 preceding scalars; the suffix needs at most
    // N+1 boundaries. At four bytes per scalar plus a 128-scalar query, the whole
    // requested interval stays below the source's 4 KiB lease limit.
    let context = (SNIPPET_CONTEXT_CHARACTERS + 1) * 4;
    let begin = match_start.saturating_sub(context);
    let end = match_end.saturating_add(context).min(text.len());
    text.with_bytes(begin..end, is_current, |bytes| {
        let mut skip = 0;
        while skip < bytes.len() && bytes[skip] & 0xc0 == 0x80 {
            // An arbitrary start can split only one already-validated scalar;
            // skip its continuation tail before decoding the finite context.
            skip += 1;
        }
        let bytes = &bytes[skip..];
        let valid = match std::str::from_utf8(bytes) {
            // A scalar-aligned complete interval needs no trimming.
            Ok(value) => value,
            Err(error) if error.error_len().is_none() => {
                // The bounded interval may end within the next scalar. Trim only
                // that residual; the already-validated real EOF cannot be incomplete.
                std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .map_err(|_| MarkdownWorkspaceError::InvalidUtf8Document)?
            }
            // A definite invalid sequence is not an interval-boundary artifact.
            Err(_) => return Err(MarkdownWorkspaceError::InvalidUtf8Document),
        };
        let local = match_start
            .checked_sub(begin + skip)
            .filter(|local| {
                valid.is_char_boundary(*local)
                    && local
                        .checked_add(match_bytes)
                        .is_some_and(|end| valid.is_char_boundary(end))
            })
            .ok_or(MarkdownWorkspaceError::StorageUnavailable)?;
        Ok(bounded_search_snippet(valid, local, match_bytes))
    })?
}

/// Materialize only scalar-aligned context and remove control/duplicate whitespace for display.
fn bounded_search_snippet(text: &str, match_start: usize, match_bytes: usize) -> String {
    // The matcher returns scalar-aligned UTF-8 bounds; extend by a fixed number of surrounding scalars.
    let match_end = match_start.saturating_add(match_bytes).min(text.len());
    let context_start = text[..match_start]
        .char_indices()
        .rev()
        .nth(SNIPPET_CONTEXT_CHARACTERS)
        .map_or(0, |(index, _)| {
            // Retain only a scalar-aligned source byte position for later bounded slicing.
            index
        });
    let context_end = text[match_end..]
        .char_indices()
        .nth(SNIPPET_CONTEXT_CHARACTERS)
        .map_or(text.len(), |(index, _)| {
            // Translate a bounded suffix scalar position back into the note's source byte coordinate.
            match_end + index
        });
    let mut snippet = String::with_capacity(context_end.saturating_sub(context_start));
    let mut previous_was_space = false;
    for character in text[context_start..context_end].chars() {
        // The fixed context bounds cap both iteration work and retained preview content.
        if character.is_whitespace() {
            // Normalize any Unicode whitespace to a single display separator.
            if !previous_was_space && !snippet.is_empty() {
                // Avoid leading/duplicate whitespace while preserving separation between visible words.
                snippet.push(' ');
            }
            previous_was_space = true;
        } else if !character.is_control() {
            // Keep printable content literally; other control characters contribute nothing to the UI.
            snippet.push(character);
            previous_was_space = false;
        }
    }
    snippet.trim().to_owned()
}

#[cfg(test)]
mod tests {
    //! External cancellation must remain effective when it happens before worker startup.

    use super::*;

    /// A pre-cancelled request must not start a scan or invalidate a later request generation.
    #[test]
    fn caller_cancellation_before_start_prevents_generation_and_can_be_reused() {
        // A synthetic vault exposes query results without accessing any personal file content.
        let directory = tempfile::tempdir().expect("temporary vault");
        let root = directory.path().join("vault");
        let workspace = MarkdownWorkspace::new(root.clone());
        let access = workspace.open_session().expect("open synthetic session");
        fs::write(root.join("Note.md"), "needle").expect("seed searchable note");
        let generation = access.search_generation.load(AtomicOrdering::Acquire);
        let cancelled = AtomicBool::new(true);

        // Closing before the worker calls into core returns immediately without a generation claim.
        assert!(
            access
                .search_notes_cancellable("needle", 10, &cancelled)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            access.search_generation.load(AtomicOrdering::Acquire),
            generation
        );

        // A fresh active request still uses the same ordinary session and bounded result semantics.
        cancelled.store(false, AtomicOrdering::Release);
        let results = access
            .search_notes_cancellable("needle", 10, &cancelled)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relative_path, "Note.md");
        assert!(access.search_generation.load(AtomicOrdering::Acquire) > generation);
    }
}

#[cfg(test)]
mod bounded_tests;
