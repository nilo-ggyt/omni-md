//! Replay one table from actual accepted row boundaries. Retain structural
//! checkpoints only, without rebuilding a Global frontier or storing source text.

use super::*;
use pulldown_cmark::block_frontier::{TableContinuationState, TableStep};

#[derive(Clone, Debug)]
/// The checkpoint comes from the same accepted row boundary and owns its opaque
/// physical line number. Core adds only the actual fact's next ordinal/byte/UTF-16
/// coordinates; window-local indices and EOF tails cannot derive new boundaries.
pub(super) struct Boundary {
    pub(super) checkpoint: TableCheckpoint,
    pub(super) next_row_ordinal: usize,
    pub(super) next_byte: usize,
    pub(super) next_utf16: usize,
}

impl Boundary {
    /// After actual Global acceptance of header/delimiter, save the first body
    /// entrance without rereading the header for a disabled display extension.
    pub(super) fn after_delimiter(
        checkpoint: TableCheckpoint,
        header: &GlobalTableFact,
        delimiter_utf16_end: usize,
    ) -> Self {
        Self {
            checkpoint,
            next_row_ordinal: 1,
            next_byte: header.separator_byte_end,
            next_utf16: delimiter_utf16_end,
        }
    }

    /// Row fact and checkpoint come from the same structural acceptance. Derive
    /// their next coordinates without another replay of the row's text.
    pub(super) fn after_row(checkpoint: TableCheckpoint, row: TableRowFact) -> Self {
        Self {
            checkpoint,
            next_row_ordinal: row.row_ordinal + 1,
            next_byte: row.byte_end,
            next_utf16: row.utf16_end,
        }
    }
}

#[derive(Clone, Debug)]
/// Three bounded anchors share original spine pages and fuel. They retain no
/// document root, native object, source text, or directory of all past rows.
pub(super) struct ReplayAnchors {
    pub(super) header: GlobalTableFact,
    pub(super) header_utf16: Range<usize>,
    pub(super) delimiter_utf16: Range<usize>,
    pub(super) base: Boundary,
    pub(super) chunk: Boundary,
    pub(super) halo: Option<Boundary>,
}

#[derive(Debug)]
pub(super) enum ReplayOutcome {
    // The parent batch already admitted RequiredState and its fixed 4096-byte
    // workspace. This allocation transfers one bounded receipt, not a full table copy.
    Window(Box<MarkdownTableStructuralWindow>),
    End {
        end: VerifiedTableEnd,
        terminal: Box<MarkdownTableStructuralWindow>,
    },
}

/// This temporary continuation replays one verified table, without taking over or
/// removing the parent batch's still-owned primary Global when replay finishes.
pub(super) struct ReplayRequest {
    source: MarkdownTableSourceIdentity,
    anchors: ReplayAnchors,
    target: usize,
    boundary: Boundary,
    previous: Option<Boundary>,
    table: Option<TableContinuationState>,
    // Retain only the last complete row and its actual predecessor boundary, so
    // a missing target can produce an actual terminal window rather than an empty row.
    last_completed: Option<(TableRowFact, Boundary, Option<Boundary>)>,
    consumed: bool,
    failed: Option<MarkdownTableRequestError>,
}

impl ReplayRequest {
    pub(super) fn new(
        source: MarkdownTableSourceIdentity,
        anchors: ReplayAnchors,
        target: usize,
    ) -> Result<Self, MarkdownTableRequestError> {
        validate_columns(anchors.header.columns)?;
        if anchors.header.header_byte_start >= anchors.header.header_byte_end
            || anchors.header.header_byte_end != anchors.header.separator_byte_start
            || anchors.header.separator_byte_start >= anchors.header.separator_byte_end
            || anchors.header.separator_byte_end > source.byte_len()
            || anchors.header_utf16.start >= anchors.header_utf16.end
            || anchors.header_utf16.end != anchors.delimiter_utf16.start
            || anchors.delimiter_utf16.start >= anchors.delimiter_utf16.end
            || anchors.base.next_row_ordinal != 1
            || anchors.base.next_byte != anchors.header.separator_byte_end
            || anchors.base.next_utf16 != anchors.delimiter_utf16.end
        {
            // Only the actual structural path constructs these fields. Any
            // disagreement rejects replay rather than repairing a guessed seed.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut selected = None;
        for candidate in std::iter::once(&anchors.base)
            .chain(std::iter::once(&anchors.chunk))
            .chain(anchors.halo.iter())
        {
            if candidate.next_row_ordinal == 0
                || candidate.next_byte > source.byte_len()
                || candidate.next_byte < anchors.base.next_byte
                || candidate.next_utf16 < anchors.base.next_utf16
            {
                // All three anchors belong to this table's accepted prefix. An
                // unselected anchor cannot quietly carry backward coordinates either.
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if candidate.next_row_ordinal <= target
                && selected.as_ref().is_none_or(|previous: &&Boundary| {
                    previous.next_row_ordinal < candidate.next_row_ordinal
                })
            {
                // Select the nearest actual entrance before the target. Its
                // ordinal selects an existing checkpoint, never creates a Parser seed.
                selected = Some(candidate);
            }
        }
        let boundary = if target == 0 {
            // Header/delimiter already have independent structural evidence.
            // Header-only lookup does not need to read the first body row.
            anchors.base.clone()
        } else {
            selected
                .ok_or(MarkdownTableRequestError::CoverageRequiresCheckpoint)?
                .clone()
        };
        let previous = std::iter::once(&anchors.base)
            .chain(std::iter::once(&anchors.chunk))
            .chain(anchors.halo.iter())
            .find(|candidate| {
                candidate.next_row_ordinal.checked_add(1) == Some(boundary.next_row_ordinal)
            })
            .cloned();
        let table =
            (target != 0).then(|| TableContinuationState::resume(boundary.checkpoint.clone()));
        Ok(Self {
            source,
            anchors,
            target,
            boundary,
            previous,
            table,
            last_completed: None,
            consumed: false,
            failed: None,
        })
    }

    /// The outer batch owns cancellation and cumulative lifetime fuel. Every
    /// entry here still checks the original root and runs one bounded child stage.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(semantic::SemanticWork, Option<ReplayOutcome>), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            // First failure remains revoked. Another callback cannot retry the
            // same old checkpoint and manufacture renewed result authority.
            return Err(error);
        }
        let result = if !self.source.is_current(document) {
            Err(MarkdownTableRequestError::SourceSuperseded)
        } else if !(3072..=65536).contains(&budget) || self.consumed {
            Err(MarkdownTableRequestError::InvalidRequest)
        } else {
            self.advance_inner(document, budget)
        };
        if let Err(error) = result {
            // Drop this continuation and last-row metadata on failure. The caller
            // still owns primary Global; do not reconstruct it at source zero.
            self.failed = Some(error);
            self.last_completed = None;
            self.table = None;
        }
        result
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(semantic::SemanticWork, Option<ReplayOutcome>), MarkdownTableRequestError> {
        if self.target == 0 {
            // Header-only transfers existing actual metadata without classifying
            // body rows or mixing the context-only header into the body list.
            self.consumed = true;
            let ticket = self.window(None);
            return Ok((
                semantic::SemanticWork {
                    structural_transitions: 64,
                    ..semantic::SemanticWork::default()
                },
                Some(ReplayOutcome::Window(Box::new(ticket))),
            ));
        }
        let tree = tree_height(document);
        let child = child_budget(budget, tree)?;
        let table = self
            .table
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let frame = document
            .table_source_frame(table.next_byte(), child)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = table
            .advance_bounded(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                child,
                child,
            )
            .map_err(|error| {
                MarkdownTableRequestError::Global(GlobalFrontierError::Frontier(error))
            });
        // Return the actual frame before consuming the Parser result. Its early
        // completion cannot conceal a mapping/reclaim failure.
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = step?;
        let measured = table.last_work();
        let work = semantic::SemanticWork {
            source_bytes: measured.byte_reads,
            source_tree_steps: tree,
            structural_transitions: measured.transitions + tree + 64,
            ..semantic::SemanticWork::default()
        };
        match step {
            TableStep::Row { fact, .. } => {
                if fact.header_byte_start != self.anchors.header.header_byte_start
                    || fact.row_ordinal != self.boundary.next_row_ordinal
                    || fact.byte_start != self.boundary.next_byte
                    || fact.utf16_start != self.boundary.next_utf16
                {
                    // Reject another table or a noncontiguous replayed Row. Equal
                    // ordinal alone cannot replace this accepted coordinate identity.
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                let checkpoint = table
                    .checkpoint()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                // Original TableContinuation finished this row/container grammar.
                // Consume its same checkpoint directly, without rereading the text.
                if fact.row_ordinal == self.target {
                    self.anchors.chunk = self.boundary.clone();
                    self.anchors.halo = self.previous.take();
                    self.boundary = Boundary::after_row(checkpoint, fact);
                    self.consumed = true;
                    self.table = None;
                    return Ok((
                        work,
                        Some(ReplayOutcome::Window(Box::new(self.window(Some(fact))))),
                    ));
                }
                // A nontarget row replaces fixed metadata only. Actual last-row
                // and predecessor facts remain available for a terminal window.
                self.last_completed = Some((fact, self.boundary.clone(), self.previous.clone()));
                self.previous = Some(std::mem::replace(
                    &mut self.boundary,
                    Boundary::after_row(checkpoint, fact),
                ));
                Ok((work, None))
            }
            TableStep::Ended { .. } => {
                let end = table
                    .verified_end()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                if end.source_revision() != self.source.generation()
                    || end.header_byte_start() != self.anchors.header.header_byte_start
                    || end.byte_end() != self.boundary.next_byte
                    || end.utf16_end() != self.boundary.next_utf16
                {
                    // Deliver only this continuation's actual End. A missed
                    // target creates no empty window or assumed target activation.
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                // An out-of-table ordinal cannot authorize append. Keep the actual
                // final-row receipt for the same batch's semantics and shared quota.
                let mut terminal = if let Some((row, before, halo)) = self.last_completed.take() {
                    self.anchors.chunk = before;
                    self.anchors.halo = halo;
                    self.window(Some(row))
                } else if end.last_body_row_ordinal().is_none() {
                    // Header/delimiter-only still retains the actual header fact;
                    // an empty body projection cannot impersonate a synthetic row 1.
                    let mut window = self.window(None);
                    window.include_header = true;
                    window
                } else {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                };
                terminal.table_end = Some(end.clone());
                self.consumed = true;
                self.table = None;
                Ok((
                    work,
                    Some(ReplayOutcome::End {
                        end,
                        terminal: Box::new(terminal),
                    }),
                ))
            }
            TableStep::Progress { .. } | TableStep::NeedInput { .. } => {
                // Short frames and incomplete classification keep continuation
                // only; neither renews a checkpoint nor grants End authority.
                Ok((work, None))
            }
        }
    }

    fn window(&self, row: Option<TableRowFact>) -> MarkdownTableStructuralWindow {
        let mut body = Vec::with_capacity(usize::from(row.is_some()));
        if let Some(row) = row {
            // Materialize metadata for this one actual target row only. Header and
            // delimiter remain separate syntax context; omitted middle rows stay omitted.
            body.push(row);
        }
        MarkdownTableStructuralWindow {
            source: self.source.clone(),
            header: self.anchors.header,
            checkpoint: self.boundary.checkpoint.clone(),
            header_utf16: self.anchors.header_utf16.clone(),
            delimiter_utf16: self.anchors.delimiter_utf16.clone(),
            body,
            inline_rows: row
                .map(|_| {
                    self.boundary
                        .checkpoint
                        .last_inline_row()
                        .expect("目标原行已经真实接受，after-row checkpoint 保留同一行来源")
                })
                .into_iter()
                .collect(),
            include_header: self.target == 0,
            next_required_row: None,
            table_end: None,
            replay: Some(self.anchors.clone()),
        }
    }
}
