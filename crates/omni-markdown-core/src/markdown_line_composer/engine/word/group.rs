//! Logical word movement and selection within a hard line share one original
//! grapheme cursor. This entry does not itself implement bidi visual traversal.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownWordGroupAction {
    Backward,
    Forward,
    /// Bias chooses the adjacent left/right group. At an actual hard-line edge,
    /// real neighboring newline facts redirect selection toward the existing side.
    Select {
        backward_bias: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownWordGroupAnswer {
    Yielded,
    /// A prepared range lacks control-free level-zero proof. This logical group
    /// cannot provide RTL visual traversal, but the healthy Source remains usable.
    UnsupportedVisualOrder,
    /// Preserve actual step work. This group cannot read across a hidden Source
    /// range before the original fold has been expanded.
    NeedsFoldExpansion(Range<usize>),
    /// Actual logical range in the original Source. Movement chooses its directional
    /// endpoint and Select uses the full range; Native commit authority is separate.
    Ready(Range<usize>),
}

pub struct MarkdownWordGroupStep {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
    pub answer: MarkdownWordGroupAnswer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownWordMoveScope {
    WholeUniformZero,
    /// Each actual grapheme still needs its complete original bidi paragraph proof;
    /// this scope grants no level-zero authority at an arbitrary position.
    VisibleParagraphScopes,
    Unsupported,
}

/// Actual original UBA preparation work. None remains pending; a visible subrange
/// cannot replace the Source's direction responsibilities.
pub struct MarkdownWordMovePreparation {
    pub bytes: usize,
    pub scalars: usize,
    pub metadata_units: usize,
    pub scope: Option<MarkdownWordMoveScope>,
}

enum Phase {
    Move,
    Seed { backward: bool, fallback: bool },
    Left,
    Right,
    Complete(Range<usize>),
    Unsupported,
}

/// Advance both sides sequentially through one cursor. Phase changes cannot spend
/// a second byte/record allowance in the same turn.
pub(in crate::markdown_line_composer::engine) struct WordGroup {
    head: usize,
    category: Option<Category>,
    start: usize,
    phase: Phase,
    cursor: WordDelete,
    restore_position: Option<usize>,
    deferred_answer: Option<MarkdownWordGroupAnswer>,
}

impl WordGroup {
    fn new(action: MarkdownWordGroupAction, head: usize, length: usize) -> Self {
        let (direction, mode, phase) = match action {
            // Start at the original head, skip all leading space, then consume one
            // category. Movement does not use deletion's single-space exception.
            MarkdownWordGroupAction::Backward => (
                MarkdownWordDeleteDirection::Backward,
                CursorMode::Move,
                Phase::Move,
            ),
            // Forward movement follows the same rule; consecutive tabs/newlines
            // are not split into deletion-specific standalone steps.
            MarkdownWordGroupAction::Forward => (
                MarkdownWordDeleteDirection::Forward,
                CursorMode::Move,
                Phase::Move,
            ),
            MarkdownWordGroupAction::Select { backward_bias } => {
                // Bias chooses the seed category. Whole-Source edges select the
                // existing side; actual newline facts later verify hard-line edges.
                let backward = head != 0 && (head == length || backward_bias);
                (
                    if backward {
                        MarkdownWordDeleteDirection::Backward
                    } else {
                        MarkdownWordDeleteDirection::Forward
                    },
                    CursorMode::Seed,
                    Phase::Seed {
                        backward,
                        fallback: false,
                    },
                )
            }
        };
        Self {
            head,
            category: None,
            start: head,
            phase,
            cursor: WordDelete::group(direction, head, mode),
            restore_position: None,
            deferred_answer: None,
        }
    }

    fn advance(
        &mut self,
        source: &MarkdownLayoutTextSource,
        coordinator: &mut MarkdownLineItemCoordinator,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordGroupStep> {
        if self.deferred_answer.is_some() {
            let position = self
                .restore_position
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let (actual, restored) = coordinator.restore_word_layout_context(position, budget)?;
            let answer = if restored {
                self.restore_position = None;
                self.deferred_answer
                    .take()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            } else {
                MarkdownWordGroupAnswer::Yielded
            };
            return Ok(MarkdownWordGroupStep {
                bytes: actual.bytes,
                scalars: actual.scalars,
                records: actual.records,
                metadata_units: actual.metadata_units,
                answer,
            });
        } // Original bidi ownership still manages its real paragraph slots. A
        // restore turn does not also spend grapheme/classification work.
        if matches!(self.phase, Phase::Unsupported) {
            return Ok(self.defer_scoped_answer(MarkdownWordGroupStep {
                bytes: 0,
                scalars: 0,
                records: 0,
                metadata_units: 0,
                answer: MarkdownWordGroupAnswer::UnsupportedVisualOrder,
            }));
        } // The outer composer already checked Source/cancellation; capability
        // refusal does not reread text whose visual direction is unsupported.
        let step = self
            .cursor
            .advance(source, coordinator, authority, budget)?;
        if self.cursor.visual_rejected {
            self.phase = Phase::Unsupported;
            return Ok(self.defer_scoped_answer(MarkdownWordGroupStep {
                bytes: step.bytes,
                scalars: step.scalars,
                records: step.records,
                metadata_units: step.metadata_units,
                answer: MarkdownWordGroupAnswer::UnsupportedVisualOrder,
            }));
        } // Preserve actual paragraph preparation/I/O work even on capability refusal.
        let mut answer = match &step.answer {
            MarkdownWordDeleteAnswer::NeedsFoldExpansion(hidden) => {
                MarkdownWordGroupAnswer::NeedsFoldExpansion(hidden.clone())
            }
            _ => MarkdownWordGroupAnswer::Yielded,
        };
        if let MarkdownWordDeleteAnswer::Ready(range) = step.answer {
            match &self.phase {
                Phase::Move => {
                    // Movement reached a real group boundary. Return Source facts;
                    // Native still needs its original permit to change selection.
                    self.phase = Phase::Complete(range.clone());
                    answer = MarkdownWordGroupAnswer::Ready(range);
                }
                Phase::Seed { backward, fallback } => {
                    // Hard-line selection cannot choose the neighboring line. If
                    // the biased side has no local grapheme, try the other side once.
                    if range.is_empty() || self.cursor.seed_newline {
                        if !fallback {
                            let backward = !backward;
                            self.phase = Phase::Seed {
                                backward,
                                fallback: true,
                            };
                            self.cursor = WordDelete::group(
                                if backward {
                                    MarkdownWordDeleteDirection::Backward
                                } else {
                                    MarkdownWordDeleteDirection::Forward
                                },
                                self.head,
                                CursorMode::Seed,
                            );
                        } else {
                            let range = self.head..self.head;
                            self.phase = Phase::Complete(range.clone());
                            answer = MarkdownWordGroupAnswer::Ready(range);
                        } // Both sides being newline/end establishes an empty line,
                    // without selecting a word from a neighboring line.
                    } else {
                        // Both expansions use this complete seed grapheme's same
                        // category, rather than choosing a word left and punctuation right.
                        let category = self
                            .cursor
                            .category
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        self.category = Some(category);
                        self.phase = Phase::Left;
                        self.cursor = WordDelete::group(
                            MarkdownWordDeleteDirection::Backward,
                            self.head,
                            CursorMode::Expand(category),
                        );
                    }
                }
                Phase::Left => {
                    // With the left edge fixed, retire that cursor and use the same
                    // Source for the right half under its next single work allowance.
                    self.start = range.start;
                    self.phase = Phase::Right;
                    self.cursor = WordDelete::group(
                        MarkdownWordDeleteDirection::Forward,
                        self.head,
                        CursorMode::Expand(
                            self.category
                                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                        ),
                    );
                }
                Phase::Right => {
                    // Both sides were proven from the same head/category; combine
                    // their actual endpoints into one selection within the hard line.
                    let range = self.start..range.end;
                    self.phase = Phase::Complete(range.clone());
                    answer = MarkdownWordGroupAnswer::Ready(range);
                }
                Phase::Complete(range) => {
                    answer = MarkdownWordGroupAnswer::Ready(range.clone()); // Original health checks still apply.
                }
                Phase::Unsupported => unreachable!("能力拒绝已经在推进原游标前返回"),
            }
        }
        Ok(self.defer_scoped_answer(MarkdownWordGroupStep {
            bytes: step.bytes,
            scalars: step.scalars,
            records: step.records,
            metadata_units: step.metadata_units,
            answer,
        }))
    }

    /// Scoped terminal results first restore the original layout paragraph. The
    /// original Root fast path and logical groups need no extra stage or read.
    fn defer_scoped_answer(&mut self, mut step: MarkdownWordGroupStep) -> MarkdownWordGroupStep {
        if self.restore_position.is_some()
            && !matches!(step.answer, MarkdownWordGroupAnswer::Yielded)
        {
            self.deferred_answer = Some(step.answer);
            step.answer = MarkdownWordGroupAnswer::Yielded;
        }
        step
    }
}

impl MarkdownLineComposer {
    /// Establish the original coordinator's actual visual-word responsibility
    /// through one preparation quantum at a time.
    ///
    /// # Errors
    /// Source, cancellation, and real bidi storage failures close through the
    /// original boundary. Scope only states responsibility, not proof for unread positions.
    pub fn prepare_word_move(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordMovePreparation> {
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
            let (step, scope) = payload.coordinator.prepare_word_move(budget)?;
            Ok(MarkdownWordMovePreparation {
                bytes: step.bytes,
                scalars: step.scalars,
                metadata_units: step.metadata_units,
                scope,
            })
        })();
        self.finish(result)
    }

    /// Begin logical movement/selection at an original complete-grapheme position
    /// without another index, reader, or text copy.
    ///
    /// # Errors
    /// Source/cancellation precede range errors. An active word action must be
    /// cancelled before another replaces it.
    pub fn begin_word_group(
        &mut self,
        action: MarkdownWordGroupAction,
        head_utf16: usize,
    ) -> Result<()> {
        self.begin_group(action, head_utf16, false)
    }

    /// Preserve the complete Root certificate's fast visual path. Scoped folded
    /// layout checks each original visible grapheme; logical order never guesses RTL.
    pub fn begin_visual_word_group(
        &mut self,
        direction: MarkdownWordDeleteDirection,
        head_utf16: usize,
    ) -> Result<()> {
        let action = match direction {
            MarkdownWordDeleteDirection::Backward => MarkdownWordGroupAction::Backward,
            MarkdownWordDeleteDirection::Forward => MarkdownWordGroupAction::Forward,
        };
        self.begin_group(action, head_utf16, true)
    }

    /// Both consumers share one Source/grapheme owner. The caller supplies visual
    /// intent; only the original coordinator can supply the required authority.
    fn begin_group(
        &mut self,
        action: MarkdownWordGroupAction,
        head_utf16: usize,
        visual: bool,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if payload.word_delete.is_some() || head_utf16 > payload.source.utf16_len() {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        // Visual requests must use an already prepared real scope. Save the current
        // layout paragraph only when scoped movement may temporarily move that owner.
        let scoped = if visual {
            match payload.coordinator.word_move_scope()? {
                Some(MarkdownWordMoveScope::WholeUniformZero) => false,
                Some(MarkdownWordMoveScope::VisibleParagraphScopes) => true,
                _ => return Err(MarkdownLineComposeError::InvalidRequest),
            }
        } else {
            false
        };
        let mut group = WordGroup::new(action, head_utf16, payload.source.utf16_len());
        group.cursor.visible_word = scoped;
        if scoped {
            group.restore_position = Some(payload.coordinator.word_layout_position()?);
        }
        payload.word_delete = Some(WordJob::Group(group));
        Ok(())
    }

    /// Advance the current side under one 4-KiB/256-scalar/grapheme-record quantum.
    /// Huge words or single graphemes continue yielding instead of truncating.
    ///
    /// # Errors
    /// Non-grapheme positions, actual Source/index failure, and cancellation close
    /// the same composer; a later Source cannot continue this action.
    pub fn step_word_group(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordGroupStep> {
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
            let Some(WordJob::Group(group)) = payload.word_delete.as_mut() else {
                return Err(MarkdownLineComposeError::InvalidRequest);
            };
            group.advance(
                &payload.source,
                &mut payload.coordinator,
                &self.authority,
                budget,
            )
        })();
        self.finish(result)
    }

    /// Cancel only this group cursor. Original Source/fonts/layout dependencies
    /// retain their composer lifecycle, and a different delete action stays intact.
    pub fn cancel_word_group(&mut self) {
        if let Some(payload) = self.payload.as_mut()
            && matches!(payload.word_delete, Some(WordJob::Group(_)))
        {
            payload.word_delete = None;
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod move_tests;

#[cfg(test)]
mod visible_tests;
