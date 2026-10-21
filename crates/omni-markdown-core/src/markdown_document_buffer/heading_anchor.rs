//! Heading links advance through actual Global facts of the same document, without loading its whole
//! text or building an in-memory duplicate-slug index.
//! The request retains weak original-root identity and exclusively owns anonymous scratch, explicitly
//! closing it on success, failure, or cancellation before returning a target.

use super::*;
use crate::markdown_viewport_presentation::{
    parser::MarkdownParserSeed,
    table_window::{MarkdownTableWindowQuota, references::MarkdownTableReferenceQueries},
};
use pulldown_cmark::block_frontier::SourceFrame;
use pulldown_cmark::global_frontier::{
    BlockOriginLimits, GlobalFact, GlobalFrontier, GlobalFrontierError, GlobalFrontierLimits,
    GlobalSourceAuthority, GlobalStep, InlineBlockKind, InlineParagraphContext,
    InlineParagraphStep, InlinePlainLimits, LeafBlockDetails, LeafBlockKind, StructuralSpineLimits,
    VerifiedGlobalLeafBlock,
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::{fs::File, io, os::fd::IntoRawFd, os::unix::fs::FileExt, sync::Weak};
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};

mod projection;
use projection::{HeadingProjection, HeadingSource};
mod references;
use references::HeadingReferences;
mod footnotes;
use footnotes::HeadingFootnotes;

const SMALL: usize = 2048;
const PAGE: usize = 1024;
const FRAGMENT_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// Errors retain structural categories or system errno only; fragments, headings, and paths never enter
/// error messages.
pub enum MarkdownHeadingAnchorError {
    #[error("标题链接的正文来源不可用：{0}")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("标题链接的结构解析未完成：{0:?}")]
    Global(GlobalFrontierError),
    #[error("标题链接的行内来源未完成：{0:?}")]
    Semantic(super::table_window_request::MarkdownTableRequestError),
    #[error("标题链接临时存储不可用（{kind:?}，系统错误 {raw_os_error:?}）")]
    Storage {
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
    },
    #[error("标题链接已经取消")]
    Cancelled,
    #[error("标题链接的原文版本已经失效")]
    SourceSuperseded,
    #[error("标题链接片段不是有效的有限值")]
    InvalidFragment,
    #[error("标题链接的本次工作预算无效")]
    InvalidBudget,
    #[error("这个超长标题的行内语义尚不能完整确认")]
    UnsupportedInline,
    #[error("标题链接坐标或存储长度溢出")]
    ArithmeticOverflow,
    #[error("标题链接失败：{primary}；清理同时失败：{cleanup}")]
    Cleanup {
        primary: Arc<Self>,
        cleanup: Arc<Self>,
    },
}

type Error = MarkdownHeadingAnchorError;

/// Preserve the actual storage category and errno without file paths or heading payload.
fn storage(error: io::Error) -> Error {
    Error::Storage {
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Account actual input, copying, file I/O, and control transitions separately; one parser call is not
/// claimed as per-token fuel accounting.
pub struct MarkdownHeadingAnchorWork {
    pub source_bytes: usize,
    pub copied_bytes: usize,
    pub metadata_bytes: usize,
    pub storage_bytes: usize,
    pub parser_input_bytes: usize,
    pub structural_transitions: usize,
}

#[derive(Clone, Debug)]
struct SourceIdentity {
    root: Option<Weak<MarkdownPieceTreeNode>>,
    // An empty Source has no root; weak identity of its existing derived read window distinguishes
    // empty documents without a new resource or retained cache.
    empty_owner: Option<Weak<Mutex<super::read_cache::Utf16ReadCache>>>,
    generation: u64,
    bytes: usize,
}

impl SourceIdentity {
    /// Capture original weak allocation identity and immutable generation/length facts.
    fn new(document: &MarkdownDocumentBuffer) -> Self {
        // A nonempty piece root already has actual allocation identity; downgrade it without copying
        // the index or retaining Source bytes.
        // For empty Sources, the existing read-cache weak brand distinguishes separate documents with
        // equal length and generation.
        // The document still releases its read cache; this ticket extends neither that cache, mmap, nor
        // history lifetime.
        Self {
            root: document.root.as_ref().map(Arc::downgrade),
            empty_owner: document
                .root
                .is_none()
                .then(|| Arc::downgrade(&document.utf16_read_cache)),
            generation: document.generation(),
            bytes: document.byte_len(),
        }
    }

    /// Empty documents retain version/length checks; nonempty ones require the original allocation, and
    /// undo to the same root cannot revive an old ticket.
    fn require(&self, document: &MarkdownDocumentBuffer) -> Result<(), Error> {
        document.original_backing_health()?;
        // Check actual Source health before its brand; equal text, length, or generation cannot
        // impersonate the original document.
        // Undo advances generation even when restoring the same root, so an old navigation result never
        // regains authority.
        let root_matches = match (&self.root, &document.root) {
            (Some(expected), Some(actual)) => Weak::ptr_eq(expected, &Arc::downgrade(actual)),
            (None, None) => self.empty_owner.as_ref().is_some_and(|owner| {
                Weak::ptr_eq(owner, &Arc::downgrade(&document.utf16_read_cache))
            }),
            _ => false,
        };
        if !root_matches
            || self.generation != document.generation()
            || self.bytes != document.byte_len()
        {
            return Err(Error::SourceSuperseded);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
/// Only the same actual heading Fact may issue this target; fragments or arbitrary offsets cannot
/// construct navigation authority.
pub struct MarkdownHeadingAnchorTarget {
    source: SourceIdentity,
    position: usize,
}

impl MarkdownHeadingAnchorTarget {
    /// The position is canonical UTF-16 at the original heading's physical line start; actual Native
    /// selection still owns display and fold expansion.
    pub fn position_for(&self, document: &MarkdownDocumentBuffer) -> Result<usize, Error> {
        self.source.require(document)?;
        // Convert the original Fact's physical line start and revalidate its UTF-16 boundary through
        // the current tree.
        // Never infer Source position from slug length, visible text, or folded height; display
        // consumes its own actual selection intent.
        document.byte_offset_for_utf16(self.position)?;
        Ok(self.position)
    }
}

#[derive(Debug)]
pub enum MarkdownHeadingAnchorStep {
    Progress {
        work: MarkdownHeadingAnchorWork,
    },
    Found {
        target: MarkdownHeadingAnchorTarget,
        work: MarkdownHeadingAnchorWork,
    },
    NotFound {
        work: MarkdownHeadingAnchorWork,
    },
}

/// Committed file records contain length and final slug only; one unpublished heading stays at the
/// tail, without an in-memory index.
struct Slugs {
    // Before committed are sequential finalized unique records; after it is only the current
    // unpublished candidate, with no second memory index.
    // base_end retains the original base end while candidate_end follows numeric suffix changes,
    // without reconstructing a long slug in RAM.
    // scan/compared describe this fixed-page comparison only, not a viewport, document line number, or
    // visible range.
    file: Option<File>,
    committed: u64,
    base_end: u64,
    candidate_end: u64,
    scan: u64,
    record_len: u64,
    compared: u64,
    equal: bool,
    suffix: u64,
    maximum: u64,
    irrelevant: bool,
    // Visible heading text uses continuous Unicode default lowercasing across inline domains, retaining
    // one pending sigma without caching subsequent marks.
    previous_cased: bool,
    pending_sigma: Option<u64>,
}

impl Slugs {
    /// Initialize finite scratch bookkeeping from the admitted fragment bound without opening a file.
    fn new(maximum: usize) -> Self {
        // The bound comes from validated fragment bytes, not document length; a longer final ID cannot
        // match that fragment.
        // Open no file here. An empty fragment targets document start; a document without headings
        // creates no scratch FD.
        Self {
            file: None,
            committed: 0,
            base_end: 0,
            candidate_end: 0,
            scan: 0,
            record_len: 0,
            compared: 0,
            equal: true,
            suffix: 0,
            maximum: maximum as u64,
            irrelevant: false,
            previous_cased: false,
            pending_sigma: None,
        }
    }

    /// Borrow only this query's already-created scratch descriptor; missing ownership is an internal
    /// error.
    fn file(&self) -> Result<&File, Error> {
        self.file.as_ref().ok_or(Error::ArithmeticOverflow)
    }

    /// Create a private 0600 anonymous temporary only for the first actual heading, never from an
    /// external path.
    fn begin(&mut self) -> Result<(), Error> {
        if self.file.is_none() {
            self.file = Some(tempfile::tempfile().map_err(storage)?);
        }
        // Append a candidate after committed records; a pruned long heading leaves only an unpublished
        // tail that may be overwritten.
        // Reset this heading's comparison/lowercase state without discarding committed unique-ID
        // history.
        self.base_end = self
            .committed
            .checked_add(8)
            .ok_or(Error::ArithmeticOverflow)?;
        self.candidate_end = self.base_end;
        self.scan = 0;
        self.record_len = 0;
        self.compared = 0;
        self.equal = true;
        self.suffix = 0;
        self.irrelevant = false;
        self.previous_cased = false;
        self.pending_sigma = None;
        Ok(())
    }

    /// Append certified visible bytes only while the candidate can still match this fragment.
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        // Accept only visible fragments with completed semantic proof; later markup closure cannot
        // change the meaning of already emitted output.
        // irrelevant terminates this candidate: later fragments allocate no larger buffers and write no
        // useless scratch bytes.
        if self.irrelevant {
            return Ok(());
        }
        if self
            .length()
            .checked_add(bytes.len() as u64)
            .ok_or(Error::ArithmeticOverflow)?
            > self.maximum
        {
            // Uniqueness adds suffixes only; a proven base longer than the fragment cannot affect any
            // shorter ID that might equal it.
            // Complete semantic projection must precede pruning; an unclosed literal Source prefix
            // cannot authorize it.
            self.irrelevant = true;
            return Ok(());
        }
        self.file()?
            .write_all_at(bytes, self.candidate_end)
            .map_err(storage)?;
        self.candidate_end = self
            .candidate_end
            .checked_add(bytes.len() as u64)
            .ok_or(Error::ArithmeticOverflow)?;
        Ok(())
    }

    /// Unicode default lowercase needs language-independent context for final sigma; all visible
    /// fragments share this original file state.
    /// Write equal-width normal sigma first, deciding final sigma only at an actual successor/heading
    /// end; arbitrarily long ignorable suffixes retain one offset.
    fn append_visible(&mut self, text: &str) -> Result<(usize, usize), Error> {
        if self.irrelevant {
            return Ok((0, 0));
        }
        // Each caller supplies at most 260 complete UTF-8 bytes; lowercase expansion still fits this
        // fixed output page.
        // Filter after default lowercase context: removed punctuation may still end or extend the Greek
        // word context.
        let mut output = [0u8; 1024];
        let mut length = 0;
        let mut writes = 0;
        let start = self.candidate_end;
        for scalar in text.chars() {
            let ignored = case_ignorable(scalar);
            let cased = scalar.is_lowercase()
                || scalar.is_uppercase()
                || scalar.general_category() == GeneralCategory::TitlecaseLetter;
            // Consume the pending sigma on every non-ignorable successor, including a cased one.
            // Only a non-cased successor converts it to final sigma; neither path retains a word copy.
            if !ignored
                && let Some(pending) = self.pending_sigma.take()
                && !cased
            {
                if pending >= start {
                    // A sigma from this quantum still occupies its fixed output page. Both UTF-8
                    // forms use two bytes, so replacing the final byte cannot shift later content.
                    output[(pending - start) as usize + 1] = 0x82;
                } else {
                    // Earlier output is in this query's exclusive candidate file; patch the same
                    // certified offset without allocating another output namespace or loading its text.
                    self.file()?
                        .write_all_at("ς".as_bytes(), pending)
                        .map_err(storage)?;
                    writes += 2;
                }
            }
            // The current capital sigma may resolve its predecessor and become the new pending scalar;
            // retain only one offset.
            // Existing lowercase sigma forms remain unchanged; lowercasing must not become Unicode case
            // folding.
            if scalar == 'Σ' && self.previous_cased {
                self.pending_sigma = Some(start + length as u64);
            }
            if !ignored {
                self.previous_cased = cased;
            }
            for lower in scalar.to_lowercase() {
                let Some(lower) = slug_scalar(lower) else {
                    continue;
                };
                let mut encoded = [0; 4];
                let bytes = lower.encode_utf8(&mut encoded).as_bytes();
                let end = length + bytes.len();
                output
                    .get_mut(length..end)
                    .ok_or(Error::ArithmeticOverflow)?
                    .copy_from_slice(bytes);
                length = end;
            }
        }
        self.append(&output[..length])?;
        // Crossing the fragment bound discards the candidate without writing this output page; any
        // earlier sigma patch still counts its actual two bytes.
        let appended = if self.irrelevant { 0 } else { length };
        Ok((writes + appended, length))
    }

    /// Only actual heading end finalizes sigma; semantic-domain, viewport-page, or file-chunk ends do
    /// not.
    fn finish_visible(&mut self) -> Result<usize, Error> {
        let Some(pending) = self.pending_sigma.take() else {
            return Ok(0);
        };
        if self.irrelevant {
            return Ok(0);
        }
        self.file()?
            .write_all_at("ς".as_bytes(), pending)
            .map_err(storage)?;
        Ok(2)
    }

    /// Measure candidate bytes from original scratch offsets, excluding its record header.
    fn length(&self) -> u64 {
        // The eight-byte record header never enters ID comparison; derive length from file offsets
        // without scanning or copying candidate text.
        self.candidate_end - self.committed - 8
    }

    /// Final slugs are unique; compare one fixed page per turn and charge actual repeated I/O rather
    /// than building a HashMap.
    fn collision_step(&mut self) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        if self.scan == self.committed {
            return Ok((work, true));
        }
        if self.compared == 0 {
            // Read the committed record length first; unequal lengths cannot match, so skip directly to
            // the next original record.
            // Read both fixed pages only for equal lengths; the uncommitted headerless tail must not be
            // scanned as history.
            let mut bytes = [0; 8];
            self.file()?
                .read_exact_at(&mut bytes, self.scan)
                .map_err(storage)?;
            self.record_len = u64::from_le_bytes(bytes);
            work.storage_bytes += 8;
            if self.record_len != self.length() {
                self.scan = self
                    .scan
                    .checked_add(8)
                    .and_then(|v| v.checked_add(self.record_len))
                    .ok_or(Error::ArithmeticOverflow)?;
                return Ok((work, false));
            }
            self.equal = true;
        }
        // Compare at most 1 KiB per quantum without loading a complete candidate for a long-ID
        // collision.
        // Even after an early mismatch, advance to the actual record boundary so subsequent scans
        // cannot start inside a record.
        let count = (self.record_len - self.compared).min(PAGE as u64) as usize;
        let mut previous = [0; PAGE];
        let mut candidate = [0; PAGE];
        self.file()?
            .read_exact_at(&mut previous[..count], self.scan + 8 + self.compared)
            .map_err(storage)?;
        self.file()?
            .read_exact_at(&mut candidate[..count], self.committed + 8 + self.compared)
            .map_err(storage)?;
        self.equal &= previous[..count] == candidate[..count];
        self.compared += count as u64;
        work.storage_bytes += count * 2;
        work.structural_transitions += count;
        if self.compared != self.record_len {
            return Ok((work, false));
        }
        if self.equal {
            // Write the base once and replace only its candidate suffix, without returning a whole long
            // slug to RAM.
            if self.suffix == 0 {
                self.base_end = self.candidate_end;
            }
            // Resolve collisions in appearance order with the smallest available numeric suffix; an
            // explicitly written a-1 occupies that same ID space.
            // Recompare each numeric suffix against all existing final IDs; a per-base counter alone
            // cannot prove uniqueness.
            self.suffix = self
                .suffix
                .checked_add(1)
                .ok_or(Error::ArithmeticOverflow)?;
            self.candidate_end = self.base_end;
            self.file()?.set_len(self.base_end).map_err(storage)?;
            let suffix = format!("-{}", self.suffix);
            self.append(suffix.as_bytes())?;
            work.storage_bytes += suffix.len();
            self.scan = 0;
        } else {
            self.scan = self
                .scan
                .checked_add(8)
                .and_then(|v| v.checked_add(self.record_len))
                .ok_or(Error::ArithmeticOverflow)?;
        }
        self.compared = 0;
        Ok((work, false))
    }

    /// Publish one complete unique record header only after matching has finished.
    fn commit(&mut self) -> Result<(), Error> {
        // Write a record header only after actual uniqueness and fragment comparison complete; failed
        // candidates cannot reserve IDs for later headings.
        // A pwrite failure ends the original request without replaying an uncertain commit or advancing
        // committed early.
        self.file()?
            .write_all_at(&self.length().to_le_bytes(), self.committed)
            .map_err(storage)?;
        self.committed = self.candidate_end;
        Ok(())
    }

    /// All terminal paths close explicitly; uncertain close outcomes never retry, while Drop only
    /// compensates an abandoned request.
    fn close(&mut self) -> Result<(), Error> {
        if let Some(file) = self.file.take() {
            let fd = file.into_raw_fd();
            // SAFETY: This anonymous file just transferred sole FD ownership; close once, then never
            // read, write, or retry that descriptor.
            if unsafe { libc::close(fd) } != 0 {
                return Err(storage(io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}

// All phases belong to one request; advance performs a bounded current step without driving the UI
// event loop.
// Short headings have an explicit 2-KiB parser bound; long headings stream their actual projection.
// Both share one uniqueness file.
// References/footnotes answer only actual dependencies of this heading; Unknown must never become
// Missing.
enum Phase {
    Scan,
    Copy { range: Range<usize>, next: usize },
    Parse,
    CollectReferences,
    References,
    Footnotes,
    Project,
    Slug { text: String, next: usize },
    Collision,
    Compare { next: usize, equal: bool },
    Done,
}

/// The request retains no text/history/mapping owner; giant headings use their original Fact, fixed
/// pages, and anonymous scratch without a whole-text string.
pub struct MarkdownHeadingAnchorRequest {
    source: SourceIdentity,
    // The sole external contiguous value is the fragment, limited to 16 KiB on entry and released
    // immediately on completion.
    fragment: String,
    global: Option<GlobalFrontier>,
    phase: Phase,
    // small contains one complete heading up to 2 KiB; its physical prefix, semantic seed, and parser
    // must derive from the same Global fact.
    // This is no paragraph fallback cache: long headings use disk projection from admission and cannot
    // fall back here by growing capacity.
    small: Vec<u8>,
    small_leaf: Option<VerifiedGlobalLeafBlock>,
    paragraph: Option<InlineParagraphContext>,
    references: Option<Box<HeadingReferences>>,
    footnotes: Option<Box<HeadingFootnotes>>,
    projection: Option<Box<HeadingProjection>>,
    position: usize,
    slugs: Slugs,
    failure: Option<Error>,
    found: Option<MarkdownHeadingAnchorTarget>,
}

impl std::fmt::Debug for MarkdownHeadingAnchorRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MarkdownHeadingAnchorRequest")
            .field("source", &self.source)
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

impl MarkdownDocumentBuffer {
    /// The URL boundary decoded the fragment once; do not decode again or normalize heading text as
    /// though it were that fragment.
    pub fn heading_anchor_request(
        &self,
        fragment: &str,
    ) -> Result<MarkdownHeadingAnchorRequest, Error> {
        self.original_backing_health()?;
        if fragment.len() > FRAGMENT_LIMIT || fragment.contains(['\0', '\r', '\n']) {
            return Err(Error::InvalidFragment);
        }
        // Derive cumulative fuel from the same actual Source length; repeated advances or new viewports
        // cannot reset it.
        // Structural pages have separate finite inventory; retain no whole-document AST and grant no
        // Fact from bare consumer coordinates.
        let bytes = self.byte_len();
        let fuel = bytes
            .checked_mul(4096)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(Error::ArithmeticOverflow)?;
        let global = GlobalFrontier::new(
            self.generation(),
            bytes,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: fuel,
                },
                max_byte_work: fuel,
                max_transitions: fuel,
            },
        )
        .map_err(Error::Global)?;
        Ok(MarkdownHeadingAnchorRequest {
            source: SourceIdentity::new(self),
            fragment: fragment.to_owned(),
            global: Some(global),
            phase: Phase::Scan,
            small: Vec::new(),
            small_leaf: None,
            paragraph: None,
            references: None,
            footnotes: None,
            projection: None,
            position: 0,
            slugs: Slugs::new(fragment.len()),
            failure: None,
            found: None,
        })
    }
}

impl MarkdownHeadingAnchorRequest {
    /// Revoke authority before returning anonymous FDs; subsequent advances remain cancelled and cannot
    /// restart old heading scans.
    pub fn cancel(&mut self) -> Result<(), Error> {
        // Fix cancellation before releasing every owned resource; cleanup failure cannot turn
        // cancellation into resumable scanning.
        // Retain an existing primary failure and return any additional actual close failure at the same
        // boundary.
        self.failure.get_or_insert(Error::Cancelled);
        if let Err(cleanup) = self.release() {
            let primary = self.failure.take().unwrap_or(Error::Cancelled);
            let error = Error::Cleanup {
                primary: Arc::new(primary),
                cleanup: Arc::new(cleanup),
            };
            self.failure = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }

    /// Revoke parsing and attempt all independently owned scratch closes, retaining both failures.
    fn release(&mut self) -> Result<(), Error> {
        // Drop Source semantic continuations and finite text first, revoking further parsing before
        // attempting each fallible close.
        // Projection and slug files own independent closes; failure of either cannot skip closing the
        // other actual FD.
        // Done alone is not success: found or a fixed failure interprets the terminal state, and
        // completed cleanup cannot invent Found.
        self.global = None;
        self.small = Vec::new();
        self.small_leaf = None;
        self.paragraph = None;
        self.references = None;
        self.footnotes = None;
        self.phase = Phase::Done;
        self.fragment = String::new();
        let projection = self
            .projection
            .take()
            .map_or(Ok(()), |mut projection| projection.close());
        let slugs = self.slugs.close();
        match (projection, slugs) {
            (Err(primary), Err(cleanup)) => Err(Error::Cleanup {
                primary: Arc::new(primary),
                cleanup: Arc::new(cleanup),
            }),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }

    /// Preserve the actual first cause together with close failure; later calls repeat that result,
    /// never turning cleanup success into lookup success.
    fn fail(&mut self, error: Error) -> Error {
        let primary = self.failure.take().unwrap_or(error);
        let error = match self.release() {
            Ok(()) => primary,
            Err(cleanup) => Error::Cleanup {
                primary: Arc::new(primary),
                cleanup: Arc::new(cleanup),
            },
        };
        self.failure = Some(error.clone());
        error
    }

    /// Check original mapping health and root at each entry, advancing one actual phase without driving
    /// the event loop or claiming UI display completion.
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownHeadingAnchorStep, Error> {
        if let Err(error) = self.source.require(document) {
            return Err(self.fail(error));
        }
        if let Some(error) = self.failure.as_ref() {
            return Err(error.clone());
        }
        // An invalid budget is a real failure boundary; immediately return existing scratch instead of
        // waiting for Drop.
        // Native uses its fixed valid quantum; public callers also cannot revive failed work by
        // changing budget.
        if !(4096..=64 * 1024).contains(&budget) {
            return Err(self.fail(Error::InvalidBudget));
        }
        match self.advance_inner(document, budget) {
            Ok(step) => Ok(step),
            Err(error) => Err(self.fail(error)),
        }
    }

    /// Advance one actual phase under the caller's existing quantum and original Source identity.
    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownHeadingAnchorStep, Error> {
        use MarkdownHeadingAnchorStep::*;
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        // Completed requests may return the same weak result again, after the entry's original Source
        // health/generation checks.
        // Never access closed files or reallocate/rescan headings for the same completed navigation.
        if matches!(self.phase, Phase::Done) {
            return Ok(match &self.found {
                Some(target) => Found {
                    target: target.clone(),
                    work,
                },
                None => NotFound { work },
            });
        }
        if self.fragment.is_empty() {
            return self.finish(document, 0, work); // An empty URL fragment means document start, still under the same Source check.
        }
        match &mut self.phase {
            Phase::Scan => {
                // Accept heading Facts only from the original global structural frontier; a literal #
                // cannot turn fenced code, HTML, or a paragraph into a heading.
                // Finish the borrowed page before propagating structural errors, preserving actual
                // Source reclamation failure in the single error chain.
                let global = self.global.as_mut().ok_or(Error::ArithmeticOverflow)?;
                let frame = document.table_source_frame(global.next_byte(), PAGE)?;
                let borrowed = frame.bytes.len();
                let step = global
                    .advance(
                        SourceFrame {
                            source_revision: frame.generation,
                            byte_start: frame.byte_start,
                            bytes: &frame.bytes,
                            real_eof: frame.real_eof,
                        },
                        GlobalSourceAuthority {
                            current_revision: document.generation(),
                            cancelled: false,
                        },
                        budget - 128,
                    )
                    .map_err(Error::Global);
                frame.finish()?;
                let step = step?;
                let (measured, fact) = match step {
                    GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => {
                        (work, None)
                    }
                    GlobalStep::Fact { fact, work } => (work, Some(fact)),
                    GlobalStep::Complete { work: measured } => {
                        work.source_bytes = borrowed;
                        work.structural_transitions += measured.transitions;
                        self.release()?;
                        return Ok(NotFound { work });
                    }
                };
                work.source_bytes = borrowed;
                work.structural_transitions += measured.transitions;
                if let Some(fact) = fact {
                    self.accept_heading(document, fact)?;
                }
            }
            Phase::Copy { range, next } => {
                // The complete short-heading Fact and 2-KiB limit certify range; copy only this small
                // page without reinterpreting line/container syntax.
                // A piece boundary may split UTF-8; pass complete Source bytes to the original parser
                // only after acquisition, never treating a piece end as syntax EOF.

                if *next == range.end {
                    self.phase = Phase::Parse;
                    return Ok(Progress { work });
                }
                let frame = document.table_source_frame(*next, (range.end - *next).min(PAGE))?;
                self.small.extend_from_slice(&frame.bytes);
                let count = frame.bytes.len();
                *next += count;
                frame.finish()?;
                work.source_bytes = count;
                work.copied_bytes = count;
            }
            Phase::Parse => {
                let source =
                    std::str::from_utf8(&self.small).map_err(|_| Error::UnsupportedInline)?;
                if let Some(context) = self
                    .paragraph
                    .as_mut()
                    .filter(|context| !context.is_complete())
                {
                    // A short Setext heading inside deep containers must complete its captured physical
                    // prefixes before parsing; its fragment cannot be reclassified as Root.
                    let step = context
                        .advance(
                            source,
                            GlobalSourceAuthority {
                                current_revision: document.generation(),
                                cancelled: false,
                            },
                            budget - 128,
                        )
                        .map_err(Error::Global)?;
                    let measured = match step {
                        InlineParagraphStep::Progress { work }
                        | InlineParagraphStep::Complete { work } => work,
                        InlineParagraphStep::Unsupported { .. } => {
                            return Err(Error::UnsupportedInline);
                        }
                    };
                    work.source_bytes = measured.byte_reads;
                    work.structural_transitions += measured.transitions;
                    return Ok(Progress { work });
                }
                // Parse only this heading's actual seed. Start/End delimit heading events; URLs,
                // titles, and raw HTML never enter visible heading text.
                // The parser already handled code trimming/line endings and text entities/escapes; do
                // not implement another syntax pass here.
                let mut heading = false;
                let mut completed = false;
                let mut text = String::new();
                // Reference roles differ: ordinary links need destination values, while footnotes need
                // actual definition existence.
                // Both callbacks register bounded dependencies only, without full-Source I/O or
                // blocking the main thread through EOF during parser iteration.
                let unresolved = std::cell::Cell::new(false);
                let references = self.references.as_ref();
                let fuel = self
                    .source
                    .bytes
                    .checked_mul(4096)
                    .and_then(|n| n.checked_add(1024 * 1024))
                    .ok_or(Error::ArithmeticOverflow)?;
                let footnotes = self.footnotes.get_or_insert_with(|| {
                    Box::new(HeadingFootnotes::new(MarkdownTableWindowQuota::new(
                        1024, fuel,
                    )))
                });
                let footnotes = std::cell::RefCell::new(footnotes.as_mut());
                let footnote_lookup = |label: &str| footnotes.borrow_mut().lookup(label);
                let mut callback = |link: pulldown_cmark::BrokenLink<'_>| {
                    if let Some(references) = references {
                        // Borrow only values certified by this heading's actual Source scan;
                        // distinguish Missing from unfinished queries.
                        return references.resolve(&link.reference);
                    }
                    unresolved.set(true);
                    None
                };
                let parser = if let Some(leaf) = self.small_leaf.as_ref() {
                    Parser::new_with_atx_leaf_seed(source, leaf, Some(&mut callback))
                        .map_err(Error::Global)?
                } else if let Some(context) = self.paragraph.as_ref() {
                    Parser::new_with_inline_block_seed(source, context, Some(&mut callback))
                        .map_err(Error::Global)?
                } else {
                    Parser::new_with_broken_link_callback(source,
                        crate::markdown_viewport_presentation::parser::markdown_presentation_options(false),
                        Some(&mut callback))
                };
                // Use the original parser only for complete closed headings up to 2 KiB, without
                // per-fragment parsers or an enlarged input limit.
                // Same-Source footnote answers require actual global Facts; Unknown suspends work, and
                // absent definitions in one fragment do not mean Missing.
                for event in parser.with_footnote_lookup(&footnote_lookup) {
                    match event {
                        Event::Start(Tag::Heading { .. }) if !heading && !completed => {
                            heading = true
                        }
                        Event::End(TagEnd::Heading(_)) if heading => {
                            heading = false;
                            completed = true;
                        }
                        Event::Text(value) | Event::Code(value) if heading => text.push_str(&value),
                        Event::SoftBreak | Event::HardBreak if heading => text.push(' '),
                        _ => {}
                    }
                }
                if let Some(error) = footnotes.borrow_mut().take_failure() {
                    return Err(error);
                }
                let footnotes_pending = !footnotes.borrow().is_ready();
                // Parser callbacks and their temporary RefCell borrows have ended. Only this scalar
                // pending result crosses into the next phase; no footnote body is retained here.
                if !completed {
                    return Err(Error::UnsupportedInline);
                }
                work.parser_input_bytes = self.small.len();
                work.copied_bytes = text.len();
                // Discard event text whose dependencies remain unknown; retry the same bounded heading
                // only after actual answers arrive, without publishing a partial slug.
                // Footnotes affect original LinkStack closure, so their Found/Missing answer precedes
                // final ordinary-link semantic publication.
                if footnotes_pending {
                    self.phase = Phase::Footnotes;
                    return Ok(Progress { work });
                }
                if unresolved.get() {
                    self.phase = Phase::CollectReferences;
                    return Ok(Progress { work }); // This quantum already ran one parser; dependency collection belongs to the next actual phase.
                }
                self.small = Vec::new();
                self.small_leaf = None;
                self.paragraph = None;
                self.references = None;
                self.footnotes = None;
                self.phase = Phase::Slug { text, next: 0 };
            }
            Phase::CollectReferences => {
                // The previous quantum ran the parser; advance dependency collection separately rather
                // than hiding two whole-block parses in one receipt.
                // Query inventory covers this short heading only; later definitions still come from the
                // original Source in actual order.

                let source =
                    std::str::from_utf8(&self.small).map_err(|_| Error::UnsupportedInline)?;
                let seed = self
                    .small_leaf
                    .as_ref()
                    .map(MarkdownParserSeed::Atx)
                    .or_else(|| self.paragraph.as_ref().map(MarkdownParserSeed::Scoped));
                let options = self.small_leaf.as_ref().map_or_else(
                    || crate::markdown_viewport_presentation::parser::markdown_presentation_options(false),
                    VerifiedGlobalLeafBlock::options);
                let queries = MarkdownTableReferenceQueries::from_bounded_inline(
                    source,
                    options,
                    MarkdownTableWindowQuota::new(1024, 512 * 1024),
                    seed,
                )
                .map_err(|error| {
                    Error::Semantic(
                        super::table_window_request::MarkdownTableRequestError::Window(error),
                    )
                })?;
                self.references = Some(Box::new(HeadingReferences::new(document, queries)?));
                work.parser_input_bytes = self.small.len();
                self.phase = Phase::References;
            }
            Phase::References => {
                let references = self.references.as_mut().ok_or(Error::ArithmeticOverflow)?;
                let (measured, complete) = references.advance(document, budget)?;
                work = measured;
                if complete {
                    self.phase = Phase::Parse;
                }
            }
            Phase::Footnotes => {
                let (measured, complete) = self
                    .footnotes
                    .as_mut()
                    .ok_or(Error::UnsupportedInline)?
                    .advance(document, budget)?;
                work = measured;
                if complete {
                    self.phase = Phase::Parse;
                }
            }
            Phase::Project => {
                // Long headings complete shared inline facts before projecting Source order to the slug
                // file; later delimiters cannot rewrite emitted semantics.
                // Only actual heading end finalizes sigma, never a file-page end, viewport end, or
                // temporary NeedInput.

                let projection = self.projection.as_mut().ok_or(Error::UnsupportedInline)?;
                let (measured, complete) = projection.advance(document, &mut self.slugs, budget)?;
                work = measured;
                if complete {
                    work.storage_bytes += self.slugs.finish_visible()?;
                    self.projection = None;
                    self.phase = Phase::Collision;
                }
            }
            Phase::Slug { text, next } => {
                // This text comes strictly from bounded parser output; slice complete scalars while
                // Slugs retains lowercase context across chunks.
                // Final IDs and long projections share filtering/collision rules rather than separate
                // anchor algorithms for each rendering path.

                let mut end = (*next + 256).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let (writes, copied) = self.slugs.append_visible(&text[*next..end])?;
                work.copied_bytes = copied;
                work.storage_bytes = writes;
                work.structural_transitions += end - *next;
                *next = end;
                if end == text.len() {
                    work.storage_bytes += self.slugs.finish_visible()?;
                    self.phase = Phase::Collision;
                }
            }
            Phase::Collision => {
                if self.slugs.irrelevant {
                    self.phase = Phase::Scan;
                    return Ok(Progress { work });
                }
                let (measured, done) = self.slugs.collision_step()?;
                work = measured;
                if done {
                    self.phase = Phase::Compare {
                        next: 0,
                        equal: self.slugs.length() == self.fragment.len() as u64,
                    };
                }
            }
            Phase::Compare { next, equal } => {
                // Compare the sole current candidate against the bounded external fragment; equality
                // alone permits publishing the original Fact's UTF-16 endpoint.
                // A nonmatch commits its final ID for later uniqueness and resumes the original Global
                // frontier, without a synthetic parser starting at this position.

                if *equal && *next < self.fragment.len() {
                    let count = (self.fragment.len() - *next).min(PAGE);
                    let mut bytes = [0; PAGE];
                    self.slugs
                        .file()?
                        .read_exact_at(&mut bytes[..count], self.slugs.committed + 8 + *next as u64)
                        .map_err(storage)?;
                    *equal &= bytes[..count] == self.fragment.as_bytes()[*next..*next + count];
                    *next += count;
                    work.storage_bytes += count;
                    return Ok(Progress { work });
                }
                if *equal {
                    return self.finish(document, self.position, work);
                }
                self.slugs.commit()?;
                work.storage_bytes += 8;
                self.phase = Phase::Scan;
            }
            Phase::Done => unreachable!("已完成状态在入口处理"),
        }
        self.source.require(document)?;
        Ok(Progress { work })
    }

    /// Accept a genuine Global heading Fact and select its bounded or streamed preparation path.
    fn accept_heading(
        &mut self,
        document: &MarkdownDocumentBuffer,
        fact: GlobalFact,
    ) -> Result<(), Error> {
        // The Fact determines heading kind and physical first line; length chooses bounded parsing or
        // actual streaming without changing heading membership.
        // An ATX first line may include ancestor prefixes; Setext preserves the same paragraph capture
        // and actual body end certified by its underline.
        let (range, physical, long) = match fact {
            GlobalFact::LeafBlock(leaf) if matches!(leaf.kind(), LeafBlockKind::AtxHeading(_)) => {
                let range = leaf.physical_start()..leaf.source_range().end;
                let physical = leaf.physical_start();
                if range.len() > SMALL {
                    (range, physical, Some(HeadingSource::Atx(leaf)))
                } else {
                    self.small_leaf = Some(leaf);
                    (range, physical, None)
                }
            }
            GlobalFact::InlineBlock(block)
                if matches!(block.kind(), InlineBlockKind::SetextHeading(_)) =>
            {
                let range = block.physical_range();
                let physical = range.start;
                if range.len() > SMALL {
                    (range, physical, Some(HeadingSource::Setext(block)))
                } else {
                    if block.scope_depth() > 1 {
                        let fuel = self
                            .source
                            .bytes
                            .checked_mul(4096)
                            .and_then(|n| n.checked_add(1024 * 1024))
                            .ok_or(Error::ArithmeticOverflow)?;
                        let (origins, seed) = block
                            .into_origins_and_semantic_seed(BlockOriginLimits {
                                max_byte_work: fuel,
                                max_transitions: fuel,
                            })
                            .map_err(Error::Global)?;
                        drop(origins); // Transfer only this Fact's context, retaining neither another line directory nor a strong Source graph.
                        self.paragraph = seed
                            .bounded_paragraph_context(InlinePlainLimits {
                                max_byte_work: fuel,
                                max_transitions: fuel,
                            })
                            .map_err(Error::Global)?;
                        if self.paragraph.is_none() {
                            return Err(Error::UnsupportedInline);
                        }
                    }
                    (range, physical, None)
                }
            }
            _ => return Ok(()), // Actual code, HTML, reference definitions, and ordinary paragraphs do not become heading candidates.
        };
        // Convert coordinates through the original tree index, not visible-heading scans; UTF-8
        // emoji/combining characters are not single UTF-16 units.
        // Record a Source endpoint only; actual owning layers still verify final Source health,
        // selection, and visible CPU position.
        self.position = document.utf16_offset_for_byte(physical)?;
        self.slugs.begin()?;
        self.phase = if let Some(source) = long {
            self.projection = Some(Box::new(HeadingProjection::new(source)?));
            Phase::Project
        } else {
            self.small = Vec::with_capacity(range.len());
            Phase::Copy {
                next: range.start,
                range,
            }
        };
        Ok(())
    }

    /// Revalidate Source, close owned scratch, then publish a weak canonical endpoint.
    fn finish(
        &mut self,
        document: &MarkdownDocumentBuffer,
        position: usize,
        work: MarkdownHeadingAnchorWork,
    ) -> Result<MarkdownHeadingAnchorStep, Error> {
        // Recheck original Source immediately before Found; close every owned temporary successfully
        // before handing the endpoint to Native.
        // The target keeps weak branding and canonical coordinates only; navigation results retain no
        // document, history, mmap, or font owner.
        self.source.require(document)?;
        let target = MarkdownHeadingAnchorTarget {
            source: self.source.clone(),
            position,
        };
        self.release()?;
        self.found = Some(target.clone());
        Ok(MarkdownHeadingAnchorStep::Found { target, work })
    }
}

/// Section-link whitespace/punctuation filtering consumes one actual lowercase scalar, without
/// locale-specific casing or NFKC.
fn slug_scalar(scalar: char) -> Option<char> {
    if scalar == ' ' {
        return Some('-');
    }
    (matches!(scalar, '-' | '_')
        || matches!(
            scalar.general_category_group(),
            GeneralCategoryGroup::Letter
                | GeneralCategoryGroup::Mark
                | GeneralCategoryGroup::Number
        ))
    .then_some(scalar)
}

/// Unicode 17 Case_Ignorable follows D136 general-category sets plus three actual WordBreak classes,
/// matching unicode-properties 0.1.4.
/// The 17 punctuation scalars belong to MidLetter/MidNumLet/Single_Quote in that version; neither all
/// punctuation nor all marks are ignorable.
/// Authority: https://www.unicode.org/Public/17.0.0/ucd/DerivedCoreProperties.txt and
/// auxiliary/WordBreakProperty.txt.
fn case_ignorable(scalar: char) -> bool {
    matches!(
        scalar.general_category(),
        GeneralCategory::NonspacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::Format
            | GeneralCategory::ModifierLetter
            | GeneralCategory::ModifierSymbol
    ) || matches!(
        scalar,
        '\u{0027}'
            | '\u{002E}'
            | '\u{003A}'
            | '\u{00B7}'
            | '\u{0387}'
            | '\u{055F}'
            | '\u{05F4}'
            | '\u{2018}'
            | '\u{2019}'
            | '\u{2024}'
            | '\u{2027}'
            | '\u{FE13}'
            | '\u{FE52}'
            | '\u{FE55}'
            | '\u{FF07}'
            | '\u{FF0E}'
            | '\u{FF1A}'
    )
}

impl Drop for MarkdownHeadingAnchorRequest {
    fn drop(&mut self) {
        // An abandoned request has no error-return boundary; File RAII still returns its sole FD, while
        // ordinary terminal paths close explicitly in release.
        drop(self.slugs.file.take());
    }
}

#[cfg(test)]
mod tests;
