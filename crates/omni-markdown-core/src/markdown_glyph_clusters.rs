//! 同一真实字形任务的逻辑cluster与26.6前缀索引，只保存一页派生元数据。
//!
//! cluster可能合并多个字素；这个索引不把cluster内位置伪装成可复用字形的断点。
//! 它也不决定软行、重排、词典、字体切换或caret，消费者须完成各自的真实阶段。

use std::{ops::Range, sync::Weak};

use thiserror::Error;

use crate::{
    MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority, MarkdownShapedGlyph,
    MarkdownShapingDirection, MarkdownShapingError, MarkdownShapingJob,
    markdown_layout_index::{LayoutIndexError, LayoutIndexObserver, PagedLayoutIndex},
};

/// 单次调用至多消费这些真实glyph；一个任意长cluster也要保留同一待决状态并让出。
pub const MARKDOWN_GLYPH_CLUSTER_STEP: usize = 256;

#[derive(Debug, Error)]
pub enum MarkdownGlyphClusterError {
    #[error("字形cluster索引已经撤权")]
    Poisoned,
    #[error("字形cluster索引不属于此任务")]
    JobMismatch,
    #[error("字形cluster配额或查询范围无效")]
    InvalidRequest,
    #[error("字形cluster坐标、计数或累计advance溢出")]
    Overflow,
    #[error("字形cluster顺序或元数据与原任务不一致")]
    InconsistentGlyphs,
    #[error("无法访问原字形任务：{0}")]
    Shaping(#[from] MarkdownShapingError),
    #[error("字形cluster分页操作失败：{0}")]
    Storage(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, MarkdownGlyphClusterError>;

impl From<LayoutIndexError> for MarkdownGlyphClusterError {
    fn from(error: LayoutIndexError) -> Self {
        match error {
            LayoutIndexError::InvalidRecord => Self::InconsistentGlyphs,
            LayoutIndexError::Overflow => Self::Overflow,
            LayoutIndexError::Poisoned => Self::Poisoned,
            LayoutIndexError::Io(error) => Self::Storage(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGlyphClusterStatus {
    Building,
    Ready,
    Cancelled,
    Poisoned,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 原全局UTF16范围与原视觉glyph半开范围；前缀advance按逻辑源顺序累计。
pub struct MarkdownGlyphCluster {
    pub source_utf16: Range<usize>,
    pub glyphs: Range<usize>,
    pub prefix_x_26_6: i64,
    pub prefix_y_26_6: i64,
    pub x_advance_26_6: i64,
    pub y_advance_26_6: i64,
    /// 保留原flags并集，绝不把unsafe-to-break/concat解释成安全边界。
    pub flags: u32,
    pub missing_glyphs: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownGlyphClusterAnswer {
    Pending,
    Known(Option<MarkdownGlyphCluster>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGlyphClusterStep {
    pub glyphs: usize,
    pub clusters: usize,
    pub status: MarkdownGlyphClusterStatus,
}

#[derive(Clone)]
/// 只有真实FD/map/写buffer归还事实，不保活索引、job、字体或原文。
pub struct MarkdownGlyphClusterResourceObserver(LayoutIndexObserver);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGlyphClusterResources {
    /// 原索引writer已实际close；私有单页没有File时，仍不能把活owner误报为退出。
    pub owner_closed: bool,
    pub file_open: bool,
    pub mapped_bytes: usize,
    pub peak_mapped_bytes: usize,
    /// 唯一metadata写buffer；Reader归还该buffer后只保原File。
    pub buffer_bytes: usize,
    pub peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
}
impl MarkdownGlyphClusterResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownGlyphClusterResources {
        let value = self.0.snapshot();
        MarkdownGlyphClusterResources {
            owner_closed: value.owner_closed,
            file_open: value.file_open,
            mapped_bytes: value.mapped_bytes,
            peak_mapped_bytes: value.peak_mapped_bytes,
            buffer_bytes: value.buffer_bytes,
            peak_buffer_bytes: value.peak_buffer_bytes,
            cleanup_failed: value.cleanup_failed,
        }
    }
}

struct Payload {
    index: PagedLayoutIndex<64>,
    pending: Option<MarkdownGlyphCluster>,
}

/// 不持有原job的强边；每次推进、读取、EOF均须借用创建索引的同一个Ready任务。
pub struct MarkdownGlyphClusterIndex {
    payload: Option<Payload>,
    identity: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    source_range: Range<usize>,
    glyph_count: usize,
    backwards: bool,
    consumed: usize,
    committed_end: usize,
    prefix_x: i64,
    prefix_y: i64,
    status: MarkdownGlyphClusterStatus,
    resources: MarkdownGlyphClusterResourceObserver,
}

impl MarkdownGlyphClusterIndex {
    pub fn new(
        job: &mut MarkdownShapingJob,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Self> {
        // profile核真实原job，EOF读再验证调用方的原Source/Font；未Ready不创建匿名文件。
        let profile = job.profile()?;
        job.glyph(source, font, profile.glyph_count)?;
        let backwards = matches!(
            job.direction(),
            MarkdownShapingDirection::RightToLeft | MarkdownShapingDirection::BottomToTop
        );
        let index = PagedLayoutIndex::new()?;
        let resources = MarkdownGlyphClusterResourceObserver(index.resource_observer());
        let source_range = job.source_range();
        Ok(Self {
            payload: Some(Payload {
                index,
                pending: None,
            }),
            identity: job.index_identity(),
            source: source.clone(),
            font: font.clone(),
            committed_end: source_range.start,
            source_range,
            glyph_count: profile.glyph_count,
            backwards,
            consumed: 0,
            prefix_x: 0,
            prefix_y: 0,
            status: MarkdownGlyphClusterStatus::Building,
            resources,
        })
    }

    fn guard(&self, job: &mut MarkdownShapingJob) -> Result<()> {
        if !Weak::ptr_eq(&self.identity, &job.index_identity()) {
            return Err(MarkdownGlyphClusterError::JobMismatch);
        }
        // 源/字体真实失败优先于本派生索引的状态；不能以旧metadata掩盖共享源已失败。
        if job
            .glyph(&self.source, &self.font, self.glyph_count)?
            .is_some()
        {
            return Err(MarkdownGlyphClusterError::InconsistentGlyphs);
        }
        if self.payload.is_none() {
            return Err(MarkdownGlyphClusterError::Poisoned);
        }
        Ok(())
    }

    fn fail<T>(&mut self, error: MarkdownGlyphClusterError) -> Result<T> {
        self.status = MarkdownGlyphClusterStatus::Poisoned;
        drop(self.payload.take()); // 返回失败前真实交还本索引的FD/map，不保留部分可发布结果。
        Err(error)
    }

    /// 读取完整原glyph，按真实方向归并同一cluster；step边界不提交待决cluster。
    pub fn step(
        &mut self,
        job: &mut MarkdownShapingJob,
        budget: usize,
    ) -> Result<MarkdownGlyphClusterStep> {
        let result = self.advance(job, budget);
        match result {
            Ok(step) => Ok(step),
            Err(error) => self.fail(error),
        }
    }

    fn advance(
        &mut self,
        job: &mut MarkdownShapingJob,
        budget: usize,
    ) -> Result<MarkdownGlyphClusterStep> {
        self.guard(job)?;
        if budget == 0 {
            return Err(MarkdownGlyphClusterError::InvalidRequest);
        }
        let before = self.count();
        let mut read = 0;
        while self.consumed < self.glyph_count && read < budget.min(MARKDOWN_GLYPH_CLUSTER_STEP) {
            // RTL/BTT只反向读取原视觉数组；不反转glyph内容、不分配排序Vec。
            let at = if self.backwards {
                self.glyph_count - self.consumed - 1
            } else {
                self.consumed
            };
            let glyph = job
                .glyph(&self.source, &self.font, at)?
                .ok_or(MarkdownGlyphClusterError::InconsistentGlyphs)?;
            self.push(at, glyph)?;
            self.consumed += 1;
            read += 1;
        }
        if self.consumed == self.glyph_count && self.status != MarkdownGlyphClusterStatus::Ready {
            // 真实数组EOF才结算最后cluster和被shaper移除的零glyph源范围。
            self.commit(self.source_range.end)?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownGlyphClusterError::Poisoned)?;
            payload.index.seal()?;
            self.guard(job)?; // seal的真实IO归还后再核原job，不凭本地计数签Ready。
            self.status = MarkdownGlyphClusterStatus::Ready;
        }
        Ok(MarkdownGlyphClusterStep {
            glyphs: read,
            clusters: self.count() - before,
            status: self.status,
        })
    }

    fn push(&mut self, at: usize, glyph: MarkdownShapedGlyph) -> Result<()> {
        if !self.source_range.contains(&glyph.source_utf16_cluster) {
            return Err(MarkdownGlyphClusterError::InconsistentGlyphs);
        }
        let previous = self
            .payload
            .as_ref()
            .and_then(|payload| payload.pending.as_ref())
            .map(|cluster| cluster.source_utf16.start);
        if let Some(previous) = previous {
            if glyph.source_utf16_cluster < previous {
                return Err(MarkdownGlyphClusterError::InconsistentGlyphs); // MonotoneGraphemes的方向合同被破坏。
            }
            if glyph.source_utf16_cluster > previous {
                self.commit(glyph.source_utf16_cluster)?;
            }
        } else if self.committed_end < glyph.source_utf16_cluster {
            // 开头被移除的默认忽略字符仍有真实源坐标；它没有可画glyph，不能凭空补一个。
            self.commit(glyph.source_utf16_cluster)?;
        }
        let pending = self
            .payload
            .as_mut()
            .ok_or(MarkdownGlyphClusterError::Poisoned)?
            .pending
            .get_or_insert_with(|| MarkdownGlyphCluster {
                source_utf16: glyph.source_utf16_cluster..glyph.source_utf16_cluster,
                glyphs: at..at + 1,
                prefix_x_26_6: self.prefix_x,
                prefix_y_26_6: self.prefix_y,
                x_advance_26_6: 0,
                y_advance_26_6: 0,
                flags: 0,
                missing_glyphs: 0,
            });
        // 一个cluster的glyph在原数组连续；RTL只改变读取方向，不改变此半开范围。
        pending.glyphs.start = pending.glyphs.start.min(at);
        pending.glyphs.end = pending.glyphs.end.max(at + 1);
        pending.x_advance_26_6 = pending
            .x_advance_26_6
            .checked_add(i64::from(glyph.x_advance))
            .ok_or(MarkdownGlyphClusterError::Overflow)?;
        pending.y_advance_26_6 = pending
            .y_advance_26_6
            .checked_add(i64::from(glyph.y_advance))
            .ok_or(MarkdownGlyphClusterError::Overflow)?;
        pending.flags |= glyph.flags;
        pending.missing_glyphs += usize::from(glyph.glyph_id == 0);
        Ok(())
    }

    fn commit(&mut self, end: usize) -> Result<()> {
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownGlyphClusterError::Poisoned)?;
        let mut value = payload.pending.take().unwrap_or(MarkdownGlyphCluster {
            source_utf16: self.committed_end..self.committed_end,
            glyphs: if self.backwards {
                self.glyph_count..self.glyph_count
            } else {
                0..0
            },
            prefix_x_26_6: self.prefix_x,
            prefix_y_26_6: self.prefix_y,
            x_advance_26_6: 0,
            y_advance_26_6: 0,
            flags: 0,
            missing_glyphs: 0,
        });
        if end == value.source_utf16.start && self.source_range.is_empty() {
            return Ok(()); // 空run没有cluster，真实EOF由Ready单独表达。
        }
        if value.source_utf16.start != self.committed_end
            || end <= self.committed_end
            || end > self.source_range.end
        {
            return Err(MarkdownGlyphClusterError::InconsistentGlyphs);
        }
        value.source_utf16.end = end;
        // 累计保留有符号advance；负kerning不能被夹成零或当成单调宽度证明。
        let next_x = self
            .prefix_x
            .checked_add(value.x_advance_26_6)
            .ok_or(MarkdownGlyphClusterError::Overflow)?;
        let next_y = self
            .prefix_y
            .checked_add(value.y_advance_26_6)
            .ok_or(MarkdownGlyphClusterError::Overflow)?;
        payload.index.append(encode(&value)?)?;
        self.prefix_x = next_x;
        self.prefix_y = next_y;
        self.committed_end = end;
        Ok(())
    }

    /// 完整索引按原逻辑源顺序逐条交付；调用方不必把已知ordinal重新二分成同一条记录。
    #[cfg(any(feature = "paged-line-compose", test))]
    pub(crate) fn at_ordinal(
        &mut self,
        job: &mut MarkdownShapingJob,
        ordinal: usize,
    ) -> Result<Option<MarkdownGlyphCluster>> {
        let result = (|| {
            self.guard(job)?; // 同一原job的源/字体失败先于本地Ready或ordinal判断。
            if self.status != MarkdownGlyphClusterStatus::Ready {
                return Err(MarkdownGlyphClusterError::InvalidRequest);
            } // 未封存的待决cluster不能因已经有一段前缀而取得完整顺序读取权限。
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownGlyphClusterError::Poisoned)?;
            if ordinal > payload.index.len() {
                return Err(MarkdownGlyphClusterError::InvalidRequest);
            } // 只允许恰好位于真实记录末端的EOF，越界不能冒充空结果。
            let value = if ordinal == payload.index.len() {
                None
            } else {
                // 只读原索引的一条固定record；RTL和零glyph范围仍沿原commit顺序，不推算cluster。
                Some(decode(payload.index.read(ordinal)?)?)
            };
            self.guard(job)?; // 页IO及回收完成后再核原权限，EOF也不能绕过真实源失效。
            Ok(value)
        })();
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail(error), // 保留真实首因并立即交还这个索引的FD/map。
        }
    }

    /// 查询一个原全局UTF16单元所在的真实shaping cluster；未完成尾部始终Pending。
    pub fn containing(
        &mut self,
        job: &mut MarkdownShapingJob,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        let result = self.containing_inner(job, offset);
        match result {
            Ok(answer) => Ok(answer),
            Err(error) => self.fail(error),
        }
    }

    fn containing_inner(
        &mut self,
        job: &mut MarkdownShapingJob,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        self.guard(job)?;
        if offset < self.source_range.start || offset > self.source_range.end {
            return Err(MarkdownGlyphClusterError::InvalidRequest);
        }
        if offset >= self.committed_end {
            // 完整EOF确定无记录；同一位置在未封存时不能被解释为空文档。
            return Ok(if self.status == MarkdownGlyphClusterStatus::Ready {
                MarkdownGlyphClusterAnswer::Known(None)
            } else {
                MarkdownGlyphClusterAnswer::Pending
            });
        }
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownGlyphClusterError::Poisoned)?;
        let mut low = 0;
        let mut high = payload.index.len();
        while low < high {
            // 已提交源范围严格递增，二分至多usize::BITS步；不按glyph数量分配目录。
            let middle = low + (high - low) / 2;
            let value = decode(payload.index.read(middle)?)?;
            if offset < value.source_utf16.start {
                high = middle;
            } else if offset >= value.source_utf16.end {
                low = middle + 1;
            } else {
                self.guard(job)?; // 真实页读取与回收之后核原job，不能发布失权缓存。
                return Ok(MarkdownGlyphClusterAnswer::Known(Some(value)));
            }
        }
        Err(MarkdownGlyphClusterError::InconsistentGlyphs)
    }

    fn count(&self) -> usize {
        self.payload
            .as_ref()
            .map_or(0, |payload| payload.index.len())
    }
    #[must_use]
    pub fn status(&self) -> MarkdownGlyphClusterStatus {
        self.status
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownGlyphClusterResourceObserver {
        self.resources.clone()
    }
    pub fn cancel(&mut self) {
        if self.payload.is_some() {
            self.status = MarkdownGlyphClusterStatus::Cancelled;
            drop(self.payload.take()); // 元数据及时归还，原job仍由其真实owner独立决定生命周期。
        }
    }
}

fn encode(value: &MarkdownGlyphCluster) -> Result<[u8; 64]> {
    let mut bytes = [0; 64];
    for (slot, value) in [
        value.source_utf16.start,
        value.source_utf16.end,
        value.glyphs.start,
        value.glyphs.end,
        value.missing_glyphs,
    ]
    .into_iter()
    .enumerate()
    {
        let value = u32::try_from(value).map_err(|_| MarkdownGlyphClusterError::Overflow)?;
        bytes[slot * 4..slot * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[20..24].copy_from_slice(&value.flags.to_le_bytes());
    for (slot, value) in [
        value.prefix_x_26_6,
        value.prefix_y_26_6,
        value.x_advance_26_6,
        value.y_advance_26_6,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[24 + slot * 8..32 + slot * 8].copy_from_slice(&value.to_le_bytes());
    }
    Ok(bytes) // 尾部8B固定保留为零，不存地址、正文或不稳定枚举布局。
}

fn decode(bytes: [u8; 64]) -> Result<MarkdownGlyphCluster> {
    let small = |slot: usize| {
        usize::try_from(u32::from_le_bytes(
            bytes[slot * 4..slot * 4 + 4]
                .try_into()
                .expect("固定4B字段"),
        ))
        .map_err(|_| MarkdownGlyphClusterError::Overflow)
    };
    let signed = |slot: usize| {
        i64::from_le_bytes(
            bytes[24 + slot * 8..32 + slot * 8]
                .try_into()
                .expect("固定8B字段"),
        )
    };
    let value = MarkdownGlyphCluster {
        source_utf16: small(0)?..small(1)?,
        glyphs: small(2)?..small(3)?,
        missing_glyphs: small(4)?,
        flags: u32::from_le_bytes(bytes[20..24].try_into().expect("固定4B字段")),
        prefix_x_26_6: signed(0),
        prefix_y_26_6: signed(1),
        x_advance_26_6: signed(2),
        y_advance_26_6: signed(3),
    };
    if value.source_utf16.start >= value.source_utf16.end
        || value.glyphs.start > value.glyphs.end
        || value.missing_glyphs > value.glyphs.len()
        || bytes[56..].iter().any(|byte| *byte != 0)
    {
        return Err(MarkdownGlyphClusterError::InconsistentGlyphs);
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
