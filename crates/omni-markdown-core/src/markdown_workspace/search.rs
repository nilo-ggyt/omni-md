//! Cancellable session-owned vault search with bounded ranking and streaming matching.

use super::*;
use crate::immutable_snapshot::SnapshotDirectory;

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
        let pattern = SearchPattern::new(query.as_bytes());
        let mut ranked = Vec::<RankedSearchResult>::with_capacity(result_limit);
        // One query may reuse an empty private scratch directory, but no snapshot or namespace survives the call.
        let mut snapshot_directory = None;
        let walk_result = walk_markdown_workspace(&self.canonical_root, |entry| {
            // Check directories too so cancellation does not walk a tree with no note files.
            if !is_current() {
                // Returning Stop releases the walk's active directory handles promptly.
                return Ok(WalkControl::Stop);
            }
            let MarkdownFilesystemEntry::Note(path) = entry else {
                // Directory metadata is navigation input, not searchable Markdown text.
                return Ok(WalkControl::Continue);
            };
            // A title match ranks above a body match and avoids mapping that note's content.
            let title = markdown_title(path)?;
            let title_match = pattern.find(title.as_bytes(), is_current)?;
            let (score, snippet) = if title_match.is_some() {
                // Title results need no retained body snippet or file mapping.
                (2_u8, None)
            } else {
                // Map one candidate transiently; its decoded text never escapes this callback.
                if snapshot_directory.is_none() {
                    // Acquire scratch only for the first body scan; title-only results perform no snapshot I/O.
                    snapshot_directory = Some(SnapshotDirectory::new().map_err(|_| {
                        // An unavailable private namespace cannot safely provide immutable source bytes.
                        MarkdownWorkspaceError::StorageUnavailable
                    })?);
                }
                let body_match = inspect_validated_markdown_text_cancellable(
                    path,
                    MAX_SEARCH_FILE_BYTES,
                    is_current,
                    |text| {
                        // Search borrowed mapped bytes with the same cancellation predicate before deriving a bounded snippet.
                        pattern
                            .find(text.as_bytes(), is_current)
                            .map(|match_start| {
                                // Transform a found body offset into optional bounded context; no document body escapes the inspection callback.
                                match_start.map(|match_start| {
                                    // Retain only a bounded context around a complete literal match.
                                    bounded_search_snippet(text, match_start, pattern.byte_len())
                                })
                            })
                    },
                    snapshot_directory.as_mut(),
                )?
                .value?;
                let Some(snippet) = body_match else {
                    // An unmatched note contributes no result and releases its transient mapping.
                    return Ok(WalkControl::Continue);
                };
                (1_u8, Some(snippet))
            };
            // Keep at most the requested best results, with deterministic path tie-breaking.
            ranked.push(RankedSearchResult {
                score,
                result: MarkdownSearchResult {
                    relative_path: relative_markdown_path(&self.canonical_root, path)?,
                    title,
                    snippet,
                },
            });
            ranked.sort_unstable_by(compare_ranked_search_results);
            ranked.truncate(result_limit);
            Ok(WalkControl::Continue)
        });

        if !is_current() {
            // A closing/superseded owner must receive no stale results or expected cancellation error.
            return Ok(Vec::new());
        }
        // Genuine I/O failures still reach the current foreground owner after cancellation is ruled out.
        walk_result?;
        Ok(ranked
            .into_iter()
            .map(|item| {
                // Ranking scores are private; transfer only bounded user-facing search metadata.
                item.result
            })
            .collect())
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

    /// Supersede scans after a mutation or explicit foreground cancellation.
    pub(super) fn cancel_active_search(&self) {
        // No background work is created; already running callbacks observe this generation change.
        self.search_generation.fetch_add(1, AtomicOrdering::AcqRel);
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
        let workspace = MarkdownWorkspace::new(root.clone(), directory.path().join("data"));
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
