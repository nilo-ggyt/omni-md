//! 将真实物理行的四个 byte 边界分步转换为绝对 UTF-16；不复制正文，也不重新分类普通块。

use super::*;

const MIN_STEP_BUDGET: usize = 2048;
const MAX_STEP_BUDGET: usize = 65536;
const CONTROL_WORK: usize = 64;

/// 仅保四个坐标与当前标量的续读状态；源身份、取消和累计 fuel 由外层来源请求拥有。
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
    /// 边界依次为 physical start、content start、content end、EOL end，均来自真实来源票据。
    pub(super) fn new(
        document: &MarkdownDocumentBuffer,
        boundaries: [usize; 4],
        budget: usize,
    ) -> Result<(Self, SemanticWork), MarkdownTableRequestError> {
        validate_budget(budget)?;
        if boundaries.windows(2).any(|pair| pair[0] > pair[1])
            || boundaries[3] > document.byte_len()
        {
            // 坐标次序或末端与当前源冲突时，不能把它纠正为另一条看似可用的物理行。
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
            // AVL 路径与固定四边界校验先准入；低预算不能隐含一次无界的树遍历。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut node = document
            .root
            .as_deref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let mut local = boundaries[0];
        let mut before_utf16 = 0usize;
        // 完整左子树只取已经验证的缓存计量；目标所在叶片的前缀留给 advance 有界读取。
        while let MarkdownPieceTreeNode::Branch { left, right, .. } = node {
            let left_metrics = left.metrics();
            if local < left_metrics.byte_len {
                // 查询点在左侧，右侧正文与其计量均不参与当前前缀。
                node = left;
            } else {
                // 查询点进入右侧，完整左侧无需解码；checked 运算拒绝内部计量失配。
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
            // 真实叶片不能容纳目标或缓存前缀超出全文，属于内部来源失配，不发布猜测坐标。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if let MarkdownPieceTreeNode::OriginalSpan(span) = node {
            // 共享目录给出真实小片起点；仅累加已验证前缀，不扫描Span正文。
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
        // 定位与固定校验已经执行，立即移交其工作；创建后取消或 Drop 也不会漏计本次真实成本。
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

    /// 复用上一组真实末端，跨行或跨块间隙仍由后续 advance 读取，不从叶片起点重新计量。
    pub(super) fn continue_at(
        &mut self,
        boundaries: [usize; 4],
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if self.boundary_index != self.boundaries.len()
            || self.scalar_remaining != 0
            || self.next != self.boundaries[3]
        {
            // 未完成的来源组必须先完成或由父请求丢弃；不能用下一行覆盖尚未交付的坐标。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if boundaries.windows(2).any(|pair| pair[0] > pair[1])
            || boundaries[0] < self.next
            || boundaries[3] > self.source_byte_len
        {
            // 续读只沿原源前进；回跳、重叠或末端越界需要新的可信请求，不能隐式重置扫描位置。
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
        // 仅替换固定四边界与结果槽；原 next/UTF-16/标量状态保留，且没有新增树定位工作。
        self.boundaries = boundaries;
        self.values = [0; 4];
        self.boundary_index = 0;
        // 固定复用工作在本次调用完成时同步归账，不把已发生的成本留待下一步读取才补报。
        Ok(work)
    }

    /// 每次至多借一个真实源 frame；返回的工作包含本转换自己的重读，不重复声明 origin 扫描。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(SemanticWork, Option<[usize; 4]>), MarkdownTableRequestError> {
        validate_budget(budget)?;
        // 本步只报告本次读取与控制；构造、续读复用的工作已由各自调用同步返回。
        let mut work = SemanticWork {
            structural_transitions: CONTROL_WORK,
            ..SemanticWork::default()
        };
        self.record_boundaries(&mut work)?;
        if self.boundary_index == self.boundaries.len() {
            // 零长度区段或上一步恰好到达末边界时，无需为发布坐标再次借正文。
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
            // 只归还本步已完成的固定工作；下一步仍从同一个真实 byte 继续，不借空 frame 冒充进展。
            return Ok((work, None));
        };
        let unread = self.boundaries[3]
            .checked_sub(self.next)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let wanted = (available / 3).min(unread).min(MAX_STEP_BUDGET);
        if wanted == 0 {
            // 尚有未签发边界却没有可读字节，说明游标与真实边界状态已经不一致。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // tree_height 是实际源定位的保守路径上界，单列为树工作；它不声称精确 CPU 指令数。
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
            // 只接受请求位置上的真实连续借用；短 leaf 可以暂停，错误 frame 或假 EOF 不可续用。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        for byte in frame.bytes.iter() {
            // 每字节分别计一次源读取和两次解码/推进转换；四边界元数据与发布空间已提前保留。
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
                // 最后边界是真实 EOL end；即使 frame 尚有借用余量，也不读取行外正文。
                frame
                    .finish()
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return self.finish(work, budget);
            }
        }
        if work_cost(work)? > budget {
            // 准入和实际计量必须一致；内部超支不能靠截小工作报告或发布半行坐标掩盖。
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
        // 只有四个实际来源边界；相邻空 prefix/content/EOL 可以在同一 byte 记录多个真实位置。
        while let Some(boundary) = self.boundaries.get(self.boundary_index).copied() {
            if boundary < self.next {
                // 已越过尚未记录的边界，拒绝把后面的 UTF-16 位置当成它的近似答案。
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if boundary > self.next {
                // 当前边界仍在真实源前方，必须先由有界读取推进，不能按 byte 差推导 UTF-16。
                return Ok(());
            }
            if self.scalar_remaining != 0 {
                // 票据承诺 scalar 边界；命中 UTF-8 continuation 中部表示内部失配。
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
            // ASCII 含 CR、LF，各自贡献一个原始 UTF-16 unit；这里不折叠 CRLF。
            (0, 0x00..=0x7f) => (1usize, 0),
            // 有效快照已经验证 UTF-8；首字节确定整个 scalar 的 UTF-16 贡献，尾字节不重复计量。
            (0, 0xc2..=0xdf) => (1, 1),
            (0, 0xe0..=0xef) => (1, 2),
            (0, 0xf0..=0xf4) => (2, 3),
            // frame 可以切在 scalar 内部；只保最多三个待续 byte 的计数，不保存正文副本。
            (1..=3, 0x80..=0xbf) => (
                0,
                self.scalar_remaining
                    .checked_sub(1)
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
            ),
            // 这只是快照合同下的边界一致性检查，不将该有限桥接宣称为独立 UTF-8 验证器。
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
            // 输出四坐标也属于本步元数据工作，不能在累计燃料不足时悄悄免费发布。
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        Ok((work, Some(self.values)))
    }
}

fn validate_budget(budget: usize) -> Result<(), MarkdownTableRequestError> {
    if !(MIN_STEP_BUDGET..=MAX_STEP_BUDGET).contains(&budget) {
        // 父来源入口仍要求至少 4096；子转换器允许父层扣除移交余量后传入有限剩余预算。
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
    // 每条记录真实读取一个 byte 边界并写入一个 UTF-16 坐标，不声明正文分配容量。
    std::mem::size_of::<usize>()
        .checked_mul(2)
        .ok_or(MarkdownTableRequestError::WorkLimit)
}

fn work_cost(work: SemanticWork) -> Result<usize, MarkdownTableRequestError> {
    // source_tree_steps 已包含于 structural_transitions，保留分列观察而不重复扣同一树路径。
    work.source_bytes
        .checked_add(work.structural_transitions)
        .and_then(|cost| cost.checked_add(work.metadata_bytes))
        .ok_or(MarkdownTableRequestError::WorkLimit)
}
