//! 解析边界的唯一真实job交接；只读spool与后继Measure都消费相同的子job，不开放可替换Job借用。
use std::{ops::Range, sync::Weak};

use super::*;
use crate::{
    MarkdownGlyphClusterStep, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownShapedSpool, MarkdownShapedSpoolBuilder,
    MarkdownShapedSpoolStep, MarkdownSoftLineMeasureBudget, MarkdownSoftLineMeasureRequest,
    MarkdownSoftLineMeasureStep,
};

/// 只有实际解析成功的余项能构造；同坐标、同字体名或公开Boundary副本均不能产生此值。
#[derive(Clone)]
pub(crate) struct MarkdownResolvedLineStart {
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    shape: Weak<()>,
    at: usize,
}
impl MarkdownResolvedLineStart {
    pub(crate) fn require(
        &self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        shape: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        at: usize,
    ) -> std::result::Result<(), MarkdownSoftLineMeasureError> {
        source.health()?;
        if self.source != *authority || !source.permits(authority) {
            return Err(MarkdownSoftLineMeasureError::SourceMismatch);
        }
        if !Weak::ptr_eq(&self.shape, &shape.index_identity()) {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        }
        let profile = shape.profile()?;
        shape.glyph(authority, font, profile.glyph_count)?;
        shape.glyph(&self.source, &self.font, profile.glyph_count)?;
        // FontAuthority不按名字或字段比较；两张票都必须由同一个真实job的原font owner接受。
        if at != self.at || shape.source_range().start != at {
            return Err(MarkdownSoftLineMeasureError::InvalidRequest);
        }
        match breaks.grapheme_query(authority, at, MarkdownGraphemeQuery::IsBoundary)? {
            MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == at => {}
            _ => return Err(MarkdownSoftLineMeasureError::InvalidRequest),
        } // 原GCB仍须真实已知；不能把私有proof变成跳过Unicode边界检查的通用开关。
        source.health()?;
        Ok(())
    }
}

/// 解析后的实际prefix与可选tail；没有Clone，也没有公开字段或按坐标重建owner的构造器。
pub struct MarkdownSoftLineResolvedParts {
    boundary: MarkdownSoftLineResolvedBoundary,
    line: MarkdownSoftLineResolvedShape,
    remainder: Option<MarkdownSoftLineResolvedShape>,
}
impl MarkdownSoftLineResolvedParts {
    pub(super) fn new(
        boundary: MarkdownSoftLineResolvedBoundary,
        line: MarkdownSoftLineResolvedShape,
        remainder: Option<MarkdownSoftLineResolvedShape>,
    ) -> Self {
        Self {
            boundary,
            line,
            remainder,
        }
    }

    /// 消费唯一交接包。坐标副本只是事实，真正行首权限仍封在具体余项owner中。
    pub fn into_parts(
        self,
    ) -> (
        MarkdownSoftLineResolvedBoundary,
        MarkdownSoftLineResolvedShape,
        Option<MarkdownSoftLineResolvedShape>,
    ) {
        (self.boundary, self.line, self.remainder)
    }
}

/// worker局部实际子job；原Job为!Send，不能把本类型强行移入另一个线程或Native主线程。
pub struct MarkdownSoftLineResolvedShape {
    owned: Option<OwnedShape>,
    source: Option<MarkdownLayoutTextSource>,
    authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    shape: Weak<()>,
    start: Option<MarkdownResolvedLineStart>,
    backing: Option<MarkdownOriginalBackingObserver>,
}
impl MarkdownSoftLineResolvedShape {
    pub(super) fn new(
        mut owned: OwnedShape,
        source: MarkdownLayoutTextSource,
        authority: MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFontAuthority,
        resolved_start: bool,
    ) -> Result<Self> {
        source.health()?;
        let job = owned.job()?;
        let profile = job.profile()?;
        job.glyph(&authority, &font, profile.glyph_count)?;
        let shape = job.index_identity();
        let start = resolved_start.then(|| MarkdownResolvedLineStart {
            source: authority.clone(),
            font: font.clone(),
            shape: shape.clone(),
            at: job.source_range().start,
        });
        let backing = source.original_backing_observer();
        Ok(Self {
            owned: Some(owned),
            source: Some(source),
            authority,
            font,
            shape,
            start,
            backing,
        })
    }

    fn guard(&mut self) -> Result<()> {
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let source = self
            .source
            .as_ref()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        source.health()?;
        let owned = self
            .owned
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        if owned.resources.failed.load(Ordering::Acquire) {
            return Err(MarkdownSoftLineResolveError::ResourceCleanup);
        }
        let job = owned.job()?;
        if !Weak::ptr_eq(&self.shape, &job.index_identity()) {
            return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
        }
        let profile = job.profile()?;
        job.glyph(&self.authority, &self.font, profile.glyph_count)?;
        Ok(())
    }

    /// 仅本模块固定方法能借可变job；调用方无法mem::replace掉真实被登记的资源身份。
    fn with_job<T>(
        &mut self,
        visit: impl FnOnce(
            &mut MarkdownShapingJob,
            &MarkdownLayoutTextSource,
            &MarkdownLayoutTextAuthority,
            &MarkdownLayoutFontAuthority,
        ) -> Result<T>,
    ) -> Result<T> {
        let result = (|| {
            self.guard()?;
            let source = self
                .source
                .as_ref()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            let job = self
                .owned
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?
                .job()?;
            visit(job, source, &self.authority, &self.font)
        })();
        let result = match self.source.as_ref().map(MarkdownLayoutTextSource::health) {
            Some(Err(error)) => Err(error.into()),
            _ => result.and_then(|answer| {
                self.guard()?;
                Ok(answer)
            }),
        };
        if result.is_err() {
            drop(self.owned.take());
            drop(self.source.take());
        } // 失败实际归还job和Source，不保留一个可再次签发的旧immutable权限壳。
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        result
    }

    /// 只供合成控制计入真实输入与邻接context；不以工作计数代替Native验收。
    #[cfg(all(test, feature = "paged-safe-glyph-slices"))]
    pub(super) fn profile_for_test(&mut self) -> Result<crate::MarkdownShapingProfile> {
        self.with_job(|job, _, _, _| Ok(job.profile()?))
    }

    pub fn source_range(&mut self) -> Result<Range<usize>> {
        self.with_job(|job, _, _, _| Ok(job.source_range()))
    }
    pub fn glyph(&mut self, ordinal: usize) -> Result<Option<MarkdownShapedGlyph>> {
        self.with_job(|job, _, authority, font| Ok(job.glyph(authority, font, ordinal)?))
    }
    /// 封闭Store producer核原Source/Font后读取实际Ready事实；不借出可替换的Job。
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn require_authorities(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<crate::MarkdownShapingProfile> {
        self.with_job(|job, _, _, _| {
            let facts = job.profile()?;
            job.glyph(source, font, facts.glyph_count)?;
            Ok(facts)
        }) // 真EOF原语验证两张原票；同名字体与同字节Source都不会重新绑定具体child。
    }
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn instance_identity(&self) -> Weak<()> {
        self.shape.clone()
    }
    pub fn cluster_index(&mut self) -> Result<MarkdownGlyphClusterIndex> {
        self.with_job(|job, _, authority, font| {
            Ok(MarkdownGlyphClusterIndex::new(job, authority, font)?)
        })
    }
    pub fn step_clusters(
        &mut self,
        index: &mut MarkdownGlyphClusterIndex,
        budget: usize,
    ) -> Result<MarkdownGlyphClusterStep> {
        self.with_job(|job, _, _, _| Ok(index.step(job, budget)?))
    }
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn containing_cluster(
        &mut self,
        index: &mut MarkdownGlyphClusterIndex,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        self.with_job(|job, _, _, _| Ok(index.containing(job, offset)?))
    } // 索引仍以实际child nonce核验，不能用原item或相同glyph的新索引替代。

    /// 只有真实tail自带的已解析行首可开始下一行；不接受外部start字段。
    pub fn measure(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        width_26_6: i64,
        cancellation: MarkdownSoftLineMeasureCancellation,
    ) -> Result<MarkdownSoftLineMeasureJob> {
        let proof = self.start.clone();
        self.with_job(move |job, source, authority, font| {
            let proof = proof.ok_or(MarkdownSoftLineResolveError::InvalidRequest)?;
            Ok(MarkdownSoftLineMeasureJob::after_resolved_start(
                source.clone(),
                authority,
                font,
                job,
                breaks,
                MarkdownSoftLineMeasureRequest {
                    start_utf16: proof.at,
                    width_26_6,
                    cancellation,
                },
                &proof,
            )?)
        })
    }
    pub fn step_measure(
        &mut self,
        measure: &mut MarkdownSoftLineMeasureJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> Result<MarkdownSoftLineMeasureStep> {
        self.with_job(|job, _, _, _| Ok(measure.step(job, clusters, breaks, budget)?))
    }

    pub fn resolver(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
    ) -> Result<MarkdownSoftLineResolveJob> {
        self.with_job(|job, _, _, _| MarkdownSoftLineResolveJob::new(measure, job))
    }
    pub fn step_resolver(
        &mut self,
        resolver: &mut MarkdownSoftLineResolveJob,
        measure: &MarkdownSoftLineMeasureJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownSoftLineResolveStep> {
        self.with_job(|job, _, _, _| resolver.step(measure, job, clusters, breaks, budget))
    }

    pub fn take_resolved_parts(
        &mut self,
        resolver: MarkdownSoftLineResolveJob,
        measure: &MarkdownSoftLineMeasureJob,
    ) -> Result<MarkdownSoftLineResolvedParts> {
        self.with_job(|job, _, _, _| resolver.take_parts(measure, job))
    }

    /// 原只读spool builder直接读取本真实job；无第二个shaper或新正文镜像。
    pub fn spool_builder(&mut self) -> Result<MarkdownShapedSpoolBuilder> {
        self.with_job(|job, _, authority, font| {
            Ok(MarkdownShapedSpoolBuilder::new(job, authority, font)?)
        })
    }
    pub fn step_spool(
        &mut self,
        builder: &mut MarkdownShapedSpoolBuilder,
        budget: usize,
    ) -> Result<MarkdownShapedSpoolStep> {
        self.with_job(|job, _, _, _| Ok(builder.step(job, budget)?))
    }
    pub fn seal_spool(
        &mut self,
        builder: MarkdownShapedSpoolBuilder,
    ) -> Result<MarkdownShapedSpool> {
        self.with_job(|job, _, _, _| Ok(builder.seal(job)?))
    }

    pub fn close(self) -> Result<()> {
        let observer = self.owned.as_ref().map(|owned| owned.observer.clone());
        let backing = self.backing.clone();
        drop(self); // 实际归还本job，不把ownership transfer计成析构。
        if let Some(errno) = backing.as_ref().and_then(|observer| observer.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let facts = observer
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?
            .snapshot();
        if facts.files_open != 0
            || facts.mappings_open != 0
            || facts.mapped_bytes != 0
            || facts.cleanup_failed
            || facts.first_error.is_some()
        {
            return Err(MarkdownSoftLineResolveError::ResourceCleanup);
        }
        Ok(())
    }
}

/// 只有原selector内部Reusable阶段能构造；公开边界副本或任意Range不能重签安全换行。
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) struct MarkdownEmergencyResolvedChoice<'a> {
    seed: &'a MarkdownSoftLineResolveSeed,
    end: usize,
    width: i64,
}
#[cfg(feature = "paged-safe-glyph-slices")]
impl<'a> MarkdownEmergencyResolvedChoice<'a> {
    pub(super) fn new(payload: &'a Payload) -> Result<Self> {
        if !payload.reuse_safe
            || !matches!(payload.stage, Stage::Reusable)
            || payload.selected_end <= payload.seed.start
            || payload.selected_end > payload.limit
        {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        Ok(Self {
            seed: &payload.seed,
            end: payload.selected_end,
            width: payload
                .expected_width
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?,
        })
    }
    pub(crate) fn seed(&self) -> &MarkdownSoftLineResolveSeed {
        self.seed
    }
    pub(crate) fn range(&self) -> Range<usize> {
        self.seed.start..self.end
    }
    pub(crate) fn width(&self) -> i64 {
        self.width
    }
}
