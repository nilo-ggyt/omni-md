//! Find word-deletion ranges incrementally over original graphemes and frozen
//! pieces. Viewport, chunk, and soft-line edges never substitute for real boundaries.

use super::*;
use crate::{MarkdownDocumentBufferError, MarkdownGraphemeBoundary, MarkdownLineBreakBudget};
use std::ops::Range;

mod group;
pub use group::{
    MarkdownWordGroupAction, MarkdownWordGroupAnswer, MarkdownWordGroupStep,
    MarkdownWordMovePreparation, MarkdownWordMoveScope,
};

/// One word action belongs to this composer. Move/select/delete share original
/// grapheme metadata rather than create parallel indexes for separate actions.
pub(super) enum WordJob {
    Delete(WordDelete),
    Group(group::WordGroup),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownWordDeleteDirection {
    Backward,
    Forward,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownWordDeleteAnswer {
    Yielded,
    /// This quantum reached a real opaque endpoint. The caller must expand the
    /// original fold/rebuild its read graph; this grants no deletion of hidden text.
    NeedsFoldExpansion(Range<usize>),
    /// An actual range in the original Source. The caller still needs its original
    /// input permit; this result cannot renew authority over current document text.
    Ready(Range<usize>),
}

pub struct MarkdownWordDeleteStep {
    pub bytes: usize,
    pub scalars: usize,
    /// Grapheme-record work, including Dense integer records; not a count of preads.
    pub records: usize,
    pub metadata_units: usize,
    pub answer: MarkdownWordDeleteAnswer,
}

impl MarkdownWordDeleteStep {
    fn yielded() -> Self {
        Self {
            bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            answer: MarkdownWordDeleteAnswer::Yielded,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(in crate::markdown_line_composer) enum Category {
    Space,
    Word,
    Other,
}

#[derive(Clone, Copy)]
/// Consumers share a grapheme cursor but retain their own stopping rules. Ordinary
/// deletion has no word categories; movement has no deletion-specific space exception.
enum CursorMode {
    Delete,
    /// Ordinary deletion uses one adjacent real grapheme, without category reads or
    /// dependence on soft-line geometry.
    Grapheme,
    Move,
    Seed,
    Expand(Category),
}

/// Binary search retains an original stable ordinal interval. Coordinates and EOF
/// length cannot infer a boundary record that was never read.
struct Seek {
    low: usize,
    high: usize,
    candidate: Option<(usize, MarkdownGraphemeBoundary)>,
}

/// A single grapheme can exceed 4 KiB. Accumulate its category and one partial UTF-8
/// scalar without retaining all grapheme bytes.
pub(in crate::markdown_line_composer) struct Cluster {
    next_ordinal: usize,
    next: MarkdownGraphemeBoundary,
    byte: usize,
    end: MarkdownGraphemeBoundary,
    utf16: usize,
    pending: [u8; 4],
    used: usize,
    needed: usize,
    word: bool,
    space: bool,
    newline: bool,
    single_space: bool,
}

impl Cluster {
    pub(in crate::markdown_line_composer) fn new(
        next_ordinal: usize,
        current: MarkdownGraphemeBoundary,
        next: MarkdownGraphemeBoundary,
    ) -> Result<Self> {
        let (start, end) = if current.byte < next.byte {
            (current, next)
        } else {
            (next, current)
        };
        // Adjacent original boundaries must strictly increase on both coordinate
        // axes before their metadata is allowed to drive Source reads.
        if start.byte >= end.byte || start.utf16 >= end.utf16 {
            return Err(MarkdownLineComposeError::InconsistentCoverage);
        }
        Ok(Self {
            next_ordinal,
            next,
            byte: start.byte,
            end,
            utf16: start.utf16,
            pending: [0; 4],
            used: 0,
            needed: 0,
            word: false,
            space: true,
            newline: false,
            single_space: end.byte - start.byte == 1,
        })
    }

    fn push(&mut self, byte: u8, step: &mut MarkdownWordDeleteStep) -> Result<()> {
        self.byte += 1;
        step.bytes += 1;
        if self.used == 0 {
            self.needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
        } // A real grapheme start cannot excuse malformed UTF-8 or insert replacements.
        self.pending[self.used] = byte;
        self.used += 1;
        if self.used != self.needed {
            return Ok(()); // Retain this partial scalar across piece/quantum boundaries.
        }
        let scalar = std::str::from_utf8(&self.pending[..self.used])
            .ok()
            .and_then(|value| value.chars().next())
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        // Preserve the original editor's three group categories over the complete
        // grapheme. UTF-16 units cannot independently classify emoji or marks.
        self.word |= scalar.is_alphabetic() || scalar.is_numeric() || scalar == '_';
        self.space &= ecmascript_space(scalar);
        self.newline |= matches!(scalar, '\r' | '\n');
        self.single_space &= scalar == ' ';
        self.utf16 += scalar.len_utf16();
        self.used = 0;
        step.scalars += 1;
        Ok(())
    }

    pub(in crate::markdown_line_composer) fn read(
        &mut self,
        source: &MarkdownLayoutTextSource,
        budget: MarkdownLineComposeBudget,
        step: &mut MarkdownWordDeleteStep,
    ) -> Result<bool> {
        let end = self
            .byte
            .saturating_add(budget.bytes - step.bytes)
            .min(self.end.byte);
        let mut stopped = false;
        let mut failure = None;
        let result = source.visit_byte_range(self.byte..end, |piece| {
            for byte in piece {
                if step.scalars == budget.scalars {
                    stopped = true;
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                } // Check scalar quota before dereferencing the next input byte.
                if let Err(error) = self.push(*byte, step) {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
                if stopped || failure.is_some() => {}
            Err(error) => return Err(error.into()),
        } // Consume only this callback's pause/failure sentinel. Actual Source
        // failure still reaches the composer's original first-error boundary.
        if let Some(error) = failure {
            return Err(error);
        }
        if self.byte == self.end.byte {
            if self.used != 0 || self.utf16 != self.end.utf16 {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            } // Actual decoded content must match both original endpoint coordinates.
            return Ok(true);
        }
        Ok(false)
    }

    pub(in crate::markdown_line_composer) fn category(&self) -> Category {
        if self.space {
            Category::Space
        } else if self.word {
            Category::Word
        } else {
            Category::Other
        }
    }
}

/// Preserve the original ECMAScript whitespace set: it includes BOM and excludes
/// NEL. Rust's broader is_whitespace would change existing editor group behavior.
fn ecmascript_space(scalar: char) -> bool {
    matches!(scalar, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

pub(super) struct WordDelete {
    mode: CursorMode,
    direction: MarkdownWordDeleteDirection,
    head: usize,
    at: Option<(usize, MarkdownGraphemeBoundary)>,
    seek: Option<Seek>,
    cluster: Option<Cluster>,
    started: bool,
    category: Option<Category>,
    dependency: bool,
    ready: Option<Range<usize>>,
    ascii_ready: Option<bool>,
    ascii_run: Option<crate::markdown_document_buffer::MarkdownAsciiWordRun>,
    ascii_endpoint: Option<usize>,
    ascii_skip: bool,
    seed_newline: bool,
    visible_word: bool,
    visible_proved: bool,
    visible_proof_next: Option<usize>,
    visual_rejected: bool,
}

impl WordDelete {
    fn new(direction: MarkdownWordDeleteDirection, head: usize) -> Self {
        Self {
            mode: CursorMode::Delete,
            direction,
            head,
            at: None,
            seek: None,
            cluster: None,
            started: false,
            category: None,
            dependency: false,
            ready: None,
            ascii_ready: None,
            ascii_run: None,
            ascii_endpoint: None,
            ascii_skip: false,
            seed_newline: false,
            visible_word: false,
            visible_proved: false,
            visible_proof_next: None,
            visual_rejected: false,
        }
    }

    /// A logical group changes only this cursor's stopping rule. Original Payload
    /// ownership still governs Source, grapheme dependencies, and each work quantum.
    fn group(direction: MarkdownWordDeleteDirection, head: usize, mode: CursorMode) -> Self {
        let mut cursor = Self::new(direction, head);
        cursor.mode = mode;
        if let CursorMode::Expand(category) = mode {
            cursor.category = Some(category); // Both sides expand the same seed category.
        }
        cursor
    }

    fn finish(&mut self, step: &mut MarkdownWordDeleteStep) -> Result<()> {
        // Return only the range between original head and the actual current
        // boundary. This does not apply an edit or renew any Native input permit.
        let position = self
            .at
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .1
            .utf16;
        let range = self.head.min(position)..self.head.max(position);
        self.ready = Some(range.clone());
        step.answer = MarkdownWordDeleteAnswer::Ready(range);
        Ok(())
    }

    /// Only original whole-Source Dense graphemes permit alphanumeric tree-summary
    /// traversal. Mixed edge leaves still charge scalar work and return the stopping
    /// character to the original grapheme/category rules.
    fn ascii_alnum(
        &mut self,
        source: &MarkdownLayoutTextSource,
        breaks: &mut crate::MarkdownLayoutBreakJob,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineComposeBudget,
        step: &mut MarkdownWordDeleteStep,
    ) -> Result<bool> {
        if self.visible_word
            || matches!(self.mode, CursorMode::Seed | CursorMode::Grapheme)
            || self.ascii_skip
            || self
                .category
                .is_some_and(|category| category != Category::Word)
        {
            return Ok(false); // A locked Space/Other group cannot skip into a later word.
        }
        if self.ascii_ready.is_none() {
            if step.metadata_units == budget.metadata_units {
                return Ok(true);
            }
            self.ascii_ready = Some(breaks.ascii_prefix_ready(authority)?);
            step.metadata_units += 1;
        }
        if self.ascii_ready != Some(true) {
            // Unicode/CRLF or uncertified Dense stays on the original grapheme
            // path. A locally ASCII leaf alone grants no summary traversal.
            return Ok(false);
        }
        let current = self
            .at
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .1;
        if self.ascii_endpoint.is_none() {
            if self.ascii_run.is_none() {
                if step.metadata_units == budget.metadata_units {
                    return Ok(true);
                }
                self.ascii_run = Some(source.begin_ascii_word_run(
                    current.utf16,
                    self.direction == MarkdownWordDeleteDirection::Forward,
                )?);
                step.metadata_units += 1; // Capturing the cursor inspects real root-domain facts.
            }
            let run = self
                .ascii_run
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let progress = source.advance_ascii_word_run(
                run,
                budget.bytes - step.bytes,
                budget.scalars - step.scalars,
                budget.metadata_units - step.metadata_units,
            )?;
            step.bytes += progress.bytes;
            step.scalars += progress.scalars;
            step.metadata_units += progress.metadata;
            if !progress.complete {
                return Ok(true); // Preserve this node/edge cursor even under quota one.
            }
            self.ascii_run = None; // Release actual node Arcs within this original Source action.
            if progress.position == current.utf16 {
                self.ascii_skip = true;
                return Ok(false); // A non-alphanumeric stopping character still belongs to GCB rules.
            }
            self.ascii_endpoint = Some(progress.position);
        }
        if step.records == budget.records || step.metadata_units == budget.metadata_units {
            return Ok(true);
        }
        let position = self
            .ascii_endpoint
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let (_, endpoint) = breaks.certified_grapheme_endpoint(authority, position)?;
        step.records += 1;
        step.metadata_units += 1;
        let endpoint = endpoint
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .boundary(authority)?;
        if endpoint.utf16 != position || endpoint.byte != position {
            return Err(MarkdownLineComposeError::InconsistentCoverage);
        }
        // Ordinal equals UTF-16 only when both original whole-Source Dense facts and
        // this actual endpoint proof establish that identity.
        self.at = Some((position, endpoint));
        self.started = true;
        self.category = Some(Category::Word);
        self.ascii_endpoint = None;
        self.ascii_skip = true; // Interpret the stopping character once through Cluster.
        Ok(false)
    }

    fn advance(
        &mut self,
        source: &MarkdownLayoutTextSource,
        coordinator: &mut MarkdownLineItemCoordinator,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordDeleteStep> {
        if self.visible_word
            && !self.visible_proved
            && let Some(cluster) = &self.cluster
        {
            let position = self.visible_proof_next.unwrap_or(cluster.utf16);
            let (actual, proof) =
                coordinator.prepare_word_cluster(position..cluster.end.utf16, budget)?;
            if let Some((permitted, through)) = proof {
                if through <= position || through > cluster.end.utf16 {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                } // A real bidi paragraph proof must advance inside this grapheme,
                // never stay still or infer coverage beyond its original end.
                self.visual_rejected = !permitted;
                self.visible_proved = permitted && through == cluster.end.utf16;
                self.visible_proof_next = (permitted && !self.visible_proved).then_some(through);
            }
            return Ok(MarkdownWordDeleteStep {
                bytes: actual.bytes,
                scalars: actual.scalars,
                records: actual.records,
                metadata_units: actual.metadata_units,
                answer: MarkdownWordDeleteAnswer::Yielded,
            });
        } // Complete this item's bidi preparation before borrowing Breaks; both
        // mutable roles remain sequential under the same grapheme authority.
        let folds = coordinator.fold_plan();
        let breaks = coordinator.word_breaks()?;
        if self.dependency {
            // Advance the original unknown-boundary dependency alone. Classifying
            // text in this same turn would spend the work allowance a second time.
            let progress = breaks.step_dependency(
                authority,
                true,
                MarkdownLineBreakBudget {
                    bytes: budget.bytes,
                    scalars: budget.scalars,
                    records: budget.records.min(budget.metadata_units),
                },
            )?;
            self.dependency = false;
            return Ok(MarkdownWordDeleteStep {
                bytes: progress.bytes,
                scalars: progress.scalars,
                records: progress.records,
                metadata_units: progress.records,
                answer: MarkdownWordDeleteAnswer::Yielded,
            });
        }
        let prefix = breaks.grapheme_prefix_facts(authority)?;
        let mut step = MarkdownWordDeleteStep::yielded();
        if let Some(range) = &self.ready {
            step.answer = MarkdownWordDeleteAnswer::Ready(range.clone());
            return Ok(step); // Even terminal results passed original GCB/Source health checks.
        }
        if self.at.is_none() && self.seek.is_none() {
            if prefix
                .last_committed_boundary
                .is_none_or(|last| last.utf16 < self.head)
            {
                if prefix.complete {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                self.dependency = true;
                return Ok(step); // Source length cannot prove the unknown head ordinal.
            }
            self.seek = Some(Seek {
                low: 0,
                high: prefix.known_boundary_count,
                candidate: None,
            });
        }
        loop {
            if let Some(seek) = self.seek.as_mut() {
                if seek.low == seek.high {
                    let (ordinal, boundary) = seek
                        .candidate
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                    if ordinal != seek.low {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    } // A contradictory candidate/rank is damaged coverage, not a user offset error.
                    if boundary.utf16 != self.head {
                        if !matches!(self.mode, CursorMode::Grapheme) {
                            return Err(MarkdownLineComposeError::InvalidRequest);
                        } // Word groups require real GCBs. Ordinary deletion keeps
                        // the original Scene behavior for a valid scalar caret inside one.
                        self.at = Some((ordinal, boundary));
                        self.seek = None;
                        if self.direction == MarkdownWordDeleteDirection::Forward {
                            // Inside a grapheme, original Next uses lower_bound
                            // itself; do not skip another grapheme or expand left.
                            self.finish(&mut step)?;
                            return Ok(step);
                        }
                        continue; // Previous uses the real predecessor, charged on its next read.
                    }
                    self.at = Some((ordinal, boundary));
                    self.seek = None;
                    continue;
                }
                if step.records == budget.records || step.metadata_units == budget.metadata_units {
                    return Ok(step);
                } // Every binary-search record consumes this same quantum.
                let ordinal = seek.low + (seek.high - seek.low) / 2;
                let mut record = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                if !breaks.read_grapheme_prefix(authority, ordinal..ordinal + 1, &mut record)? {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                } // This search only uses its initial committed prefix; losing a
                // record cannot be reinterpreted as waiting for more text.
                step.records += 1;
                step.metadata_units += 1;
                let boundary = record[0];
                if boundary.utf16 < self.head {
                    seek.low = ordinal + 1;
                } else {
                    seek.high = ordinal;
                    seek.candidate = Some((ordinal, boundary));
                }
                continue;
            }
            if self.cluster.is_none() {
                if self.ascii_alnum(source, breaks, authority, budget, &mut step)? {
                    return Ok(step);
                }
                let (ordinal, current) = self
                    .at
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let next = match self.direction {
                    MarkdownWordDeleteDirection::Backward => ordinal.checked_sub(1),
                    MarkdownWordDeleteDirection::Forward => ordinal.checked_add(1),
                };
                let Some(next) = next else {
                    self.finish(&mut step)?;
                    return Ok(step); // No predecessor at ordinal zero means real BOF.
                };
                if next >= prefix.known_boundary_count {
                    if prefix.complete {
                        self.finish(&mut step)?;
                    } else {
                        self.dependency = true;
                    }
                    return Ok(step); // Only a complete original GCB table proves EOF.
                }
                if step.records == budget.records || step.metadata_units == budget.metadata_units {
                    return Ok(step);
                }
                let mut record = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                if !breaks.read_grapheme_prefix(authority, next..next + 1, &mut record)? {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                step.records += 1;
                step.metadata_units += 1;
                if (self.direction == MarkdownWordDeleteDirection::Backward)
                    != (record[0].byte < current.byte)
                {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                } // Adjacent ordinal direction must match actual Source order;
                // damaged metadata cannot reverse a backward deletion.
                if let Some(plan) = &folds
                    && let Some(span) = plan.spans().iter().find(|span| {
                        span.start.utf16 == current.utf16.min(record[0].utf16)
                            && span.end.utf16 == current.utf16.max(record[0].utf16)
                    })
                {
                    step.answer = MarkdownWordDeleteAnswer::NeedsFoldExpansion(
                        span.start.utf16..span.end.utf16,
                    );
                    return Ok(step);
                } // Return the original unfold intent instead of deleting the
                // hidden section as one character or materializing it for classification.
                if matches!(self.mode, CursorMode::Grapheme) {
                    // The same GCB directory certifies these adjacent ordinals.
                    // Emoji, CRLF, and arbitrarily long combining clusters use their
                    // real endpoints without scalar/viewport truncation.
                    self.at = Some((next, record[0]));
                    self.finish(&mut step)?;
                    return Ok(step);
                }
                self.cluster = Some(Cluster::new(next, current, record[0])?);
                if self.visible_word {
                    self.visible_proved = false;
                    self.visible_proof_next = None;
                    return Ok(step);
                } // After opaque handling, a later turn obtains the original bidi
                // paragraph proof before reading/classifying this grapheme.
            }
            if step.bytes == budget.bytes || step.scalars == budget.scalars {
                return Ok(step);
            } // Keep actual endpoints in this continuation; a turn end is no group end.
            let cluster = self
                .cluster
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            if !cluster.read(source, budget, &mut step)? {
                return Ok(step);
            }
            let cluster = self
                .cluster
                .take()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            self.visible_proved = false;
            self.visible_proof_next = None;
            match self.mode {
                CursorMode::Seed => {
                    // The seed establishes category and hard-newline facts. Even
                    // a huge single grapheme must be completely read to classify it.
                    self.category = Some(cluster.category());
                    self.seed_newline = cluster.newline;
                    self.at = Some((cluster.next_ordinal, cluster.next));
                    self.finish(&mut step)?;
                    return Ok(step);
                }
                CursorMode::Move => {
                    let category = cluster.category();
                    if self
                        .category
                        .is_some_and(|previous| previous != Category::Space && previous != category)
                    {
                        self.finish(&mut step)?;
                        return Ok(step); // After leading space, consume one Word/Other group.
                    }
                    self.category = Some(category);
                    self.started = true;
                    self.at = Some((cluster.next_ordinal, cluster.next));
                    self.ascii_skip = false;
                    continue;
                }
                CursorMode::Expand(category) => {
                    if cluster.newline || cluster.category() != category {
                        self.finish(&mut step)?;
                        return Ok(step); // Selection stays in one hard line, including Space groups.
                    }
                    self.started = true;
                    self.at = Some((cluster.next_ordinal, cluster.next));
                    self.ascii_skip = false;
                    continue;
                }
                CursorMode::Grapheme => unreachable!("单GCB动作在取得相邻原边界时已经结束"),
                CursorMode::Delete => {} // Preserve deletion's single-space/newline rules below.
            }
            if cluster.newline {
                if !self.started {
                    self.at = Some((cluster.next_ordinal, cluster.next));
                } // Consume CR/LF/CRLF only at the action's initial boundary;
                // a previously started group never crosses that hard newline.
                self.finish(&mut step)?;
                return Ok(step);
            }
            let category = cluster.category();
            if self.category.is_some_and(|previous| previous != category) {
                self.finish(&mut step)?;
                return Ok(step); // The next-category grapheme remains outside the range.
            }
            if self.started || !cluster.single_space {
                self.category = Some(category);
            } // Only the initial single ASCII space leaves category unlocked;
            // a second space/tab locks Space, preserving original deletion behavior.
            self.started = true;
            self.at = Some((cluster.next_ordinal, cluster.next));
            self.ascii_skip = false; // The stopping character was consumed; later ASCII may resume.
        }
    }
}

impl MarkdownLineComposer {
    /// Begin word deletion on this worker's original frozen Source. Establish only
    /// direction/position, without scanning/copying text or allocating I/O resources.
    ///
    /// # Errors
    /// Original Source/cancellation errors precede range errors. An unconsumed word
    /// action cannot be silently replaced by another request.
    pub fn begin_word_delete(
        &mut self,
        direction: MarkdownWordDeleteDirection,
        head_utf16: usize,
    ) -> Result<()> {
        self.begin_delete(direction, head_utf16, CursorMode::Delete)
    }

    /// Begin forward-delete/backspace using original adjacent complete graphemes.
    /// A scalar caret inside a grapheme retains strict original Next/Previous rules.
    ///
    /// # Errors
    /// Source, cancellation, unfinished-action, and range failures share word
    /// deletion's original boundary without another Source or index.
    pub fn begin_grapheme_delete(
        &mut self,
        direction: MarkdownWordDeleteDirection,
        head_utf16: usize,
    ) -> Result<()> {
        self.begin_delete(direction, head_utf16, CursorMode::Grapheme)
    }

    /// The two deletion modes differ only in stopping rules. Authority validation
    /// and the one original action slot share this entry.
    fn begin_delete(
        &mut self,
        direction: MarkdownWordDeleteDirection,
        head_utf16: usize,
        mode: CursorMode,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if payload.word_delete.is_some() || head_utf16 > payload.source.utf16_len() {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        if matches!(mode, CursorMode::Grapheme) {
            // NSTextInputClient permits a scalar caret inside a grapheme, but a
            // surrogate-pair interior cannot drive an irreversible edit. Reuse the
            // original Source UTF-16-to-byte check without guessing Unicode boundaries.
            payload.source.byte_offset_for_utf16(head_utf16)?;
        }
        let mut cursor = WordDelete::new(direction, head_utf16);
        cursor.mode = mode;
        payload.word_delete = Some(WordJob::Delete(cursor));
        Ok(())
    }

    /// Advance original grapheme metadata or text classification for this action
    /// under shared 4-KiB/256-scalar/256-record per-step bounds.
    ///
    /// # Errors
    /// Preserve typed Source, index, cancellation, and non-grapheme-position failures;
    /// the original composer failure boundary releases actual resources.
    pub fn step_word_delete(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordDeleteStep> {
        let result = (|| {
            self.guard()?;
            if budget.bytes == 0
                || budget.scalars == 0
                || budget.records == 0
                || budget.metadata_units == 0
            {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let budget = MarkdownLineComposeBudget {
                bytes: budget.bytes.min(4096),
                scalars: budget.scalars.min(256),
                records: budget.records.min(256),
                metadata_units: budget.metadata_units.min(256),
            };
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let word = payload
                .word_delete
                .as_mut()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            let WordJob::Delete(word) = word else {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }; // A different active group action cannot be consumed through delete.
            word.advance(
                &payload.source,
                &mut payload.coordinator,
                &self.authority,
                budget,
            )
        })();
        self.finish(result)
    }

    /// When original input authority ends, retire only this temporary scalar cursor.
    /// Font/layout and the same Source remain under the original worker lifecycle.
    pub fn cancel_word_delete(&mut self) {
        if let Some(payload) = self.payload.as_mut()
            && matches!(payload.word_delete, Some(WordJob::Delete(_)))
        {
            payload.word_delete = None;
        }
    }
}

#[cfg(test)]
mod fold_tests;
#[cfg(test)]
mod tests;
