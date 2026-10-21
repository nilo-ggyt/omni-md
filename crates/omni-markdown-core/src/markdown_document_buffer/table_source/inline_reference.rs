//! Decode one original bounded reference identifier without copying paragraph text or duplicating definitions.

use super::*;
use crate::markdown_document_buffer::table_window_request::{
    MarkdownTableRequestError as Error, SemanticWork,
};
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowQuota, references::MAX_REFERENCE_NORMALIZED_LABEL_BYTES,
};
use pulldown_cmark::block_frontier::SourceFrame;
use pulldown_cmark::global_frontier::{
    GlobalSourceAuthority, InlineReferenceDecoder, VerifiedInlineReferenceRequest,
};

pub(in crate::markdown_document_buffer) struct ReferenceLabel {
    // The original cursor supplied this verified record, owner and label;
    // a bare range cannot manufacture an equivalent reference request.
    pub request: VerifiedInlineReferenceRequest,
    decoder: InlineReferenceDecoder,
    chunks: Vec<Vec<u8>>,
    len: usize,
    decoded: bool,
    // A contiguous key exists only at the bounded normalizer boundary, then uses
    // the original UniCase inventory. This owner has no second definition map.
    label: Option<String>,
}
impl ReferenceLabel {
    pub fn new(request: VerifiedInlineReferenceRequest) -> Self {
        // Construct the decoder from the same verified request, retaining only
        // current identifier progress until its genuine closing bracket is decoded.
        Self {
            decoder: request.reference_decoder(),
            request,
            chunks: Vec::new(),
            len: 0,
            decoded: false,
            label: None,
        }
    }
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Run shared label grammar over original pieces; only the genuine closing bracket completes it.
    /// The bounded identifier may become contiguous at normalization, with copy/capacity reported separately.
    /// This existing normalizer/UniCase boundary is not a per-token 4-KiB guarantee or paragraph-copy permission.
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<SemanticWork, Error> {
        document.original_backing_health().map_err(Error::Source)?;
        if self.request.source_revision() != document.generation()
            || self.request.source_end() != document.byte_len()
        {
            // An edited original source cannot resume the old decoder's label;
            // the parent transaction retains its stronger root/scope authority.
            return Err(Error::SourceSuperseded);
        }
        if self.label.is_some() {
            return Ok(SemanticWork::default());
        }
        if self.decoded {
            // Admit the complete bounded key before allocating it. Old chunks and
            // the new buffer briefly coexist; cumulative allocation fuel does not
            // erase that peak when chunks are returned.
            quota.admit_metadata(self.len).map_err(Error::Window)?;
            // Decoder output is valid UTF-8 while chunk storage may split scalars;
            // join original bytes once and validate the complete bounded identifier.
            let mut bytes = Vec::with_capacity(self.len);
            for chunk in std::mem::take(&mut self.chunks) {
                bytes.extend_from_slice(&chunk);
            }
            let label = String::from_utf8(bytes).map_err(|_| Error::InvalidRequest)?;
            let capacity = label.capacity();
            if capacity > self.len {
                quota
                    .admit_metadata(capacity - self.len)
                    .map_err(Error::Window)?;
            }
            self.label = Some(label);
            return Ok(SemanticWork {
                normalizer_input_bytes: self.len,
                copied_source_bytes: self.len,
                metadata_bytes: capacity,
                allocated_source_capacity: capacity,
                utf8_validation_bytes: self.len,
                structural_transitions: 128,
                ..Default::default()
            });
        }
        let tree = document
            .root
            .as_ref()
            .map_or(0, |root| usize::from(root.height()))
            + 1;
        let child = budget.saturating_sub(tree + 128 + 1024 + 512);
        // Reserve actual tree descent, control and fixed output work before the
        // shared decoder quantum; an insufficient quantum fails without borrowing input.
        if child < 2048 {
            return Err(Error::WorkLimit);
        }
        let next = self.decoder.next_byte();
        let maximum = self
            .request
            .reference_range()
            .end
            .checked_sub(next)
            .ok_or(Error::InvalidRequest)?
            .min(1024);
        let frame = if maximum == 0 {
            // After the real closing bracket, the same decoder may flush carry;
            // empty output is not evidence of the document's EOF.
            None
        } else {
            Some(
                document
                    .table_source_frame(next, maximum)
                    .map_err(Error::Source)?,
            )
        };
        let source_bytes = frame.as_ref().map_or(0, |frame| frame.bytes.len());
        let mut output = [0; 1024];
        let allocated_before = self.decoder.allocated_workspace_bytes();
        let step = self.decoder.advance(
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
            // Finish the actual lease before inspecting grammar output/errors.
            // A source reclaim failure must remain ahead of a malformed label.
            frame.finish().map_err(Error::Source)?;
            tree
        } else {
            0
        };
        let step = step.map_err(Error::Global)?;
        // Charge only newly acquired workspace for the same actual scope
        // continuation. Sharing an Arc does not copy or rematerialize source text.
        let prefix_allocated = self.decoder.allocated_workspace_bytes() - allocated_before;
        quota
            .admit_metadata(prefix_allocated)
            .map_err(Error::Window)?;
        if self.len.saturating_add(step.written) > MAX_REFERENCE_NORMALIZED_LABEL_BYTES {
            return Err(Error::Window(MarkdownTableWindowError::ReferenceLimit));
        }
        let bytes = output.get(..step.written).ok_or(Error::InvalidRequest)?;
        std::str::from_utf8(bytes).map_err(|_| Error::InvalidRequest)?;
        // Reuse remaining capacity in the current identifier page, then allocate
        // at most the next finite page. Never reserve/reallocate all retained text.
        let free = self
            .chunks
            .last()
            .map_or(0, |chunk| chunk.capacity() - chunk.len());
        let remaining = bytes.len().saturating_sub(free);
        let capacity = if remaining == 0 {
            0
        } else if step.complete {
            remaining
        } else {
            1024
        };
        let directory = if remaining != 0 && self.chunks.len() == self.chunks.capacity() {
            (self.chunks.len() + 1) * std::mem::size_of::<Vec<u8>>()
        } else {
            0
        };
        quota
            .admit_metadata(capacity + directory)
            .map_err(Error::Window)?;
        // Grow only the directory of small page handles. Report its actual new
        // capacity and moved handle bytes separately; no large identifier-text
        // copy is hidden inside directory replacement.
        let mut directory_moved = 0;
        let mut directory_allocated = 0;
        if directory != 0 {
            let mut next = Vec::with_capacity(self.chunks.len() + 1);
            directory_allocated = next.capacity() * std::mem::size_of::<Vec<u8>>();
            directory_moved = self.chunks.len() * std::mem::size_of::<Vec<u8>>();
            next.append(&mut self.chunks);
            self.chunks = next;
        }
        let copied = bytes.len().min(free);
        if let Some(tail) = self.chunks.last_mut() {
            tail.extend_from_slice(&bytes[..copied]);
        }
        let mut allocated = 0;
        if copied < bytes.len() {
            let mut chunk = Vec::with_capacity(capacity);
            allocated = chunk.capacity();
            chunk.extend_from_slice(&bytes[copied..]);
            self.chunks.push(chunk);
        }
        if allocated + directory_allocated > capacity + directory {
            // Admit allocator-provided excess capacity too; the requested page
            // size alone is not the actual retained allocation fact.
            quota
                .admit_metadata(allocated + directory_allocated - capacity - directory)
                .map_err(Error::Window)?;
        }
        self.len += bytes.len();
        self.decoded = step.complete;
        // Report borrowed bytes, scanner consumption, copied output and UTF-8
        // validation independently. The scanner may consume only a frame prefix,
        // so its byte_reads cannot stand in for the entire actual leased slice.
        Ok(SemanticWork {
            source_bytes,
            source_tree_steps: tree,
            structural_transitions: step.work.transitions
                + tree
                + 128
                + bytes.len()
                + directory_moved,
            metadata_bytes: allocated + directory_allocated + prefix_allocated,
            allocated_source_capacity: allocated,
            copied_source_bytes: bytes.len(),
            normalizer_input_bytes: step.work.byte_reads,
            utf8_validation_bytes: bytes.len(),
            ..Default::default()
        })
    }
}
