//! Actual composition-append extents establish retirement authority; keep file release receipts and copy at most two small candidate segments.

use super::*;

/// A frozen allocation owns one unique new segment index; a heap candidate owns at most two segments after its actual first append offset.
pub(in crate::markdown_document_buffer) enum CompositionAllocation {
    Frozen { segment: usize },
    Heap(HeapAllocation),
}

#[derive(Clone, Copy)]
/// Original history may reference the first segment's prefix; replacing all marked text must never overwrite bytes before this actual start.
pub(in crate::markdown_document_buffer) struct HeapAllocation {
    first: usize,
    byte_start: usize,
    end: usize,
}

/// The original input's sole candidate owns these finite new bytes; preparation neither updates the segment table nor creates another text Source.
pub(in crate::markdown_document_buffer) struct CompositionHeapCow {
    first: usize,
    segments: Vec<Arc<MarkdownEditSegment>>,
    retain_segments: usize,
    inserted: Option<PieceTreeLink>,
    allocation: Option<HeapAllocation>,
}

impl MarkdownEditSegments {
    /// Derive ownership from this actual append's leaves, not a first-segment/shared-prefix guess based on incoming text length.
    pub(in crate::markdown_document_buffer) fn append_composition(
        &mut self,
        text: &str,
        maximum_session_edit_bytes: usize,
    ) -> Result<
        (
            Vec<Arc<MarkdownPieceTreeNode>>,
            Option<CompositionAllocation>,
        ),
        MarkdownDocumentBufferError,
    > {
        if text.len() <= EDIT_SEGMENT_BYTES {
            let pieces = self.append(text, maximum_session_edit_bytes)?;
            let allocation = pieces.first().map(|first| {
                let MarkdownPieceTreeNode::Leaf(first) = first.as_ref() else {
                    unreachable!("同一次append直接产生有限叶，不返回聚合分支");
                };
                let MarkdownPieceSource::EditSegment(first_segment) = first.source else {
                    unreachable!("新增输入不引用Original源");
                };
                // At most 64 KiB of input occupies no more than two segments even from an old tail; the last actual appended leaf supplies its index.
                let MarkdownPieceTreeNode::Leaf(last) = pieces.last().unwrap().as_ref() else {
                    unreachable!("新增输入的末项同样是实际追加叶");
                };
                let MarkdownPieceSource::EditSegment(last_segment) = last.source else {
                    unreachable!("新增输入末项不引用Original源");
                };
                CompositionAllocation::Heap(HeapAllocation {
                    first: first_segment,
                    byte_start: first.byte_start,
                    end: last_segment + 1,
                })
            });
            return Ok((pieces, allocation));
        }
        let backing = self.freeze_input(text, maximum_session_edit_bytes)?;
        let segment = self.segments.len();
        let pieces = self.append_frozen(backing, maximum_session_edit_bytes)?;
        // append_frozen installs one exclusive index only after all bounded leases succeed; the original checkpoint rolls back failures.
        Ok((pieces, Some(CompositionAllocation::Frozen { segment })))
    }

    /// Observers/endpoint checks still read the original append; prepare only an equivalent finite physical rebase without touching canonical text.
    pub(in crate::markdown_document_buffer) fn prepare_composition_heap_cow(
        &self,
        previous: HeapAllocation,
        text: &str,
        before_segments: usize,
    ) -> CompositionHeapCow {
        let MarkdownEditSegment::Heap(original) = self.segments[previous.first].as_ref() else {
            unreachable!("原堆分配票只能引用相同编号的Heap段");
        };
        // Original append already froze long input into a file; retire old heap payload only, without copying long text or changing its file leaves.
        let rebase = text.len() <= EDIT_SEGMENT_BYTES;
        let input = if rebase { text } else { "" };
        let mut bytes = if previous.byte_start == 0 && input.is_empty() {
            Vec::new() // Release deleted exclusive heap capacity, retaining an empty index instead of a 64-KiB shell.
        } else {
            Vec::with_capacity(EDIT_SEGMENT_BYTES)
        };
        bytes.extend_from_slice(&original[..previous.byte_start]);
        let mut segments = Vec::with_capacity(2);
        let mut pieces = Vec::with_capacity(input.len().div_ceil(ORIGINAL_PIECE_BYTES) + 1);
        let mut consumed = 0;
        while consumed < input.len() {
            // Like original append, build complete UTF-8 leaves of at most 4 KiB; after the preserved prefix, at most one more 64-KiB segment is needed.
            let requested = ORIGINAL_PIECE_BYTES
                .min(EDIT_SEGMENT_BYTES - bytes.len())
                .min(input.len() - consumed);
            let mut end = consumed + requested;
            while !input.is_char_boundary(end) {
                end -= 1; // If the old tail cannot hold a multibyte scalar, keep that whole scalar for the next actual segment.
            }
            if end == consumed {
                segments.push(Arc::new(MarkdownEditSegment::Heap(bytes)));
                bytes = Vec::with_capacity(EDIT_SEGMENT_BYTES);
                continue;
            }
            let chunk = &input[consumed..end];
            let piece = MarkdownPiece {
                source: MarkdownPieceSource::EditSegment(previous.first + segments.len()),
                byte_start: bytes.len(),
                byte_len: chunk.len(),
                metrics: MarkdownDocumentMetrics::from_text(chunk),
            };
            bytes.extend_from_slice(chunk.as_bytes());
            pieces.push(Arc::new(MarkdownPieceTreeNode::Leaf(piece)));
            consumed = end;
        }
        segments.push(Arc::new(MarkdownEditSegment::Heap(bytes)));
        while segments.len() < previous.end - previous.first {
            // Published indices cannot move; an unused exclusive second segment becomes a genuinely zero-capacity heap slot.
            segments.push(Arc::new(MarkdownEditSegment::Heap(Vec::new())));
        }
        let end = previous.first + segments.len();
        let retain_segments = if rebase {
            before_segments.max(end) // Discard only this unpublished temporary heap suffix, never any old segment index.
        } else {
            self.segments.len() // Preserve the actual new Frozen index, retiring old heap payload without relocating file sources.
        };
        CompositionHeapCow {
            first: previous.first,
            segments,
            retain_segments,
            inserted: rebase.then(|| build_balanced_piece_tree(pieces)),
            allocation: (rebase && !text.is_empty()).then_some(HeapAllocation {
                first: previous.first,
                byte_start: previous.byte_start,
                end,
            }),
        }
    }
}

impl CompositionAllocation {
    /// Copy only original append-extent scalars; COW preparation acquires neither a strong old-payload edge nor file-retirement authority.
    pub(in crate::markdown_document_buffer) fn heap(&self) -> Option<HeapAllocation> {
        match self {
            Self::Heap(allocation) => Some(*allocation),
            Self::Frozen { .. } => None,
        }
    }

    /// The caller proved a same-group merge replacing the entire current marked range; old snapshots retain their own Arc while history keeps its first root.
    pub(in crate::markdown_document_buffer) fn retire(self, edits: &mut MarkdownEditSegments) {
        let Self::Frozen { segment } = self else {
            return; // The same candidate's COW publication already retired heap storage; do not copy or rebase after canonical publication.
        };
        let slot = &mut edits.segments[segment];
        let MarkdownEditSegment::Frozen(backing) = slot.as_ref() else {
            unreachable!("组合分配票只来自尚未退休的唯一Frozen段");
        };
        let observer = backing.observer();
        // Never move/reuse the original index; its receipt continues observing the last owner's actual msync/munmap and sticky first failure.
        *slot = Arc::new(MarkdownEditSegment::RetiredFrozen(Arc::new(observer)));
    }
}

impl CompositionHeapCow {
    /// New heap text inherits only actual compacted offsets; long Frozen input retains the original file ticket from this same append.
    pub(in crate::markdown_document_buffer) fn allocation(&self) -> Option<CompositionAllocation> {
        self.allocation.map(CompositionAllocation::Heap)
    }

    /// Some(None) means an actual empty insertion; None means long input keeps its original Frozen leaves without replacing their subtree.
    pub(in crate::markdown_document_buffer) fn take_inserted(&mut self) -> Option<PieceTreeLink> {
        self.inserted.take()
    }

    /// Shared prepare completed all checks; install equivalent bytes and then their rebased root within one nonreentrant publication stack.
    pub(in crate::markdown_document_buffer) fn publish(self, edits: &mut MarkdownEditSegments) {
        for (offset, segment) in self.segments.into_iter().enumerate() {
            edits.segments[self.first + offset] = segment;
        } // Old snapshots retain original Arcs; the original Undo baseline reads only the unchanged first-segment prefix.
        edits.segments.truncate(self.retain_segments);
        // Preserve cumulative used_bytes from actual append; physical retirement does not renew session write budget.
    }
}
