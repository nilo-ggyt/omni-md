//! Incrementally convert four actual physical-line byte boundaries to absolute UTF-16, without copying text or reclassifying blocks.

use super::*;

const MIN_STEP_BUDGET: usize = 2048;
const MAX_STEP_BUDGET: usize = 65536;
const CONTROL_WORK: usize = 64;

/// Retain four coordinates and partial-scalar state; the outer source request owns identity, cancellation, and cumulative fuel.
pub(super) struct Coordinates {
    boundaries: [usize; 4],
    values: [usize; 4],
    boundary_index: usize,
    next: usize,
    utf16: usize,
    scalar_remaining: u8,
    source_byte_len: usize,
}

impl Coordinates {
    /// Accept physical start, content start, content end, and EOL end from an actual source ticket, in that order.
    pub(super) fn new(
        document: &MarkdownDocumentBuffer,
        boundaries: [usize; 4],
        budget: usize,
    ) -> Result<(Self, SemanticWork), MarkdownTableRequestError> {
        validate_budget(budget)?;
        if boundaries.windows(2).any(|pair| pair[0] > pair[1])
            || boundaries[3] > document.byte_len()
        {
            // Reject conflicting order or source extent rather than repairing the ticket into another apparent line.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let tree = tree_height(document);
        let work = SemanticWork {
            source_tree_steps: tree,
            structural_transitions: tree
                .checked_add(CONTROL_WORK)
                .ok_or(MarkdownTableRequestError::WorkLimit)?,
            metadata_bytes: coordinate_bytes()?
                .checked_mul(2)
                .ok_or(MarkdownTableRequestError::WorkLimit)?,
            ..SemanticWork::default()
        };
        if work_cost(work)? > budget {
            // Admit the AVL path and fixed boundary validation first; a small budget cannot hide an unbounded traversal.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut node = document
            .root
            .as_deref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let mut local = boundaries[0];
        let mut before_utf16 = 0usize;
        // Complete left subtrees contribute validated cached metrics; advance reads the target leaf's prefix with bounded frames.
        while let MarkdownPieceTreeNode::Branch { left, right, .. } = node {
            let left_metrics = left.metrics();
            if local < left_metrics.byte_len {
                // The target is in the left subtree, so the right side contributes neither text nor prefix metrics.
                node = left;
            } else {
                // Enter the right subtree without decoding the complete left side; checked arithmetic rejects inconsistent metrics.
                local = local
                    .checked_sub(left_metrics.byte_len)
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                before_utf16 = before_utf16
                    .checked_add(left_metrics.utf16_len)
                    .ok_or(MarkdownTableRequestError::WorkLimit)?;
                node = right;
            }
        }
        if local > node.metrics().byte_len || before_utf16 > document.utf16_len() {
            // An out-of-leaf target or oversized cached prefix is an internal source mismatch, not a publishable approximation.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if let MarkdownPieceTreeNode::OriginalSpan(span) = node {
            // The shared directory identifies the real subpiece start; add its validated prefix without scanning span text.
            let hit = span
                .locate_byte(local, true)
                .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
            local = hit.local;
            before_utf16 = before_utf16
                .checked_add(hit.prefix.utf16_len)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
        }
        let next = boundaries[0]
            .checked_sub(local)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        // Return completed location/validation work immediately, so cancellation or Drop after construction cannot omit it.
        Ok((
            Self {
                boundaries,
                values: [0; 4],
                boundary_index: 0,
                next,
                utf16: before_utf16,
                scalar_remaining: 0,
                source_byte_len: document.byte_len(),
            },
            work,
        ))
    }

    /// Reuse the previous actual endpoint; advance still reads intervening lines or gaps instead of recounting from the leaf start.
    pub(super) fn continue_at(
        &mut self,
        boundaries: [usize; 4],
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if self.boundary_index != self.boundaries.len()
            || self.scalar_remaining != 0
            || self.next != self.boundaries[3]
        {
            // Complete or discard the current group through its parent before replacing its unpublished coordinates.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if boundaries.windows(2).any(|pair| pair[0] > pair[1])
            || boundaries[0] < self.next
            || boundaries[3] > self.source_byte_len
        {
            // Continue forward on the original source; backward, overlapping, or out-of-source targets require a new trusted location.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let metadata_bytes = coordinate_bytes()?
            .checked_mul(2)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let work = SemanticWork {
            structural_transitions: CONTROL_WORK,
            metadata_bytes,
            ..SemanticWork::default()
        };
        // Replace only the four boundaries/result slots; preserve the actual byte/UTF-16/scalar frontier without another tree lookup.
        self.boundaries = boundaries;
        self.values = [0; 4];
        self.boundary_index = 0;
        // Report this fixed update now rather than deferring already completed work until the next source read.
        Ok(work)
    }

    /// Borrow at most one source frame per call; charge this conversion's rereads separately from the origin scanner's work.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(SemanticWork, Option<[usize; 4]>), MarkdownTableRequestError> {
        validate_budget(budget)?;
        // Report only this call's reads/control; construction and reuse already returned their own work.
        let mut work = SemanticWork {
            structural_transitions: CONTROL_WORK,
            ..SemanticWork::default()
        };
        self.record_boundaries(&mut work)?;
        if self.boundary_index == self.boundaries.len() {
            // Empty segments or an already reached endpoint need no extra text lease merely to publish coordinates.
            return self.finish(work, budget);
        }
        let tree = tree_height(document);
        let remaining_records = self
            .boundaries
            .len()
            .checked_sub(self.boundary_index)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let output_bytes = coordinate_bytes()?;
        let reserve = remaining_records
            .checked_mul(boundary_record_bytes()?)
            .and_then(|bytes| bytes.checked_add(output_bytes))
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let available = budget
            .checked_sub(work_cost(work)?)
            .and_then(|left| left.checked_sub(tree))
            .and_then(|left| left.checked_sub(reserve));
        let Some(available) = available.filter(|left| *left >= 3) else {
            // Return completed fixed work and retain the same byte frontier; an empty lease would not prove progress.
            return Ok((work, None));
        };
        let unread = self.boundaries[3]
            .checked_sub(self.next)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let wanted = (available / 3).min(unread).min(MAX_STEP_BUDGET);
        if wanted == 0 {
            // Unpublished boundaries with no unread bytes indicate inconsistent cursor state.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // tree_height bounds the source lookup path conservatively; the separate tree field is not an exact CPU instruction count.
        work.source_tree_steps = work
            .source_tree_steps
            .checked_add(tree)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        work.structural_transitions = work
            .structural_transitions
            .checked_add(tree)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let frame = document
            .table_source_frame(self.next, wanted)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let frame_end = frame
            .byte_start
            .checked_add(frame.bytes.len())
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        if frame.byte_start != self.next
            || frame.bytes.is_empty()
            || frame.bytes.len() > wanted
            || frame_end > document.byte_len()
            || frame.real_eof != (frame_end == document.byte_len())
        {
            // Accept only the requested contiguous source frame; a short leaf may pause, but mismatched frames or fabricated EOF cannot continue.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        for byte in frame.bytes.iter() {
            // Charge each source byte and two decode/advance units; boundary metadata and publication space were reserved above.
            self.consume_byte(*byte)?;
            work.source_bytes = work
                .source_bytes
                .checked_add(1)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            work.structural_transitions = work
                .structural_transitions
                .checked_add(2)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            self.record_boundaries(&mut work)?;
            if self.boundary_index == self.boundaries.len() {
                // Stop at the actual EOL end even if the borrowed frame contains more bytes beyond this line.
                frame
                    .finish()
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return self.finish(work, budget);
            }
        }
        if work_cost(work)? > budget {
            // Admission and measured work must agree; reject overspend rather than truncating its report or publishing a partial line.
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        Ok((work, None))
    }

    fn record_boundaries(
        &mut self,
        work: &mut SemanticWork,
    ) -> Result<(), MarkdownTableRequestError> {
        // Record only four actual source boundaries; empty adjacent segments may share the same byte position.
        while let Some(boundary) = self.boundaries.get(self.boundary_index).copied() {
            if boundary < self.next {
                // Never approximate an already passed boundary with a later UTF-16 position.
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if boundary > self.next {
                // Reach a future boundary through bounded decoding; a byte difference alone cannot determine UTF-16 length.
                return Ok(());
            }
            if self.scalar_remaining != 0 {
                // Tickets promise scalar boundaries; a position inside UTF-8 continuation bytes is an internal mismatch.
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.values[self.boundary_index] = self.utf16;
            self.boundary_index = self
                .boundary_index
                .checked_add(1)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            work.metadata_bytes = work
                .metadata_bytes
                .checked_add(boundary_record_bytes()?)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
        }
        Ok(())
    }

    fn consume_byte(&mut self, byte: u8) -> Result<(), MarkdownTableRequestError> {
        let (units, remaining) = match (self.scalar_remaining, byte) {
            // ASCII, including CR and LF separately, contributes one original UTF-16 unit; do not collapse CRLF.
            (0, 0x00..=0x7f) => (1usize, 0),
            // The snapshot already validates UTF-8; count a scalar's UTF-16 units at its leading byte, without recounting continuations.
            (0, 0xc2..=0xdf) => (1, 1),
            (0, 0xe0..=0xef) => (1, 2),
            (0, 0xf0..=0xf4) => (2, 3),
            // A frame may end within a scalar; retain at most a three-byte continuation count rather than a text copy.
            (1..=3, 0x80..=0xbf) => (
                0,
                self.scalar_remaining
                    .checked_sub(1)
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
            ),
            // This checks consistency under the validated-snapshot contract; it is not an independent UTF-8 validator.
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        };
        self.utf16 = self
            .utf16
            .checked_add(units)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.next = self
            .next
            .checked_add(1)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.scalar_remaining = remaining;
        Ok(())
    }

    fn finish(
        &self,
        mut work: SemanticWork,
        budget: usize,
    ) -> Result<(SemanticWork, Option<[usize; 4]>), MarkdownTableRequestError> {
        work.metadata_bytes = work
            .metadata_bytes
            .checked_add(coordinate_bytes()?)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        if work_cost(work)? > budget {
            // Returning four coordinates is metadata work too; do not publish it without enough step fuel.
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        Ok((work, Some(self.values)))
    }
}

fn validate_budget(budget: usize) -> Result<(), MarkdownTableRequestError> {
    if !(MIN_STEP_BUDGET..=MAX_STEP_BUDGET).contains(&budget) {
        // The public source entry still requires 4096; this child accepts the finite remainder after the parent reserves handoff work.
        return Err(MarkdownTableRequestError::InvalidRequest);
    }
    Ok(())
}

fn coordinate_bytes() -> Result<usize, MarkdownTableRequestError> {
    std::mem::size_of::<usize>()
        .checked_mul(4)
        .ok_or(MarkdownTableRequestError::WorkLimit)
}

fn boundary_record_bytes() -> Result<usize, MarkdownTableRequestError> {
    // Each record reads one byte boundary and writes one UTF-16 coordinate; this measures no allocated text capacity.
    std::mem::size_of::<usize>()
        .checked_mul(2)
        .ok_or(MarkdownTableRequestError::WorkLimit)
}

fn work_cost(work: SemanticWork) -> Result<usize, MarkdownTableRequestError> {
    // structural_transitions already includes source_tree_steps; retain the separate observation without charging that path twice.
    work.source_bytes
        .checked_add(work.structural_transitions)
        .and_then(|cost| cost.checked_add(work.metadata_bytes))
        .ok_or(MarkdownTableRequestError::WorkLimit)
}
