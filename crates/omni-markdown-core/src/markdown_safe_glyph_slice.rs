//! 原已测安全断点的字形切片；只交付原Ready数组的真实子范围，不重整形剩余文本。
//!
//! 切片不是任意Range许可。工厂只能读取同一个Measure内部已选机会，并重新核原GCB、
//! 原cluster端点与UNSAFE_TO_BREAK。复杂边界仍交回真实resolver，不伪造软行或NativeReady。
use crate::{
    MarkdownDocumentBufferError, MarkdownGlyphCluster, MarkdownGlyphClusterAnswer,
    MarkdownGlyphClusterError, MarkdownGlyphClusterIndex, MarkdownGlyphClusterResourceObserver,
    MarkdownGlyphClusterStatus, MarkdownGraphemeAnswer, MarkdownGraphemeQuery,
    MarkdownLayoutBreakError, MarkdownLayoutBreakJob, MarkdownLayoutFontAuthority,
    MarkdownLayoutTextAuthority, MarkdownOriginalBackingObserver, MarkdownShapingError,
    MarkdownShapingJob, MarkdownShapingStatus, MarkdownSoftLineMeasureCancellation,
    MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureError, MarkdownSoftLineMeasureJob,
    markdown_soft_line_measure::MarkdownSoftLineResolveSeed,
};
use std::{
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use thiserror::Error;
mod spool;
pub use spool::{
    MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS, MarkdownMeasuredItemSpoolProof, MarkdownSafeSpoolSlice,
    MarkdownSafeSpoolSliceError, MarkdownSafeSpoolSliceJob, MarkdownSafeSpoolSliceStep,
    MarkdownSafeSpoolSliceView,
};

const UNSAFE: u32 = harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits();

#[derive(Debug, Error)]
pub enum MarkdownSafeGlyphSliceError {
    #[error("安全切片必须来自原测量的实际安全断点")]
    NeedsResolution,
    #[error("安全切片依赖尚未完成")]
    PendingDependencies,
    #[error("安全切片不属于原任务或原测量")]
    OwnerMismatch,
    #[error("安全切片原测量已取消")]
    Cancelled,
    #[error("安全切片已经永久撤权")]
    Poisoned,
    #[error("安全切片坐标、字形或advance不一致")]
    InconsistentCoverage,
    #[error("安全切片坐标或advance溢出")]
    Overflow,
    #[error("安全切片原源失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("安全切片原字形失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("安全切片原测量失败")]
    Measure(#[from] MarkdownSoftLineMeasureError),
    #[error("安全切片原cluster索引失败")]
    Cluster(#[from] MarkdownGlyphClusterError),
    #[error("安全切片原断点索引失败")]
    Break(#[from] MarkdownLayoutBreakError),
}
type Result<T> = std::result::Result<T, MarkdownSafeGlyphSliceError>;

/// 只报告原输出中的事实；不代表Native坐标、字体回退、整行BiDi重排或屏幕可绘制权限。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSafeGlyphSliceFacts {
    pub source_utf16: Range<usize>,
    pub visual_glyphs: Range<usize>,
    pub x_advance_26_6: i64,
    pub y_advance_26_6: i64,
}

/// 不可Clone且字段私有；仅持弱权限与固定元数据，不延长Doc、Source、Font或索引FD寿命。
pub struct MarkdownSafeGlyphSlice {
    facts: MarkdownSafeGlyphSliceFacts,
    shape: Weak<()>,
    measure: Weak<()>,
    revoked: Arc<AtomicBool>,
    cancellation: MarkdownSoftLineMeasureCancellation,
    index: MarkdownGlyphClusterResourceObserver,
    backing: Option<MarkdownOriginalBackingObserver>,
    pub(crate) source: MarkdownLayoutTextAuthority,
    pub(crate) font: MarkdownLayoutFontAuthority,
    count: usize,
    generation: u64,
    poisoned: bool,
}
impl MarkdownSafeGlyphSlice {
    #[must_use]
    pub fn facts(&self) -> MarkdownSafeGlyphSliceFacts {
        self.facts.clone()
    }

    /// 每批和封口重借原job；原IO先于取消/弱票核验，失败不能用另一任务重新授权。
    pub(crate) fn require(&mut self, job: &mut MarkdownShapingJob) -> Result<()> {
        let result = (|| {
            if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
                return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
            }
            let profile = job.profile()?;
            job.glyph(&self.source, &self.font, self.count)?;
            if !Weak::ptr_eq(&self.shape, &job.index_identity())
                || profile.glyph_count != self.count
                || profile.source_generation != self.generation
            {
                return Err(MarkdownSafeGlyphSliceError::OwnerMismatch);
            }
            if self.cancellation.is_cancelled() {
                return Err(MarkdownSafeGlyphSliceError::Cancelled);
            }
            if self.poisoned
                || self.revoked.load(Ordering::Acquire)
                || self.measure.upgrade().is_none()
            {
                return Err(MarkdownSafeGlyphSliceError::Poisoned);
            }
            let index = self.index.snapshot();
            if index.owner_closed || index.cleanup_failed {
                return Err(MarkdownSafeGlyphSliceError::Poisoned);
            } // 原索引失效会真实close原owner；单页可始终无FD，observer也不能保活其buffer或重新打开。
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        } // 曾失权的切片不会因外部状态碰巧恢复而复活。
        result
    }

    /// 只消费原紧急selector私有proof；原Measure内部起点不变，续行起点由该selector封闭推进。
    pub(crate) fn from_emergency_selection(
        choice: &crate::markdown_soft_line_resolve::MarkdownEmergencyResolvedChoice<'_>,
        measure: &MarkdownSoftLineMeasureJob,
        job: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        index: &mut MarkdownGlyphClusterIndex,
    ) -> Result<Self> {
        let seed = choice.seed();
        seed.source.health()?;
        measure.require_resolve(&seed.owner, job)?;
        if index.status() != MarkdownGlyphClusterStatus::Ready {
            return Err(MarkdownSafeGlyphSliceError::PendingDependencies);
        }
        let facts = facts(seed, job, breaks, index, choice.range())?;
        if facts.x_advance_26_6 != choice.width() {
            return Err(MarkdownSafeGlyphSliceError::InconsistentCoverage);
        }
        let value = Self::make(seed, job, index, facts)?;
        seed.source.health()?;
        measure.require_resolve(&seed.owner, job)?;
        Ok(value)
    }

    pub(crate) fn cancellation(&self) -> MarkdownSoftLineMeasureCancellation {
        self.cancellation.clone()
    }

    fn make(
        seed: &MarkdownSoftLineResolveSeed,
        job: &mut MarkdownShapingJob,
        index: &MarkdownGlyphClusterIndex,
        facts: MarkdownSafeGlyphSliceFacts,
    ) -> Result<Self> {
        let profile = job.profile()?;
        let source = job.source_range();
        if facts.visual_glyphs.start > facts.visual_glyphs.end
            || facts.visual_glyphs.end > profile.glyph_count
            || facts.source_utf16.is_empty()
            || facts.source_utf16.start < source.start
            || facts.source_utf16.end > source.end
        {
            return Err(MarkdownSafeGlyphSliceError::InconsistentCoverage);
        } // 即使分页record发生损坏，原数组范围也必须先闭合，不能让后续索引加法越界或溢出。
        let mut value = Self {
            facts,
            shape: job.index_identity(),
            measure: seed.owner.clone(),
            revoked: Arc::clone(&seed.revoked),
            cancellation: seed.cancellation.clone(),
            index: index.resource_observer(),
            backing: seed.source.original_backing_observer(),
            source: seed.authority.clone(),
            font: seed.font.clone(),
            count: profile.glyph_count,
            generation: profile.source_generation,
            poisoned: false,
        };
        value.require(job)?;
        Ok(value)
    }
}

/// 原已选前缀及其同一输出余项；两者不创建新shaper，也不复制全部字形或正文。
pub struct MarkdownSafeGlyphSlices {
    line: MarkdownSafeGlyphSlice,
    remainder: Option<MarkdownSafeGlyphSlice>,
}
impl MarkdownSafeGlyphSlices {
    pub fn from_measure(
        measure: &MarkdownSoftLineMeasureJob,
        job: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        index: &mut MarkdownGlyphClusterIndex,
    ) -> Result<Self> {
        let seed = measure.resolve_seed(job)?;
        let MarkdownSoftLineMeasureChoice::MeasuredBreak(selected) = &seed.choice else {
            return Err(MarkdownSafeGlyphSliceError::NeedsResolution);
        }; // 不能让调用方用公开Choice字段把紧急断点、缺字或词典待决冒充安全机会。
        if job.status() != MarkdownShapingStatus::Ready
            || index.status() != MarkdownGlyphClusterStatus::Ready
        {
            return Err(MarkdownSafeGlyphSliceError::PendingDependencies);
        }
        let range = selected.source_utf16.clone();
        if range.start != seed.start
            || range.start < seed.item.start
            || range.end > seed.item.end
            || range.is_empty()
            || selected.boundary_flags & UNSAFE != 0
        {
            return Err(MarkdownSafeGlyphSliceError::NeedsResolution);
        }
        let line_facts = facts(&seed, job, breaks, index, range.clone())?;
        if line_facts.x_advance_26_6 != selected.x_advance_26_6 {
            return Err(MarkdownSafeGlyphSliceError::InconsistentCoverage);
        } // 原测量累计值必须等于真实cluster前缀差；跨item累计不能被误签为单item切片。
        let remainder_facts = if range.end < seed.item.end {
            Some(facts(&seed, job, breaks, index, range.end..seed.item.end)?)
        } else {
            None
        };
        measure.require_resolve(&seed.owner, job)?;
        let line = MarkdownSafeGlyphSlice::make(&seed, job, index, line_facts)?;
        let remainder = remainder_facts
            .map(|facts| MarkdownSafeGlyphSlice::make(&seed, job, index, facts))
            .transpose()?;
        seed.source.health()?; // 元数据IO后最后核真实Source，不能把共享映射错误藏在已构造候选内。
        Ok(Self { line, remainder })
    }
    #[must_use]
    pub fn into_parts(self) -> (MarkdownSafeGlyphSlice, Option<MarkdownSafeGlyphSlice>) {
        (self.line, self.remainder)
    }
}

fn cluster(
    index: &mut MarkdownGlyphClusterIndex,
    job: &mut MarkdownShapingJob,
    at: usize,
) -> Result<MarkdownGlyphCluster> {
    match index.containing(job, at)? {
        MarkdownGlyphClusterAnswer::Known(Some(value)) => Ok(value),
        MarkdownGlyphClusterAnswer::Pending => {
            Err(MarkdownSafeGlyphSliceError::PendingDependencies)
        }
        MarkdownGlyphClusterAnswer::Known(None) => {
            Err(MarkdownSafeGlyphSliceError::InconsistentCoverage)
        }
    }
}
fn facts(
    seed: &MarkdownSoftLineResolveSeed,
    job: &mut MarkdownShapingJob,
    breaks: &mut MarkdownLayoutBreakJob,
    index: &mut MarkdownGlyphClusterIndex,
    range: Range<usize>,
) -> Result<MarkdownSafeGlyphSliceFacts> {
    for at in [range.start, range.end] {
        match breaks.grapheme_query(&seed.authority, at, MarkdownGraphemeQuery::IsBoundary)? {
            MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == at => {}
            MarkdownGraphemeAnswer::Pending => {
                return Err(MarkdownSafeGlyphSliceError::PendingDependencies);
            }
            _ => return Err(MarkdownSafeGlyphSliceError::NeedsResolution),
        } // 原GCB索引确认两端；低代理项、组合簇内部和未知尾部都不能由range长度绕过。
    }
    let first = cluster(index, job, range.start)?;
    let last = cluster(index, job, range.end - 1)?;
    if first.source_utf16.start != range.start
        || last.source_utf16.end != range.end
        || (range.start != seed.item.start && first.flags & UNSAFE != 0)
    {
        return Err(MarkdownSafeGlyphSliceError::NeedsResolution);
    }
    if range.end < seed.item.end {
        let next = cluster(index, job, range.end)?;
        if next.source_utf16.start != range.end || next.flags & UNSAFE != 0 {
            return Err(MarkdownSafeGlyphSliceError::NeedsResolution);
        }
    } // UNSAFE_TO_BREAK属于断点后继cluster；不能只检查前缀自身flags并集。
    let x = last
        .prefix_x_26_6
        .checked_add(last.x_advance_26_6)
        .and_then(|end| end.checked_sub(first.prefix_x_26_6))
        .ok_or(MarkdownSafeGlyphSliceError::Overflow)?;
    let y = last
        .prefix_y_26_6
        .checked_add(last.y_advance_26_6)
        .and_then(|end| end.checked_sub(first.prefix_y_26_6))
        .ok_or(MarkdownSafeGlyphSliceError::Overflow)?;
    // RTL逻辑首尾的视觉数组区间顺序相反；只取原连续cluster的外包端点，不反转字形。
    let visual = first.glyphs.start.min(last.glyphs.start)..first.glyphs.end.max(last.glyphs.end);
    Ok(MarkdownSafeGlyphSliceFacts {
        source_utf16: range,
        visual_glyphs: visual,
        x_advance_26_6: x,
        y_advance_26_6: y,
    })
}

#[cfg(test)]
mod tests;
