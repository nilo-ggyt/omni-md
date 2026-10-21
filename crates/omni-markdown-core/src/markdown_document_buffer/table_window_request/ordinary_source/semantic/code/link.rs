//! Decode bounded destinations and complete label coordinates from the original Link proof, without
//! copied labels/paragraphs or another parser.

use super::*;
use crate::MarkdownLinkDestination;
use pulldown_cmark::global_frontier::{
    InlineLinkDestinationDecoder, InlineLinkKind, InlineLinkTarget, VerifiedInlineLink,
};

const OUTPUT_BYTES: usize = 1024;
// The existing 16-KiB destination limit permits at most 16 page handles. Reserve actual directory-move
// work without another destination-text budget.
const DIRECTORY_COPY_RESERVE: usize =
    crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_REFERENCE_BYTES
        .div_ceil(OUTPUT_BYTES)
        * std::mem::size_of::<Vec<u8>>();

enum Phase {
    Destination,
    Reference,
    Coordinates,
}

/// One actual link retains its original proof, one bounded destination-page directory, and four
/// endpoint cursors, transferring ownership on completion.
/// Labels may span many frames; neither a label nor the entire owner containing URL/title is loaded
/// into RAM.
pub(super) struct LinkJob {
    proof: VerifiedInlineLink,
    decoder: Option<InlineLinkDestinationDecoder>,
    reference: Option<ReferenceLabel>,
    shared_destination: Option<MarkdownLinkDestination>,
    destination: Vec<Vec<u8>>,
    destination_len: usize,
    maximum_destination_bytes: usize,
    coordinates: Option<Coordinates>,
    coordinate_index: usize,
    projected: [usize; 4],
    phase: Phase,
}

pub(super) struct LinkProjection {
    pub(super) span: VisibleSpan,
    pub(super) link: Option<MarkdownSourceLink>,
}

impl LinkJob {
    /// Select destination, reference, or coordinate work from the original verified link kind.
    pub(super) fn new(proof: VerifiedInlineLink, maximum_destination_bytes: usize) -> Self {
        let mut reference = None;
        let decoder = proof.destination_decoder();
        let phase = if proof.kind() == InlineLinkKind::Image {
            Phase::Coordinates // Images contribute style/marker facts only, without acquiring click authority or copying their URI.
        } else {
            match proof.target() {
                InlineLinkTarget::Direct { destination, .. } if destination.is_empty() => {
                    Phase::Coordinates
                }
                InlineLinkTarget::Direct { .. } => Phase::Destination,
                InlineLinkTarget::Reference { request } => {
                    reference = Some(ReferenceLabel::new(request));
                    Phase::Reference
                }
            }
        };
        Self {
            decoder,
            proof,
            reference,
            shared_destination: None,
            destination: Vec::new(),
            destination_len: 0,
            maximum_destination_bytes,
            coordinates: None,
            coordinate_index: 0,
            projected: [0; 4],
            phase,
        }
    }

    /// Consume one small destination frame or original-coordinate step; the decoder shares unescaping
    /// with the full parser.
    /// Account actual reads, copies, and capacity without allocating 16 KiB eagerly or copying the
    /// final destination again.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        selection: &Range<usize>,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
        queries: Option<&MarkdownTableReferenceQueries>,
    ) -> Result<(SemanticWork, Option<LinkProjection>), MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        if self.proof.source_revision() != document.generation()
            || self.proof.source_end() != document.byte_len()
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        } // The parent seed/cursor issued this proof; edits or undo generations cannot continue destination copying under old coordinates.
        match self.phase {
            Phase::Destination => self.advance_destination(document, quota, budget),
            Phase::Reference => {
                let reference = self.reference.as_mut().unwrap();
                if reference.label().is_none() {
                    return reference
                        .advance(document, quota, budget)
                        .map(|work| (work, None));
                }
                let label = reference.label().unwrap();
                let destination = queries
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?
                    .resolved_destination(label)
                    .map_err(MarkdownTableRequestError::Window)?
                    .ok_or(MarkdownTableRequestError::Window(
                        MarkdownTableWindowError::ParserMismatch,
                    ))?;
                if destination.byte_len() > self.maximum_destination_bytes {
                    return Err(MarkdownTableRequestError::Window(
                        MarkdownTableWindowError::ReferenceLimit,
                    ));
                }
                let work = SemanticWork {
                    structural_transitions: PLAIN_CONTROL,
                    normalizer_input_bytes: label.len(),
                    ..Default::default()
                };
                self.shared_destination = Some(destination);
                self.reference = None;
                self.phase = Phase::Coordinates;
                Ok((work, None))
            }
            Phase::Coordinates => {
                if let Some(coordinates) = self.coordinates.as_mut() {
                    let (work, values) = coordinates.advance(document, budget)?;
                    let Some(values) = values else {
                        return Ok((work, None));
                    };
                    self.projected[self.coordinate_index] = values[0];
                    self.coordinate_index += 1;
                    self.coordinates = None;
                    if self.coordinate_index != self.projected.len() {
                        return Ok((work, None));
                    }
                    let values = self.projected;
                    let label = values[1]..values[2];
                    let mut work = work;
                    let link = if label.is_empty() || self.proof.kind() == InlineLinkKind::Image {
                        None
                    } else if let Some(destination) = self.shared_destination.take() {
                        Some(MarkdownSourceLink {
                            label_utf16_range: label.clone(),
                            destination,
                        })
                    } else {
                        let metadata = MarkdownLinkDestination::paged_metadata();
                        quota
                            .admit_metadata(metadata)
                            .map_err(MarkdownTableRequestError::Window)?;
                        work.metadata_bytes += metadata;
                        Some(MarkdownSourceLink {
                            label_utf16_range: label.clone(),
                            destination: MarkdownLinkDestination::from_chunks(std::mem::take(
                                &mut self.destination,
                            )),
                        })
                    };
                    // The original Link owner determines marker activity and its complete label
                    // determines click authority. The visible window only
                    // bounds parent run inventory; it cannot truncate a long label into another link or
                    // substitute destination bytes as the label.
                    return Ok((
                        work,
                        Some(LinkProjection {
                            span: VisibleSpan {
                                whole: values[0]..values[3],
                                body: label.clone(),
                                active: markdown_selection_intersects(
                                    selection,
                                    &self.proof.source_range(),
                                ),
                                styles: MarkdownPresentationStyles::LINK,
                            },
                            link,
                        }),
                    ));
                }
                let owner = self.proof.source_range();
                let label = self.proof.label_range();
                let boundary =
                    [owner.start, label.start, label.end, owner.end][self.coordinate_index];
                // Locate each of the four actual endpoints through the original tree's bounded
                // directory. Never reread an entire long
                // label from owner.start merely to convert label.end to UTF-16. One cursor returns each
                // step's actual read work.
                let (coordinates, work) = Coordinates::new(document, [boundary; 4], budget)?;
                self.coordinates = Some(coordinates);
                Ok((work, None))
            }
        }
    }

    /// The decoder produces complete UTF-8 output frames; Core validates each frame and extends one
    /// destination without another escaping algorithm.
    /// The decoder borrows original same-Source frames; return the lease before consuming output so
    /// actual Source failure retains precedence.
    fn advance_destination(
        &mut self,
        document: &MarkdownDocumentBuffer,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<(SemanticWork, Option<LinkProjection>), MarkdownTableRequestError> {
        let tree = tree_height(document);
        // Reserve this quantum's actual copying, UTF-8 validation, page/directory capacity, and
        // parent-control work without acquiring another quantum.
        let child =
            budget.saturating_sub(tree + PLAIN_CONTROL + OUTPUT_BYTES + DIRECTORY_COPY_RESERVE);
        if child < 2048 {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let next = self.decoder.as_ref().unwrap().next_byte();
        let maximum = self
            .proof
            .destination_range()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?
            .end
            .checked_sub(next)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?
            .min(OUTPUT_BYTES)
            .min(child);
        let frame = if maximum == 0 {
            None // The decoder may drain previously read carry at destination end; without a new read, invent neither a Source frame nor tree cost.
        } else {
            Some(
                document
                    .table_source_frame(next, maximum)
                    .map_err(MarkdownTableRequestError::Source)?,
            )
        };
        let mut output = [0; OUTPUT_BYTES];
        let source_bytes = frame.as_ref().map_or(0, |frame| frame.bytes.len());
        let step = self.decoder.as_mut().unwrap().advance(
            SourceFrame {
                source_revision: document.generation(),
                byte_start: next,
                bytes: frame.as_ref().map_or(&[][..], |frame| &frame.bytes),
                real_eof: frame
                    .as_ref()
                    .map_or(next == document.byte_len(), |frame| frame.real_eof),
            },
            GlobalSourceAuthority {
                current_revision: document.generation(),
                cancelled: false,
            },
            &mut output,
            child,
        );
        let tree = if let Some(frame) = frame {
            frame.finish().map_err(MarkdownTableRequestError::Source)?;
            tree
        } else {
            0
        };
        let step = step.map_err(MarkdownTableRequestError::Global)?;
        let bytes = output
            .get(..step.written)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let text =
            std::str::from_utf8(bytes).map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        if self.destination_len.saturating_add(text.len()) > self.maximum_destination_bytes {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::ReferenceLimit,
            )); // Keep original batch destination admission; an oversized actual target cannot be skipped in favor of another link.
        }
        let tail = self
            .destination
            .last()
            .map_or(0, |chunk| chunk.capacity() - chunk.len());
        let new_chunk = text.len() > tail;
        let new_directory = new_chunk && self.destination.len() == self.destination.capacity();
        // The final output page holds only its remaining destination; incomplete output retains a fixed
        // small page filled within its existing capacity.
        // Short URIs retain no unnecessary 1-KiB tail, without reallocating already accumulated
        // destination bytes to save capacity.
        let chunk_bytes = if !new_chunk {
            0
        } else if step.complete {
            text.len() - tail
        } else {
            OUTPUT_BYTES
        };
        let directory_bytes = usize::from(new_directory)
            * (self.destination.len() + 1)
            * std::mem::size_of::<Vec<u8>>();
        let reserved = chunk_bytes + directory_bytes;
        quota
            .admit_metadata(reserved)
            .map_err(MarkdownTableRequestError::Window)?;
        let mut directory_allocated = 0;
        let mut directory_moved = 0;
        if new_directory {
            // Move existing page handles into the new small directory explicitly so accounting charges
            // actual logical moves, without guessing whether allocator
            // reserve would grow in place. Both directories coexist briefly; quota charges full new
            // capacity, and the old directory
            // is returned after transfer. Cumulative allocation accounting is not a claim about
            // currently retained memory.
            let mut directory = Vec::with_capacity(self.destination.len() + 1);
            directory_allocated = directory.capacity() * std::mem::size_of::<Vec<u8>>();
            directory_moved = self.destination.len() * std::mem::size_of::<Vec<u8>>();
            directory.append(&mut self.destination);
            self.destination = directory;
        }
        let mut copied = 0;
        if let Some(chunk) = self.destination.last_mut() {
            copied = text.len().min(tail);
            chunk.extend_from_slice(&text.as_bytes()[..copied]);
        }
        let mut allocated = 0;
        if copied < text.len() {
            let mut chunk = Vec::with_capacity(chunk_bytes);
            allocated = chunk.capacity();
            chunk.extend_from_slice(&text.as_bytes()[copied..]);
            self.destination.push(chunk);
        }
        if allocated + directory_allocated > reserved {
            quota
                .admit_metadata(allocated + directory_allocated - reserved)
                .map_err(MarkdownTableRequestError::Window)?;
        }
        self.destination_len += text.len();
        // A full page adds one page without reallocating accumulated URL bytes; the at-most-16-entry
        // directory move is already budgeted.
        // Storage-page boundaries may split scalars while preserving complete decoder-output byte
        // order; only the private constructor seals the result.
        if step.complete {
            self.phase = Phase::Coordinates;
        }
        Ok((
            SemanticWork {
                // Entity lookup or full output may consume only a frame prefix; charge the actual
                // borrowed frame, including later rereads of its remaining tail.
                source_bytes,
                structural_transitions: step.work.transitions
                    + tree
                    + PLAIN_CONTROL
                    + text.len()
                    + directory_moved,
                source_tree_steps: tree,
                metadata_bytes: allocated + directory_allocated,
                allocated_source_capacity: allocated,
                copied_source_bytes: text.len(),
                utf8_validation_bytes: text.len(),
                normalizer_input_bytes: step.work.byte_reads,
                ..Default::default()
            },
            None,
        ))
    }
}
