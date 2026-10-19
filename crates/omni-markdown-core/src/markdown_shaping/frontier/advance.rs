//! 同一实际字体全部默认水平advance的分步下界；此证明本身不授予软行或源切片权限。

use harfrust::{GlyphId, ShapePlan, ShapePlanKey, ShaperFont};
use std::sync::{Arc, Weak};

use super::{
    MarkdownLayoutFont, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownShapingCancellation, MarkdownShapingError, ValidatedOptions,
};
use crate::markdown_layout_font::frontier::FontFrontierPlan;
use crate::{MarkdownDocumentBufferError, MarkdownOriginalBackingObserver};

/// 单步最多查询128个真实glyph；全字体闭包与一次正文回调的工作量分别计费。
const ADVANCE_SCAN_STEP: usize = 128;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AdvanceScanError {
    #[error("原advance扫描的Source、Font或取消状态失效：{0}")]
    Shaping(#[from] MarkdownShapingError),
    #[error("原实际计划尚未证明无跨输入位置调整")]
    UnsupportedPlan,
    #[error("原实际计划不属于这组Font与完整整形选项")]
    PlanMismatch,
    #[error("原advance扫描的实际glyph元数据或单步配额无效")]
    InvalidRequest,
    #[error("原advance扫描尚未完成或已经撤销")]
    Incomplete,
}
type Result<T> = std::result::Result<T, AdvanceScanError>;

/// 构造只借同一个原Source和Font；不可用名称或复制公开范围替换这些身份。
pub(super) struct AdvanceScanOrigin<'a> {
    pub source: &'a MarkdownLayoutTextSource,
    pub source_authority: &'a MarkdownLayoutTextAuthority,
    pub font: &'a MarkdownLayoutFont,
    pub font_authority: &'a MarkdownLayoutFontAuthority,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdvanceScanAnswer {
    Pending,
    Complete,
    /// 一个真实负值足以拒绝非负后缀证明；不把它夹成0或忽略未扫描glyph。
    Negative {
        glyph: u32,
        advance_26_6: i32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AdvanceScanStep {
    pub answer: AdvanceScanAnswer,
    pub queried: usize,
    pub total_queried: u32,
}

/// 只持固定元数据与原Font，不保存源字节、glyph列表或正文索引。
pub(super) struct DefaultAdvanceScan {
    font: Option<MarkdownLayoutFont>,
    font_authority: MarkdownLayoutFontAuthority,
    source_authority: MarkdownLayoutTextAuthority,
    source_backing: Option<MarkdownOriginalBackingObserver>,
    plan: Arc<ShapePlan>,
    options: ValidatedOptions,
    cancellation: MarkdownShapingCancellation,
    count: u32,
    next: u32,
    minimum: i32,
    answer: AdvanceScanAnswer,
}

/// 只有扫描全部真实glyph后才能产生；弱票不会延长Font、计划或Source的生命期。
/// 未读文本的ASCII、稳定seam和真实GCB仍由原frontier单独认证。
pub(super) struct NonnegativeDefaultAdvanceProof {
    font: MarkdownLayoutFontAuthority,
    source: MarkdownLayoutTextAuthority,
    source_backing: Option<MarkdownOriginalBackingObserver>,
    plan: Weak<ShapePlan>,
    options: ValidatedOptions,
    glyphs: u32,
    minimum_26_6: i32,
    cancellation: MarkdownShapingCancellation,
}

fn same_options(left: &ValidatedOptions, right: &ValidatedOptions) -> bool {
    left.direction == right.direction
        && left.script == right.script
        && left.language == right.language
        && left.points.to_bits() == right.points.to_bits()
        && left.scale == right.scale
        && left.output_divisor == right.output_divisor
        && left.beginning_of_text == right.beginning_of_text
        && left.end_of_text == right.end_of_text
        && left.features == right.features
}

impl DefaultAdvanceScan {
    /// 实际计划先通过窄ASCII依赖门，再逐glyph查表；不从名字、当前前缀或抽样推导未来下界。
    pub(super) fn new(
        origin: AdvanceScanOrigin<'_>,
        options: &ValidatedOptions,
        plan_owner: &FontFrontierPlan,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        origin.source.health().map_err(MarkdownShapingError::from)?;
        if !origin.source.permits(origin.source_authority) {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        if !origin.font.permits(origin.font_authority) {
            return Err(MarkdownShapingError::FontMismatch.into());
        }
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        } // 先检查原Source首错，不能把真实EIO伪装为干净取消。
        if !plan_owner.permits_font(origin.font) {
            return Err(AdvanceScanError::PlanMismatch);
        }
        let plan = plan_owner.plan();
        let key = ShapePlanKey::new(origin.font.font(), Some(options.script), options.direction)
            .language(options.language.as_ref())
            .features(&options.features);
        if !key.matches(&plan) {
            return Err(AdvanceScanError::PlanMismatch);
        }
        plan.ascii_frontier_capability(origin.font.font())
            .map_err(|_| AdvanceScanError::UnsupportedPlan)?;
        let count = origin.font.metadata().glyph_count;
        if count == 0 || count > u32::from(u16::MAX) {
            return Err(AdvanceScanError::InvalidRequest);
        } // 实际maxp目录之外的编号不能被当作默认缺字值补入证明。
        origin.source.health().map_err(MarkdownShapingError::from)?;
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        Ok(Self {
            font: Some(origin.font.clone()),
            font_authority: origin.font_authority.clone(),
            source_authority: origin.source_authority.clone(),
            source_backing: origin.source.original_backing_observer(),
            plan,
            options: options.clone(),
            cancellation,
            count,
            next: 0,
            minimum: i32::MAX,
            answer: AdvanceScanAnswer::Pending,
        })
    }

    fn guard(
        &self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<()> {
        if let Some(errno) = self
            .source_backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(MarkdownShapingError::from(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )
            .into());
        } // 原Source故障不能被一份健康外来capture或后继取消掩盖。
        source.health().map_err(MarkdownShapingError::from)?;
        if authority != &self.source_authority || !source.permits(authority) {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        let font = self.font.as_ref().ok_or(AdvanceScanError::Incomplete)?;
        if !font.permits(&self.font_authority) {
            return Err(MarkdownShapingError::FontMismatch.into());
        }
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        Ok(())
    }

    /// 每个调用只做固定数量的真实默认metrics查询；调用方须把该成本计入原worker调度。
    pub(super) fn step(
        &mut self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        budget: usize,
    ) -> Result<AdvanceScanStep> {
        let result = (|| {
            self.guard(source, authority)?;
            if budget == 0 {
                return Err(AdvanceScanError::InvalidRequest);
            } // 零配额不算推进，也不能签发完整字体证明。
            let end = self
                .next
                .saturating_add(budget.min(ADVANCE_SCAN_STEP) as u32)
                .min(self.count);
            let start = self.next;
            if self.answer == AdvanceScanAnswer::Pending {
                let font = self.font.as_ref().ok_or(AdvanceScanError::Incomplete)?;
                let shaper = ShaperFont::new(font.font()).with_scale(self.options.scale);
                while self.next < end {
                    // 正文尚未读到的glyph也逐项覆盖；没有自定义FontFuncs或不同scale可混入。
                    let raw = shaper.default_glyph_h_advance(GlyphId::new(self.next));
                    let advance = self.options.quantize(raw);
                    let glyph = self.next;
                    self.next += 1;
                    self.minimum = self.minimum.min(advance);
                    if advance < 0 {
                        self.answer = AdvanceScanAnswer::Negative {
                            glyph,
                            advance_26_6: advance,
                        };
                        break;
                    } // 首个真实负值永久拒证；不能靠后续正值抵消来推导任意后缀非负。
                }
                if self.answer == AdvanceScanAnswer::Pending && self.next == self.count {
                    self.answer = AdvanceScanAnswer::Complete;
                }
            }
            self.guard(source, authority)?; // Font元数据读取后的原Source错误仍压过Ready和取消。
            Ok(AdvanceScanStep {
                answer: self.answer,
                queried: (self.next - start) as usize,
                total_queried: self.next,
            })
        })();
        if result.is_err() {
            drop(self.font.take()); // 任一失败永久归还本扫描的原Font，不允许换Source或选项重试。
        }
        result
    }

    /// 消费完整扫描；负值和未完成状态都不能拿到proof，失败后也没有可继续使用的扫描owner。
    pub(super) fn finish(
        self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<NonnegativeDefaultAdvanceProof> {
        self.guard(source, authority)?;
        if self.answer != AdvanceScanAnswer::Complete || self.next != self.count {
            return Err(AdvanceScanError::Incomplete);
        }
        Ok(NonnegativeDefaultAdvanceProof {
            font: self.font_authority.clone(),
            source: self.source_authority.clone(),
            source_backing: self.source_backing.clone(),
            plan: Arc::downgrade(&self.plan),
            options: self.options.clone(),
            glyphs: self.count,
            minimum_26_6: self.minimum,
            cancellation: self.cancellation.clone(),
        })
    }
}

impl NonnegativeDefaultAdvanceProof {
    /// 只给原任务核同一Font/计划/完整选项；这个bool不是源切片、紧急换行或Ready许可。
    pub(super) fn matches(
        &self,
        origin: AdvanceScanOrigin<'_>,
        options: &ValidatedOptions,
        plan_owner: &FontFrontierPlan,
    ) -> Result<bool> {
        if let Some(errno) = self
            .source_backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(MarkdownShapingError::from(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )
            .into());
        }
        origin.source.health().map_err(MarkdownShapingError::from)?;
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        } // 完成查表没有豁免原任务取消；旧proof不能被同Font后继操作重新启用。
        let plan = plan_owner.plan();
        Ok(self.source == *origin.source_authority
            && origin.source.permits(origin.source_authority)
            && origin.font.permits(&self.font)
            && origin.font.permits(origin.font_authority)
            && plan_owner.permits_font(origin.font)
            && self.plan.ptr_eq(&Arc::downgrade(&plan))
            && self.plan.strong_count() != 0
            && same_options(&self.options, options)
            && self.glyphs == origin.font.metadata().glyph_count
            && self.minimum_26_6 >= 0)
    }

    pub(super) fn minimum_26_6(&self) -> i32 {
        self.minimum_26_6
    }
}
