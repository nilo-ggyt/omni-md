//! Visual words consume original L1/L2 neighboring graphemes and the established
//! classifier. Only one cluster and one weak visual query survive each quantum.

use super::*;
use crate::markdown_line_composer::engine::word::{Category, Cluster};
use crate::{MarkdownLineComposeBudget, MarkdownWordDeleteAnswer, MarkdownWordDeleteStep};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownComposedVisualWordAnswer {
    Pending,
    PrefixNotCovered,
    NeedsFoldExpansion(Range<usize>),
    Found {
        utf16: usize,
        side: MarkdownComposedCaretSide,
        line: Range<usize>,
        ordinal: usize,
    },
}

pub struct MarkdownComposedVisualWordStep {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
    pub answer: MarkdownComposedVisualWordAnswer,
}

/// Classified progress belongs to the original writer/Source. Weak tickets
/// retain no reader, font payload, Source payload, or another UBA owner.
pub struct MarkdownComposedVisualWordQuery {
    lineage: Weak<()>,
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    position: usize,
    affinity: MarkdownComposedLineAffinity,
    side: Option<MarkdownComposedCaretSide>,
    right: bool,
    forward: Option<bool>,
    category: Option<Category>,
    visual: Option<MarkdownComposedVisualQuery>,
    cluster: Option<Cluster>,
    candidate: Option<MarkdownComposedVisualAnswer>,
    last: Option<MarkdownComposedVisualAnswer>,
    last_fonts: [Option<MarkdownLineFontRegistration>; 5],
    candidate_fonts: [Option<MarkdownLineFontRegistration>; 5],
    answer: Option<MarkdownComposedVisualWordAnswer>,
    failed: bool,
}

impl MarkdownComposedLines {
    /// Capture actual screen direction. Logical Forward/Backward use their own
    /// established query and never infer character order from paragraph base.
    pub fn visual_word_query(
        &mut self,
        utf16: usize,
        affinity: MarkdownComposedLineAffinity,
        side: Option<MarkdownComposedCaretSide>,
        right: bool,
    ) -> Result<MarkdownComposedVisualWordQuery> {
        let visual = self.visual_query(utf16, affinity, side, right)?;
        Ok(MarkdownComposedVisualWordQuery {
            lineage: self.lineage.clone(),
            output: Arc::downgrade(&self.identity),
            source: self.authority.clone(),
            position: utf16,
            affinity,
            side,
            right,
            forward: None,
            category: None,
            visual: Some(visual),
            cluster: None,
            candidate: None,
            last: None,
            last_fonts: std::array::from_fn(|_| None),
            candidate_fonts: std::array::from_fn(|_| None),
            answer: None,
            failed: false,
        })
    }

    /// Advance either original directory work or classification of one original
    /// cluster per turn. Appending a same-writer prefix rebuilds only unfinished
    /// weak directory lookup, preserving accepted caret/classification progress.
    pub fn step_visual_word(
        &mut self,
        query: &mut MarkdownComposedVisualWordQuery,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedVisualWordStep> {
        let result = self.advance_visual_word(query, budget);
        if result.is_err() {
            query.failed = true;
        }
        if matches!(result, Err(Error::InvalidRequest)) {
            // Invalid caller input ends this query without destroying a healthy
            // original output required by other editing operations.
            self.require(&query.source)?;
            return result;
        }
        self.finish(result)
    }

    /// Original Source, writer, fonts, and cancellation outrank cached progress.
    /// Another width/style pipeline for the same text cannot resume this path.
    fn advance_visual_word(
        &mut self,
        query: &mut MarkdownComposedVisualWordQuery,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedVisualWordStep> {
        self.require(&query.source)?;
        if query.failed {
            return Err(Error::Poisoned);
        }
        if !Weak::ptr_eq(&query.lineage, &self.lineage) {
            // Only the original append-only writer can carry classification
            // progress into another actual sealed prefix.
            return Err(Error::SourceMismatch);
        }
        query.require_fonts(&self.payload.as_ref().ok_or(Error::Poisoned)?.fonts)?;
        if budget.bytes == 0
            || budget.scalars == 0
            || budget.records == 0
            || budget.metadata_units == 0
        {
            return Err(Error::InvalidRequest);
        }
        let budget = MarkdownLineComposeBudget {
            bytes: budget.bytes.min(4096),
            scalars: budget.scalars.min(256),
            records: budget.records.min(256),
            metadata_units: budget.metadata_units.min(256),
        };
        if !Weak::ptr_eq(&query.output, &Arc::downgrade(&self.identity)) {
            query.visual = None;
            query.output = Arc::downgrade(&self.identity);
        } // Accepted caret/category facts keep same-writer authority; old Part tickets do not transfer.
        let mut step = MarkdownComposedVisualWordStep {
            bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            answer: MarkdownComposedVisualWordAnswer::Pending,
        };
        if let Some(answer) = &query.answer {
            // Repeated final answers still pass original Source/font checks.
            step.answer = answer.clone();
        } else if let Some(cluster) = &mut query.cluster {
            // Classify the actual single cluster under the caller's byte/scalar
            // quantum, without borrowing a whole word or paragraph.
            let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
            let mut actual = MarkdownWordDeleteStep {
                bytes: 0,
                scalars: 0,
                records: 0,
                metadata_units: 0,
                answer: MarkdownWordDeleteAnswer::Yielded,
            };
            let complete = cluster.read(source, budget, &mut actual)?;
            step.bytes = actual.bytes;
            step.scalars = actual.scalars;
            if complete {
                let category = cluster.category();
                query.cluster = None;
                query.accept(category)?;
            } // Partial UTF-8 survives one-byte quanta without issuing a category or candidate caret early.
        } else {
            if query.visual.is_none() {
                // Rebuild only unfinished visual lookup on this authenticated
                // same-writer prefix, retaining the action's original flow.
                query.visual = Some(self.visual_query(
                    query.position,
                    query.affinity,
                    query.side,
                    query.right,
                )?);
                query
                    .visual
                    .as_mut()
                    .expect("刚创建原视觉查询")
                    .flow_forward = query.forward;
            }
            let visual = query.visual.as_mut().expect("原视觉词组只保一个相邻查询");
            let actual = self.step_visual(visual, budget.records.min(budget.metadata_units))?;
            query.forward = visual.flow_forward;
            step.records = actual.records;
            step.metadata_units = actual.records;
            if let Some(hidden) = &visual.crossed_fold {
                // Hidden content needs explicit original fold expansion before
                // a word classification may inspect or traverse its source.
                query.candidate_fonts = visual.word_registrations();
                query.answer = Some(MarkdownComposedVisualWordAnswer::NeedsFoldExpansion(
                    hidden.clone(),
                ));
            } else {
                match actual.answer {
                    MarkdownComposedVisualAnswer::Pending => {}
                    MarkdownComposedVisualAnswer::PrefixNotCovered => {
                        // Unprepared prefix is neither EOF nor a completed word.
                        step.answer = MarkdownComposedVisualWordAnswer::PrefixNotCovered;
                    }
                    found @ MarkdownComposedVisualAnswer::Found { .. } => {
                        query.candidate_fonts = visual.word_registrations();
                        let hard_break = visual.crossed_break;
                        let pair = visual
                            .gcb
                            .as_ref()
                            .and_then(|gcb| gcb.origin.zip(gcb.neighbor));
                        query.candidate = Some(found);
                        query.visual = None;
                        if hard_break {
                            // Actual LF/CR/CRLF/LS/PS paragraph controls all belong
                            // to the established ECMAScript whitespace category.
                            query.accept(Category::Space)?;
                        } else if let Some(((_, origin), (ordinal, neighbor))) = pair {
                            // Only actual original neighboring GCB records may
                            // establish the single cluster to classify next.
                            query.cluster = Some(Cluster::new(ordinal, origin, neighbor)?);
                        } else {
                            let terminal =
                                query.candidate.take().ok_or(Error::InconsistentCoverage)?;
                            query.last_fonts = std::mem::take(&mut query.candidate_fonts);
                            query.answer = Some(word_answer(terminal)?);
                        } // A Found without actual GCB/control traversal is true BOF/EOF, not a guessed text span.
                    }
                }
            }
        }
        self.require(&query.source)?;
        query.require_fonts(&self.payload.as_ref().ok_or(Error::Poisoned)?.fonts)?;
        if let Some(answer) = &query.answer {
            step.answer = answer.clone();
        }
        Ok(step)
    }
}

impl MarkdownComposedVisualWordQuery {
    /// Recheck original weak font registrations across classification suspension
    /// and repeated answers. Reclaimed slots cannot authorize old visual order.
    fn require_fonts(&self, catalog: &MarkdownLineFontCatalog) -> Result<()> {
        for registration in self
            .last_fonts
            .iter()
            .chain(&self.candidate_fonts)
            .flatten()
        {
            catalog.require_registration(registration)?;
        }
        Ok(())
    }

    /// Preserve the established three categories and leading-space policy. On
    /// stop, return the last accepted actual visual caret, never logical min/max.
    fn accept(&mut self, category: Category) -> Result<()> {
        let candidate = self.candidate.take().ok_or(Error::InconsistentCoverage)?;
        if self
            .category
            .is_some_and(|old| old != Category::Space && old != category)
        {
            // A category change after non-space text completes the previous
            // accepted word; the new candidate belongs to the next action.
            let previous = self.last.take().ok_or(Error::InconsistentCoverage)?;
            self.answer = Some(word_answer(previous)?);
            self.candidate_fonts = std::array::from_fn(|_| None);
            return Ok(());
        }
        let MarkdownComposedVisualAnswer::Found {
            utf16, side, line, ..
        } = &candidate
        else {
            // Pending/uncovered states cannot enter accepted word progress.
            return Err(Error::InconsistentCoverage);
        };
        self.position = *utf16;
        self.side = Some(*side);
        self.affinity = if !line.is_empty() && *utf16 == line.end && *utf16 != 0 {
            // A real line-end caret retains upstream soft-wrap association.
            MarkdownComposedLineAffinity::Upstream
        } else {
            // Other actual positions retain downstream line association.
            MarkdownComposedLineAffinity::Downstream
        };
        self.category = Some(category);
        self.last = Some(candidate);
        self.last_fonts = std::mem::take(&mut self.candidate_fonts);
        Ok(())
    }
}

/// Transfer only an original Found position, actual side, and line identity.
/// Pending or uncovered prefix cannot be packaged as a completed word endpoint.
fn word_answer(found: MarkdownComposedVisualAnswer) -> Result<MarkdownComposedVisualWordAnswer> {
    match found {
        MarkdownComposedVisualAnswer::Found {
            utf16,
            side,
            line,
            ordinal,
        } => Ok(MarkdownComposedVisualWordAnswer::Found {
            utf16,
            side,
            line,
            ordinal,
        }),
        MarkdownComposedVisualAnswer::Pending | MarkdownComposedVisualAnswer::PrefixNotCovered => {
            Err(Error::InconsistentCoverage)
        }
    }
}
