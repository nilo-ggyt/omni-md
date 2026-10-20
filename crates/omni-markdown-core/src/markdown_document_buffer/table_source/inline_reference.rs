//! 一个原引用请求的有限identifier解码；不是段落正文，也不建立第二份定义表。

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
    // 请求来自原同一cursor，含真实record、owner与label；不允许拿裸range补一个查询。
    pub request: VerifiedInlineReferenceRequest,
    decoder: InlineReferenceDecoder,
    chunks: Vec<Vec<u8>>,
    len: usize,
    decoded: bool,
    // 完整key只在原有限normalizer边界存在，之后交原UniCase；这里没有第二份定义库存。
    label: Option<String>,
}
impl ReferenceLabel {
    pub fn new(request: VerifiedInlineReferenceRequest) -> Self {
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

    /// 借原Source小片逐步运行共享label语法；只有原closing bracket能结束，短frame不是EOF。
    /// complete后只在既有有限identifier规范化边界建立连续key，真实copy/容量完整单列，
    /// 不声称这个原Normalizer/UniCase边界逐token服从4096，也不把它扩展到段落正文。
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
            return Err(Error::SourceSuperseded);
        }
        if self.label.is_some() {
            return Ok(SemanticWork::default());
        }
        if self.decoded {
            // 原key上限先于连续分配。新旧小片在本步短暂共存，quota按真实新容量累计，
            // 归还旧chunks不从累计燃料反扣，也不伪称现在只保一份便没有分配峰值。
            quota.admit_metadata(self.len).map_err(Error::Window)?;
            // decoder每次输出完整UTF8；页可切scalar，故这里只拼原字节后检查整份有限identifier。
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
            // 到真实closing bracket后允许原decoder排出carry；空输出不是Source EOF证明。
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
            // 回收先于展开语法错误。真实Source EIO不能被坏label/输出错误抢成次因。
            frame.finish().map_err(Error::Source)?;
            tree
        } else {
            0
        };
        let step = step.map_err(Error::Global)?;
        // 只有同一真实scope续行实际取得的新页才收费；浅Arc共享并不复制或再次物化正文。
        let prefix_allocated = self.decoder.allocated_workspace_bytes() - allocated_before;
        quota
            .admit_metadata(prefix_allocated)
            .map_err(Error::Window)?;
        if self.len.saturating_add(step.written) > MAX_REFERENCE_NORMALIZED_LABEL_BYTES {
            return Err(Error::Window(MarkdownTableWindowError::ReferenceLimit));
        }
        let bytes = output.get(..step.written).ok_or(Error::InvalidRequest)?;
        std::str::from_utf8(bytes).map_err(|_| Error::InvalidRequest)?;
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
        // 不reserve/realloc已持有的identifier字节。目录只移动小句柄，并按真的新目录
        // 容量及移动长度计账；这与URL输出的有限页拥有责任一致，绝不藏一次大复制。
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
            // allocator若给出更大真实capacity，额外容量也必须准入，不能只记请求值。
            quota
                .admit_metadata(allocated + directory_allocated - capacity - directory)
                .map_err(Error::Window)?;
        }
        self.len += bytes.len();
        self.decoded = step.complete;
        // 实际借片、共享scanner消费、输出复制和UTF8验证各报自己的事实；scanner可能
        // 只用到frame前缀，故source_bytes不能退化为work.byte_reads来漏掉真实尾部I/O。
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
