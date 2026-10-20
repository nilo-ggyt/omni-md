//! 同一原 Link proof 的有限目标解码与完整 label 坐标；不复制 label、段落或另建 Parser。

use super::*;
use crate::MarkdownLinkDestination;
use pulldown_cmark::global_frontier::{
    InlineLinkDestinationDecoder, InlineLinkKind, InlineLinkTarget, VerifiedInlineLink,
};

const OUTPUT_BYTES: usize = 1024;
// 原16KiB目标上限最多形成16个页句柄；只为小目录可能的真实搬移留量，不给目标正文另领预算。
const DIRECTORY_COPY_RESERVE: usize =
    crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_REFERENCE_BYTES
        .div_ceil(OUTPUT_BYTES)
        * std::mem::size_of::<Vec<u8>>();

enum Phase {
    Destination,
    Reference,
    Coordinates,
}

/// 单个真实链接只保原 proof、同一个有界目标页目录和四端点游标，完成后直接移交所有权。
/// label 可以跨任意多帧；不会把 label 或 URL/title 所在的整个 owner 搬入 RAM。
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
    pub(super) fn new(proof: VerifiedInlineLink, maximum_destination_bytes: usize) -> Self {
        let mut reference = None;
        let decoder = proof.destination_decoder();
        let phase = if proof.kind() == InlineLinkKind::Image {
            Phase::Coordinates // 原Image只贡献样式/marker，不为其目标创建点击权限或复制URI。
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

    /// 每步只消费一个目标小帧或一个原坐标步骤；解码内核与完整 Parser 共用同一 unescape。
    /// 所有 bytes/copy/容量都来自这次实际消费，不为完整目标一次分配16KiB或收尾再复制。
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
        } // proof 来自父作业同一 seed/cursor；这里也不让编辑或 Undo 换代借旧范围继续复制目标。
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
                    // 原 Link owner 决定 marker 活动状态，完整 label 决定点击权限；可见窗口只
                    // 限制父 run 库存，不能把长 label 截短成一个新链接或拿目标字节充当 label。
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
                // 四个真实端点分别借原树的小片目录定位；不能从 owner.start 连续重读整个长
                // label 只为知道 label.end 的 UTF16。游标仍唯一，每次真实读量都独立交回。
                let (coordinates, work) = Coordinates::new(document, [boundary; 4], budget)?;
                self.coordinates = Some(coordinates);
                Ok((work, None))
            }
        }
    }

    /// 固定输出片由 vendor 保证完整 UTF8；Core 只验证本片并增长唯一目标，不实现第二套 escape 规则。
    /// 内核只借同 Source 的原字节帧，完成 lease 回收后才消费输出，保持真实源失败优先。
    fn advance_destination(
        &mut self,
        document: &MarkdownDocumentBuffer,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<(SemanticWork, Option<LinkProjection>), MarkdownTableRequestError> {
        let tree = tree_height(document);
        // 为本步实际输出复制、UTF8 验证、页/小目录容量与父控制留出原额度；不是第二次领量子。
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
            None // 原decoder可在目标末端排出已读carry；没有真实新读，不能伪造源帧或树费用。
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
            )); // 沿原本批目标库存限额，不能跳过一个过长真实目标再采用别的链接。
        }
        let tail = self
            .destination
            .last()
            .map_or(0, |chunk| chunk.capacity() - chunk.len());
        let new_chunk = text.len() > tail;
        let new_directory = new_chunk && self.destination.len() == self.destination.capacity();
        // 最后一个真实输出页只容纳剩余目标；未完成时仍保固定小页，后续只填其已有容量。
        // 短URI不必保1KiB空尾，且没有为省容量而realloc已经累积的目标正文。
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
            // 显式把既有页句柄移动到新小目录，才能按真正发生的逻辑搬移收费；不猜allocator
            // 的reserve是否原地增长。新旧目录瞬时共存，quota按新目录完整容量累计，旧目录
            // 只在这次移交后真实释放；这里不把累计分配账冒充当前保有量。
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
        // 页填满后只新增一页，绝不realloc已经积累的URL字节；最多16项目录的移动已留量。
        // 片边界可以落在scalar中间，但原完整UTF8输出的拼接顺序保持不变，只有私有构造器能封存。
        if step.complete {
            self.phase = Phase::Coordinates;
        }
        Ok((
            SemanticWork {
                // 实体查表/输出满可能只消费原帧前缀；源账仍计实际借到的小帧，不能漏尾部重读。
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
