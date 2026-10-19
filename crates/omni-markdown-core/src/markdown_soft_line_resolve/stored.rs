//! 已认证Store目录的真实前缀重排；复用原selector，不构造冒充原输出的Job或Measure。
use super::*;
use crate::markdown_soft_line_measure::provider::{
    GlyphProviderOrigin, MeasuredLineScope, ResolutionMeasurePermit,
};
use std::sync::Weak;

/// 仅是已认证包的事实副本；Store真正提交行仍须重新借同一签包，而不是收Range与字形数组。
pub(crate) struct MarkdownStoredResolvedPrefixFacts {
    pub boundary: MarkdownEmergencyShapedPrefixBoundary,
    pub source: crate::MarkdownLayoutTextAuthority,
    pub font: crate::MarkdownLayoutFontAuthority,
    pub origin: GlyphProviderOrigin,
    pub line: MeasuredLineScope,
}

/// 唯一构造点是原Stored selector真正选定的child交接；没有公开字段、Clone或Range工厂。
/// 签包保原整行弱scope及一个实际child，不保原item Job、Store文件或N个历史receipt。
pub(crate) struct MarkdownStoredResolvedPrefix {
    boundary: MarkdownEmergencyShapedPrefixBoundary,
    source: crate::MarkdownLayoutTextAuthority,
    font: crate::MarkdownLayoutFontAuthority,
    origin: GlyphProviderOrigin,
    line: MeasuredLineScope,
    child_identity: Weak<()>,
    child: Option<MarkdownSoftLineResolvedShape>,
    backing: Option<MarkdownOriginalBackingObserver>,
}
impl MarkdownStoredResolvedPrefix {
    fn check(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<()> {
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let child = self
            .child
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let profile = child.require_authorities(&self.source, &self.font)?;
        let current = permit.facts(provider)?;
        if self.source != current.source
            || self.origin != current.origin
            || !Weak::ptr_eq(&self.line.owner, &current.line.owner)
            || !Arc::ptr_eq(&self.line.alive, &current.line.alive)
            || !self
                .line
                .cancellation
                .same_request(&current.line.cancellation)
            || !Weak::ptr_eq(&self.child_identity, &child.instance_identity())
        {
            return Err(MarkdownSoftLineMeasureError::JobMismatch.into());
        } // 相同坐标/字形数、同名font、新Measure或别的Store目录不能签当前prefix。
        if !self.line.alive.load(Ordering::Acquire)
            || self.line.owner.strong_count() == 0
            || self.line.cancellation.is_cancelled()
        {
            return Err(MarkdownSoftLineMeasureError::Cancelled.into());
        } // 取消或原整行析构只撤权限；后面finish才真实归还本包实际child。
        child.require_authorities(&current.source, &current.font)?;
        if profile.glyph_count != self.boundary.glyphs
            || child.source_range()? != self.boundary.source_utf16
        {
            return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
        }
        Ok(()) // glyph宽度与flags来自同一个不可替换Ready child此前的完整有限扫描。
    }
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            drop(self.child.take());
        } // 身份/IO/取消失败后永久失效，不能再把旧包借给另一个原请求重试。
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        result
    }
    pub(crate) fn require(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<()> {
        let result = self.check(permit, provider);
        self.finish(result)
    }
    fn with_child<T>(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        visit: impl FnOnce(&mut MarkdownSoftLineResolvedShape) -> Result<T>,
    ) -> Result<T> {
        let result = (|| {
            self.check(permit, provider)?;
            let value = visit(
                self.child
                    .as_mut()
                    .ok_or(MarkdownSoftLineResolveError::Poisoned)?,
            )?;
            self.check(permit, provider)?;
            Ok(value)
        })();
        self.finish(result) // 每个真实getter都前后核同原票，不把首次认证变成永久cache权限。
    }
    pub(crate) fn facts(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<MarkdownStoredResolvedPrefixFacts> {
        self.require(permit, provider)?;
        Ok(MarkdownStoredResolvedPrefixFacts {
            boundary: self.boundary.clone(),
            source: self.source.clone(),
            font: self.font.clone(),
            origin: self.origin.clone(),
            line: self.line.clone(),
        })
    }
    pub(crate) fn glyph(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        ordinal: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        self.with_child(permit, provider, |child| child.glyph(ordinal))
    }
    pub(crate) fn cluster_index(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<MarkdownGlyphClusterIndex> {
        self.with_child(
            permit,
            provider,
            MarkdownSoftLineResolvedShape::cluster_index,
        )
    }
    pub(crate) fn step_clusters(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        index: &mut MarkdownGlyphClusterIndex,
        budget: usize,
    ) -> Result<crate::MarkdownGlyphClusterStep> {
        self.with_child(permit, provider, |child| child.step_clusters(index, budget))
    }
    pub(crate) fn containing_cluster(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        index: &mut MarkdownGlyphClusterIndex,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        self.with_child(permit, provider, |child| {
            child.containing_cluster(index, offset)
        })
    }
    pub(crate) fn close(mut self) -> Result<()> {
        let result = self
            .child
            .take()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)
            .and_then(MarkdownSoftLineResolvedShape::close);
        self.finish(result) // 真实child.close负责实际映射/FD归还，签包不合成零资源收据。
    }
}

/// 只保原selector与一个尚未归还的真实child收据；未解析余文只有私有FIFO坐标。
/// 原Measure始终由调用方持有，每次重新借回临时permit与同一目录，不能落盘重建权限。
pub(crate) struct MarkdownStoredPrefixResolveJob {
    inner: MarkdownSoftLineResolveJob,
    complete: Option<(usize, bool)>,
    prefix_outstanding: bool,
}
impl MarkdownStoredPrefixResolveJob {
    pub(crate) fn new(
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<Self> {
        let seed = permit.seed(provider)?;
        let mut inner =
            MarkdownSoftLineResolveJob::from_seed(seed, provider.shaping_cancellation())?;
        inner
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?
            .defer_tail = true;
        inner.guard_provider(permit.measure(), provider)?;
        Ok(Self {
            inner,
            complete: None,
            prefix_outstanding: false,
        }) // 原选择跨item时from_seed保留NextItem，绝不拿当前字体/目录重签历史选择。
    }

    pub(crate) fn cancellation(&self) -> MarkdownSoftLineResolveCancellation {
        self.inner.cancellation()
    }
    pub(crate) fn resource_observer(&self) -> MarkdownSoftLineResolveResourceObserver {
        self.inner.resource_observer()
    }
    fn guard(
        &self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<()> {
        // permit核真实选中阶段，inner另核构造时原Measure的nonce；新测量即使同源同范围
        // 也不能替代。两门都沿原Source健康先行，不用缓存目录长度盖掉IO失败。
        permit.require(provider)?;
        self.inner.guard_provider(permit.measure(), provider)
    }
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            drop(self.inner.payload.take());
        }
        if let Some(errno) = self
            .inner
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        } // 失败析构中的真实归还错误也优先；不能用外层控制错误或取消掩盖源失败。
        result
    }

    pub(crate) fn step(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownEmergencyGlyphSliceStep> {
        let result = self.advance(permit, provider, breaks, budget);
        self.finish(result)
    }
    fn advance(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownEmergencyGlyphSliceStep> {
        self.guard(permit, provider)?;
        if budget.input_bytes == 0
            || budget.input_scalars == 0
            || budget.glyphs == 0
            || budget.candidates == 0
        {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        if self.prefix_outstanding {
            if self.inner.resources.snapshot().active_jobs != 0 {
                return Ok(empty(
                    MarkdownEmergencyGlyphSliceAnswer::PendingPrefixConsumer,
                ));
            }
            self.prefix_outstanding = false;
        } // 先前交接包必须真正释放；不得一边保留N个child，一边宣称续行资源有界。
        if let Some((end, eof)) = self.complete {
            return Ok(empty(MarkdownEmergencyGlyphSliceAnswer::PrefixComplete {
                source_utf16_end: end,
                source_eof: eof,
            }));
        }
        let step = self
            .inner
            .step_provider(permit.measure(), provider, breaks, budget)?;
        let payload = self
            .inner
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let answer = if matches!(payload.stage, Stage::ShapedPrefix) {
            MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(emergency::prefix_boundary(payload)?)
        } else {
            match step.answer {
                MarkdownSoftLineResolveAnswer::Yielded => {
                    MarkdownEmergencyGlyphSliceAnswer::Yielded
                }
                MarkdownSoftLineResolveAnswer::PendingDependencies => {
                    MarkdownEmergencyGlyphSliceAnswer::PendingDependencies
                }
                MarkdownSoftLineResolveAnswer::Pending(reason) => {
                    MarkdownEmergencyGlyphSliceAnswer::Pending(reason)
                }
                MarkdownSoftLineResolveAnswer::EmptySource
                | MarkdownSoftLineResolveAnswer::Resolved(_) => {
                    return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
                } // 此入口只交真实非空前缀，完整tail或空文档不是一次unsafe解析成功。
            }
        };
        self.guard(permit, provider)?;
        Ok(MarkdownEmergencyGlyphSliceStep {
            input_bytes: step.input_bytes,
            glyphs: step.glyphs,
            candidates: step.candidates,
            shapes: step.shapes,
            answer,
        })
    }

    pub(crate) fn take_shaped_prefix(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<(
        MarkdownEmergencyShapedPrefixBoundary,
        MarkdownSoftLineResolvedShape,
    )> {
        let result = (|| {
            self.guard(permit, provider)?;
            if self.prefix_outstanding {
                return Err(MarkdownSoftLineResolveError::InvalidRequest);
            }
            let payload = self
                .inner
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            let boundary = emergency::prefix_boundary(payload)?;
            let line = payload
                .line
                .take()
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
            let line = MarkdownSoftLineResolvedShape::new(
                line,
                payload.seed.source.clone(),
                payload.seed.authority.clone(),
                payload.seed.font.clone(),
                false,
            )?;
            self.complete = emergency::advance_source_start(payload, boundary.source_utf16.end)?;
            self.prefix_outstanding = true;
            self.guard(permit, provider)?;
            Ok((boundary, line))
        })();
        self.finish(result)
    }
    /// Store只接收此封闭包写行；公开Boundary加任意已重排shape不能替代本原选择。
    pub(crate) fn take_signed_prefix(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<MarkdownStoredResolvedPrefix> {
        let result = (|| {
            self.guard(permit, provider)?;
            let facts = permit.facts(provider)?;
            let (boundary, child) = self.take_shaped_prefix(permit, provider)?;
            let mut prefix = MarkdownStoredResolvedPrefix {
                boundary,
                source: facts.source,
                font: facts.font,
                origin: facts.origin,
                line: facts.line,
                child_identity: child.instance_identity(),
                child: Some(child),
                backing: self.inner.backing.clone(),
            };
            prefix.require(permit, provider)?;
            Ok(prefix)
        })();
        self.finish(result) // 原selector推进后若包认证失败则整体毒化，不能跳过失败prefix继续后行。
    }
    pub(crate) fn close(self) -> Result<MarkdownSoftLineResolveResources> {
        self.inner.close()
    }
}
fn empty(answer: MarkdownEmergencyGlyphSliceAnswer) -> MarkdownEmergencyGlyphSliceStep {
    MarkdownEmergencyGlyphSliceStep {
        input_bytes: 0,
        glyphs: 0,
        candidates: 0,
        shapes: 0,
        answer,
    }
}
