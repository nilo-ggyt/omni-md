//! Emphasis matching for one actual root block. Records append to the shared anonymous file;
//! retained RAM consists of its fixed 2-KiB tail page and the current matching state.

use super::*;
use crate::markdown_document_buffer::table_source::inline_stack::{
    Entry, RecordStep, Scope, StackFile,
};
use pulldown_cmark::global_frontier::{VerifiedInlineEmphasisRun, VerifiedInlineLink};
use pulldown_cmark::inline_emphasis::{self, EmphasisBounds, EmphasisStyle};
const RECORD: usize = 64;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
struct Run {
    start: usize,
    count: usize,
    run_length: usize,
    delimiter: u8,
    can_open: bool,
    both: bool,
}

enum Phase {
    Idle,
    Search,
    Consume,
    Remainder,
    Finish,
}

pub(in crate::markdown_document_buffer) struct EmphasisJob {
    source: Range<usize>,
    link: Option<Scope>,
    bounds: EmphasisBounds,
    head: usize,
    depth: usize,
    search: usize,
    lower: usize,
    run: Option<Run>,
    matched: Option<Entry>,
    match_count: usize,
    remaining: usize,
    open_end: usize,
    close_start: usize,
    options: pulldown_cmark::Options,
    phase: Phase,
}

impl EmphasisJob {
    /// Initialize fixed state for the original ordered delimiter stream. The anonymous storage page
    /// remains lazy until the first opener is actually pushed.
    pub(in crate::markdown_document_buffer) fn new(
        options: pulldown_cmark::Options,
        source: Range<usize>,
    ) -> Self {
        Self {
            source,
            link: None,
            bounds: EmphasisBounds::default(),
            head: 0,
            depth: 0,
            search: 0,
            lower: 0,
            run: None,
            matched: None,
            match_count: 0,
            remaining: 0,
            open_end: 0,
            close_start: 0,
            options,
            phase: Phase::Idle,
        }
    }

    /// Consume final-order runs from the same root cursor; HTML and code contents never enter this
    /// matching stack as emphasis delimiters.
    pub(in crate::markdown_document_buffer) fn start(
        &mut self,
        proof: VerifiedInlineEmphasisRun,
    ) -> Result<(), MarkdownTableRequestError> {
        let scope = self.link.as_ref().map_or(&self.source, |link| &link.label);
        let range = proof.source_range();
        if self.busy()
            || proof.scope_range() != *scope
            || range.start < scope.start
            || range.end > scope.end
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        } // The cursor proved this root/link-label scope; matching cannot cross its original boundary.
        let both = proof.can_open() && proof.can_close();
        self.run = Some(Run {
            start: proof.source_range().start,
            count: proof.source_range().len(),
            run_length: proof.source_range().len(),
            delimiter: proof.delimiter(),
            can_open: proof.can_open(),
            both,
        });
        self.phase = if proof.can_close() {
            self.begin_search();
            Phase::Search
        } else {
            Phase::Finish
        };
        Ok(())
    }

    /// Before a verified link's label arrives, suspend the outer completed-run matching head and
    /// start independent matching in the same file. No second descriptor/body owner is created;
    /// unmatched label openers are discarded at LinkEnd rather than leaking into the outer scope.
    pub(in crate::markdown_document_buffer) fn enter_link(
        &mut self,
        proof: &VerifiedInlineLink,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
        file: &mut StackFile,
    ) -> Result<(SemanticWork, bool), MarkdownTableRequestError> {
        let source = proof.source_range();
        let parent = self
            .link
            .as_ref()
            .map_or(&self.source, |scope| &scope.label);
        if self.busy() || source.start < parent.start || source.end > parent.end {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        // Nested image/link scopes preserve the complete outer head and nine failure bounds in the
        // file, retaining only the top scope in RAM. An unfinished page flush leaves the current
        // scope unchanged so the next quantum resumes this proof instead of admitting another Begin.
        let previous = if let Some(outer) = self.link.as_ref() {
            match file.append_scope(outer, quota, budget)? {
                RecordStep::Progress { work } => return Ok((work, false)),
                RecordStep::Ready {
                    value,
                    work: measured,
                } => {
                    work = measured;
                    Some(value)
                }
            }
        } else {
            None
        };
        self.link = Some(Scope {
            source,
            label: proof.label_range(),
            head: self.head,
            depth: self.depth,
            bounds: std::mem::take(&mut self.bounds),
            previous,
        });
        self.head = 0;
        self.depth = 0;
        Ok((work, true))
    }

    /// Match End against the exact current owner/label and restore one original scope from the same
    /// file, without searching ancestors or rebuilding an AST. Read its checkpoint before moving RAM
    /// state so a short read/I/O error cannot expose a partially restored scope to subsequent runs.
    pub(in crate::markdown_document_buffer) fn leave_link(
        &mut self,
        source: &Range<usize>,
        label: &Range<usize>,
        budget: usize,
        file: &mut StackFile,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if self.busy()
            || self
                .link
                .as_ref()
                .is_none_or(|scope| &scope.source != source || &scope.label != label)
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        let previous = if let Some(id) = self.link.as_ref().unwrap().previous {
            work.structural_transitions += 3 * RECORD;
            if work.structural_transitions > budget {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            Some(file.read_scope(id)?)
        } else {
            None
        };
        let outer = self.link.take().unwrap();
        self.head = outer.head;
        self.depth = outer.depth;
        self.bounds = outer.bounds;
        self.link = previous;
        Ok(work)
    }

    /// Restart with the remaining stack's actual depth and original run_length; consumed count must
    /// not change the delimiter's mod3 classification.
    fn begin_search(&mut self) {
        let run = self.run.as_ref().unwrap();
        self.search = self.head;
        self.lower = self
            .bounds
            .lower_bound(run.delimiter, run.run_length, run.both)
            .min(self.depth);
    }

    /// The source cursor resumes only after this run completes. An intermediate match does not prove
    /// that all block styles have closed.
    pub(in crate::markdown_document_buffer) fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// Perform at most one record read/write or emit one actual matched span; never traverse the
    /// entire stack or a giant delimiter run inside one quantum.
    pub(in crate::markdown_document_buffer) fn advance(
        &mut self,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
        file: &mut StackFile,
    ) -> Result<(SemanticWork, Option<SpanProof>), MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        match self.phase {
            Phase::Idle => {}
            Phase::Search => {
                if self.search == 0 {
                    let run = self.run.as_ref().unwrap();
                    self.bounds
                        .failed(run.delimiter, run.run_length, run.both, self.depth);
                    self.phase = Phase::Finish;
                } else {
                    let entry = file.read(self.search)?;
                    work.structural_transitions += RECORD;
                    let run = self.run.as_ref().unwrap();
                    if entry.depth < self.lower {
                        self.bounds
                            .failed(run.delimiter, run.run_length, run.both, self.depth);
                        self.phase = Phase::Finish;
                    } else if inline_emphasis::matches(
                        run.delimiter,
                        run.run_length,
                        run.both,
                        entry.delimiter,
                        entry.run_length,
                        entry.both,
                    ) {
                        self.head = entry.previous;
                        self.depth = entry.depth;
                        self.bounds.truncate(self.depth);
                        self.match_count = run.count.min(entry.count);
                        self.remaining = self.match_count;
                        self.open_end = entry.start + entry.count;
                        self.close_start = run.start;
                        self.matched = Some(entry);
                        self.phase = Phase::Consume;
                    } else {
                        self.search = entry.previous;
                    }
                }
            }
            Phase::Consume => {
                let count = inline_emphasis::consume_count(self.remaining);
                let run = self.run.as_ref().unwrap();
                let style = inline_emphasis::style(run.delimiter, count, self.options);
                let whole = self.open_end - count..self.close_start + count;
                self.open_end -= count;
                self.close_start += count;
                self.remaining -= count;
                if self.remaining == 0 {
                    self.phase = Phase::Remainder;
                }
                if let Some(styles) = presentation_style(style) {
                    let width = match style {
                        EmphasisStyle::Strong | EmphasisStyle::Strikethrough => 2,
                        _ => 1,
                    };
                    let body = if width <= count {
                        whole.start + width..whole.end - width
                    } else {
                        whole.clone() // Preserve the style without inventing marker width when presentation cannot identify it.
                    };
                    return Ok((
                        work,
                        Some(SpanProof {
                            whole,
                            body,
                            styles,
                        }),
                    ));
                }
            }
            Phase::Remainder => {
                let entry = *self.matched.as_ref().unwrap();
                if entry.count > self.match_count {
                    self.bounds.before_push(entry.delimiter, self.depth);
                    let stored = file.append(
                        Entry {
                            previous: self.head,
                            depth: self.depth,
                            count: entry.count - self.match_count,
                            ..entry
                        },
                        quota,
                        budget,
                    )?;
                    let (head, measured) = match stored {
                        RecordStep::Progress { work } => return Ok((work, None)),
                        RecordStep::Ready { value, work } => (value, work),
                    };
                    self.head = head;
                    self.depth += 1;
                    work = measured;
                }
                self.matched = None;
                let run = self.run.as_mut().unwrap();
                run.start += self.match_count;
                run.count -= self.match_count;
                if run.count == 0 {
                    self.run = None;
                    self.phase = Phase::Idle;
                } else {
                    self.begin_search();
                    self.phase = Phase::Search;
                }
            }
            Phase::Finish => {
                let run = *self.run.as_ref().unwrap();
                if run.can_open && run.count > 0 {
                    self.bounds.before_push(run.delimiter, self.depth);
                    let stored = file.append(
                        Entry {
                            previous: self.head,
                            depth: self.depth,
                            start: run.start,
                            count: run.count,
                            run_length: run.run_length,
                            delimiter: run.delimiter,
                            both: run.both,
                        },
                        quota,
                        budget,
                    )?;
                    let (head, measured) = match stored {
                        RecordStep::Progress { work } => return Ok((work, None)),
                        RecordStep::Ready { value, work } => (value, work),
                    };
                    self.head = head;
                    self.depth += 1;
                    work = measured;
                }
                self.run = None;
                self.phase = Phase::Idle;
            }
        }
        Ok((work, None))
    }

    /// End algorithmic scope authority for the completed block. The shared underline transaction
    /// closes the unique file after its last consumer finishes.
    pub(in crate::markdown_document_buffer) fn finish(&mut self) {
        self.link = None;
    }
}

/// Map only categories issued by the original kernel. The parent retains Source authority and the
/// complete block endpoint.
fn presentation_style(style: EmphasisStyle) -> Option<MarkdownPresentationStyles> {
    Some(match style {
        EmphasisStyle::Literal => return None,
        EmphasisStyle::Emphasis => MarkdownPresentationStyles::EMPHASIS,
        EmphasisStyle::Strong => MarkdownPresentationStyles::STRONG,
        EmphasisStyle::Strikethrough => MarkdownPresentationStyles::STRIKETHROUGH,
        EmphasisStyle::Subscript => MarkdownPresentationStyles::SUBSCRIPT,
        // The kernel can issue this tag only for an enabled option. The product disables the
        // extension, so reaching it is a programming error rather than a plain successful result.
        EmphasisStyle::Superscript => unreachable!("未启用上标扩展"),
    })
}
