//! 原组合追加的实际extent决定退休权限；文件保释放票，小候选仅对至多两段做COW。

use super::*;

/// 文件拥有唯一新编号；堆候选只拥有首段真实追加起点之后、至多两段的新增字节。
pub(in crate::markdown_document_buffer) enum CompositionAllocation {
    Frozen { segment: usize },
    Heap(HeapAllocation),
}

#[derive(Clone, Copy)]
/// 原历史可以引用首段前缀；该byte起点之前的内容绝不能因完整marked替换而被覆盖。
pub(in crate::markdown_document_buffer) struct HeapAllocation {
    first: usize,
    byte_start: usize,
    end: usize,
}

/// 只有原输入唯一候选持有这些有限新字节；发布前不写回段表，不建立第二正文Source。
pub(in crate::markdown_document_buffer) struct CompositionHeapCow {
    first: usize,
    segments: Vec<Arc<MarkdownEditSegment>>,
    retain_segments: usize,
    inserted: Option<PieceTreeLink>,
    allocation: Option<HeapAllocation>,
}

impl MarkdownEditSegments {
    /// 同一次实际append的叶给出精确归属；不从待输入长度猜测首段或共享前缀。
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
                // <=64KiB输入即使从旧尾开始也至多占两段；末段编号来自真实最后一个追加叶。
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
        // append_frozen只在全部有限lease成功后追加一个独占编号；失败仍由原checkpoint回退。
        Ok((pieces, Some(CompositionAllocation::Frozen { segment })))
    }

    /// 原append仍供观察者和端点校验读取；这里只准备逻辑等价的有限物理重排，不触碰canonical。
    pub(in crate::markdown_document_buffer) fn prepare_composition_heap_cow(
        &self,
        previous: HeapAllocation,
        text: &str,
        before_segments: usize,
    ) -> CompositionHeapCow {
        let MarkdownEditSegment::Heap(original) = self.segments[previous.first].as_ref() else {
            unreachable!("原堆分配票只能引用相同编号的Heap段");
        };
        // 长输入已经由原append封存为文件；只清旧Heap候选，不能复制长输入或改其文件叶。
        let rebase = text.len() <= EDIT_SEGMENT_BYTES;
        let input = if rebase { text } else { "" };
        let mut bytes = if previous.byte_start == 0 && input.is_empty() {
            Vec::new() // 完全独占且已删除的Heap真正归还容量，保空编号而不保64KiB空壳。
        } else {
            Vec::with_capacity(EDIT_SEGMENT_BYTES)
        };
        bytes.extend_from_slice(&original[..previous.byte_start]);
        let mut segments = Vec::with_capacity(2);
        let mut pieces = Vec::with_capacity(input.len().div_ceil(ORIGINAL_PIECE_BYTES) + 1);
        let mut consumed = 0;
        while consumed < input.len() {
            // 与原append同样只建完整UTF-8、至多4KiB叶；保留前缀之后最多还需一个64KiB段。
            let requested = ORIGINAL_PIECE_BYTES
                .min(EDIT_SEGMENT_BYTES - bytes.len())
                .min(input.len() - consumed);
            let mut end = consumed + requested;
            while !input.is_char_boundary(end) {
                end -= 1; // 原尾隙不足一个多字节标量时，完整标量留给下一个真实段。
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
            // 旧已发布编号不能移动；独占第二段不再被新正文使用时只留下真正零容量Heap槽。
            segments.push(Arc::new(MarkdownEditSegment::Heap(Vec::new())));
        }
        let end = previous.first + segments.len();
        let retain_segments = if rebase {
            before_segments.max(end) // 仅本次未发布的临时Heap后缀可丢弃，不截断任何旧编号。
        } else {
            self.segments.len() // 原新Frozen编号必须保留；只退休旧Heap载荷，不重定位文件源。
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
    /// 仅复制原追加extent的标量；准备COW不能取得旧payload强边或退休文件的能力。
    pub(in crate::markdown_document_buffer) fn heap(&self) -> Option<HeapAllocation> {
        match self {
            Self::Heap(allocation) => Some(*allocation),
            Self::Frozen { .. } => None,
        }
    }

    /// 调用者已证明同组merge且替换整个当前marked；旧root没有进入新历史，旧快照自持Arc。
    pub(in crate::markdown_document_buffer) fn retire(self, edits: &mut MarkdownEditSegments) {
        let Self::Frozen { segment } = self else {
            return; // Heap已由同一候选的COW发布退休，不在canonical发布后再拷贝或改偏移。
        };
        let slot = &mut edits.segments[segment];
        let MarkdownEditSegment::Frozen(backing) = slot.as_ref() else {
            unreachable!("组合分配票只来自尚未退休的唯一Frozen段");
        };
        let observer = backing.observer();
        // 原编号不移动、不复用；真实最后owner的msync/munmap及sticky首错继续沿此票观察。
        *slot = Arc::new(MarkdownEditSegment::RetiredFrozen(Arc::new(observer)));
    }
}

impl CompositionHeapCow {
    /// 新Heap正文只继承已压缩的实际offset；长Frozen输入仍保同一次append的原文件票。
    pub(in crate::markdown_document_buffer) fn allocation(&self) -> Option<CompositionAllocation> {
        self.allocation.map(CompositionAllocation::Heap)
    }

    /// Some(None)是真正空插入；None表示长输入沿原Frozen叶，不需要替换其布局子树。
    pub(in crate::markdown_document_buffer) fn take_inserted(&mut self) -> Option<PieceTreeLink> {
        self.inserted.take()
    }

    /// 共享prepare已完成全部校验；同一不可重入发布栈内先安装等价字节，随后发布其重定位root。
    pub(in crate::markdown_document_buffer) fn publish(self, edits: &mut MarkdownEditSegments) {
        for (offset, segment) in self.segments.into_iter().enumerate() {
            edits.segments[self.first + offset] = segment;
        } // 老Snapshot各自保原Arc，旧Undo baseline只读取未改变的首段前缀。
        edits.segments.truncate(self.retain_segments);
        // used_bytes保同次真实append的累计值；物理退休不是补发会话写入预算。
    }
}
