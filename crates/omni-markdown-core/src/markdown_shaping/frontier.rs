//! 同一原计划的已认证有限前缀；未读item尾部始终没有glyph或软行权限。
//!
//! 此候选仅供显式协调器使用。ASCII局部支持域与真实单scalar GCB各自有封闭证书；
//! 一个前缀Ready不等于LineReady，多scalar GCB和未认证查表仍保留真实未解析状态。

mod advance;
pub(super) mod local;
mod positioned;
mod unicode;
mod word;
pub(super) use word::WordPrefixProof;

use std::{
    ops::Range,
    sync::{Arc, Weak},
};

use thiserror::Error;

use super::{options::ValidatedOptions, *};
use crate::{
    MarkdownGraphemeAnswer, MarkdownGraphemeQuery, MarkdownLayoutBreakError,
    MarkdownLayoutBreakJob, markdown_layout_font::frontier::FontFrontierPlan,
};

/// 一个中心固定最多256个已认证标量，另读真实支持输入；字节数不是新段落边界。
pub(crate) const MARKDOWN_SHAPING_FRONTIER_UNITS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkdownShapingFrontierReason {
    Plan(harfrust::AsciiFrontierRejection),
    NonAscii,
    WordBeyondBudget,
    NotGraphemeBoundary,
    UncertifiedWord,
    NegativeAdvance,
    NonSimpleGrapheme,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkdownShapingFrontierAnswer {
    Pending,
    PendingConsumer,
    PartReady,
    Complete,
    NeedsFrontier(MarkdownShapingFrontierReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MarkdownShapingFrontierStep {
    pub answer: MarkdownShapingFrontierAnswer,
    pub source_bytes: usize,
    pub input_bytes: usize,
    pub scalars: usize,
    pub context_bytes: usize,
    pub shapes: usize,
    pub metadata_glyphs: usize,
    pub plan_records: usize,
    pub boundary_records: usize,
}

#[derive(Debug, Error)]
pub(crate) enum MarkdownShapingFrontierError {
    #[error("前沿原字形阶段失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("前沿原字素权限失败")]
    Breaks(#[from] MarkdownLayoutBreakError),
    #[error("前沿续行票不属于原任务或阶段")]
    ContinuationMismatch,
    #[error("前沿任务已经永久撤权")]
    Poisoned,
    #[error("原字体非负闭包阶段失败")]
    Advance(#[from] advance::AdvanceScanError),
}
type FrontierResult<T> = std::result::Result<T, MarkdownShapingFrontierError>;

struct FrontierPayload {
    source: MarkdownLayoutTextSource,
    font: MarkdownLayoutFont,
    options: ValidatedOptions,
    plan: Option<FontFrontierPlan>,
}

/// 唯一owner保存未解析cursor；没有公开Range→部分item认证构造或重置入口。
pub(crate) struct MarkdownShapingFrontierJob {
    identity: Arc<()>,
    payload: Option<FrontierPayload>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    cancellation: MarkdownShapingCancellation,
    original: Range<usize>,
    original_bytes: Range<usize>,
    next: usize,
    next_byte: usize,
    serial: u64,
    scan: [u8; MARKDOWN_SHAPING_FRONTIER_UNITS + 1],
    scanned: usize,
    candidate: Option<usize>,
    child: Option<MarkdownShapingJob>,
    outstanding: Option<Weak<()>>,
    answer: MarkdownShapingFrontierAnswer,
    word_candidate: bool,
    word_source_checked: bool,
    word_closure_checked: bool,
    word_closure: Option<harfrust::AsciiGlyphClosureJob>,
    advance_scan: Option<advance::DefaultAdvanceScan>,
    nonnegative: Option<advance::NonnegativeDefaultAdvanceProof>,
    word_end_cursor: Option<crate::MarkdownLayoutBreakCursor>,
    local_source_checked: bool,
    local_source_cursor: Option<crate::markdown_document_buffer::FrozenPrintableAsciiSuffix>,
    local_source_bounds: Option<(u8, u8)>,
    local_scan: Option<harfrust::AsciiLocalFrontierJob>,
    local_capability: Option<harfrust::AsciiLocalFrontierCapability>,
    local_certificate: Option<harfrust::AsciiLocalFrontierCertificate>,
    positioned_scan: Option<harfrust::AsciiPositionedAdvanceJob>,
    positioned_proof: Option<positioned::PositionedNonnegativeProof>,
    local_verified: usize,
    resources: Option<crate::markdown_line_composer::worker_resources::ComposerResourceScope>,
    unicode: Option<Box<unicode::UnicodeState>>,
    // 开放SCX只授予独立字素中心，最后一个真实GCB始终作为未发布支持域。
    open_scope: bool,
    // 原 UBA 分项事实只裁定实际 shape 输入/上下文；从不登记为全局编辑 GCB。
    shaping_seams: [Option<crate::markdown_line_composer::ResolvedBidiShapingSeam>; 2],
    partial_grapheme: [bool; 2],
    context_bounds: Option<Range<usize>>,
}

/// 一次真实Ready子任务与唯一归还票；复制公开范围不能构造这个值。
pub(crate) struct MarkdownShapingFrontierPart {
    job: MarkdownShapingJob,
    continuation: MarkdownShapingFrontierContinuation,
    original: Range<usize>,
    scope_complete: bool,
    shaping_seams: [Option<crate::markdown_line_composer::ResolvedBidiShapingSeam>; 2],
}

/// 只含弱身份与标量，不保活Source/Font/原子任务。票不能Clone或交给另一个owner。
pub(crate) struct MarkdownShapingFrontierContinuation {
    owner: Weak<()>,
    serial: u64,
    next: usize,
    next_byte: usize,
    child: Weak<()>,
    font_splice: bool,
}

/// 只有原Unicode游标遇到完整阻塞GCB时签发；不授予任意短Range或新的SCX边界。
pub(crate) struct MarkdownShapingTerminalGrapheme {
    owner: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    utf16: Range<usize>,
    bytes: Range<usize>,
    item_end: usize,
    cancellation: MarkdownShapingCancellation,
}

impl MarkdownShapingTerminalGrapheme {
    pub(crate) fn source_range(&self) -> Range<usize> {
        self.utf16.clone()
    }

    pub(crate) fn owner_scope(&self) -> Weak<()> {
        self.owner.clone()
    }

    pub(crate) fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }

    pub(crate) fn original_font(&self) -> &MarkdownLayoutFontAuthority {
        &self.font
    }

    pub(crate) fn byte_range(&self) -> Range<usize> {
        self.bytes.clone()
    }

    pub(crate) fn item_end(&self) -> usize {
        self.item_end
    }

    pub(crate) fn cancellation(&self) -> MarkdownShapingCancellation {
        self.cancellation.clone()
    }
}

impl MarkdownShapingFrontierPart {
    pub(crate) fn shaping_seams(
        &self,
    ) -> &[Option<crate::markdown_line_composer::ResolvedBidiShapingSeam>; 2] {
        &self.shaping_seams
    }
    /// 只借此真实part的原Source身份；不能由其公开范围重新签发另一个Source的审核。
    pub(crate) fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.job.source_authority
    }
    /// 审核与重形保原同次取消位，不从调用方另建一个可脱离原任务的生命周期。
    pub(crate) fn cancellation(&self) -> MarkdownShapingCancellation {
        self.job.cancellation.clone()
    }
    /// 原Job实际参与整形的有限支持区；含中心外真实上下文，不从段落或预算猜边界。
    pub(crate) fn shaping_range(&self) -> Range<usize> {
        self.job.source_utf16.clone()
    }
    pub(crate) fn certified_range(&self) -> Range<usize> {
        self.job.source_range()
    }
    pub(crate) fn original_range(&self) -> Range<usize> {
        self.original.clone()
    }
    pub(crate) fn is_item_complete(&self) -> bool {
        self.job.source_range().end == self.original.end
    }
    pub(crate) fn is_scope_complete(&self) -> bool {
        self.scope_complete
    }
    pub(crate) fn into_parts(self) -> (MarkdownShapingJob, MarkdownShapingFrontierContinuation) {
        (self.job, self.continuation) // 真child必须交给原消费者，不能只交元数据后伪称已消费。
    }
}

/// 开放分支必须携原SCX借用和两张原GCB端点票；坐标副本本身不能构造这些票。
pub(crate) enum MarkdownShapingFrontierScope<'a, 'job> {
    Closed(Range<usize>),
    /// 只认证原样式范围的坐标；不授予开放Script、独立字形或真实段末权限。
    CertifiedCoordinates {
        original: Range<usize>,
        endpoints: [&'a crate::markdown_line_composer::CertifiedShapingEndpoint; 2],
    },
    Open {
        original: Range<usize>,
        endpoints: [&'a crate::markdown_line_composer::CertifiedShapingEndpoint; 2],
        script: &'a crate::markdown_scripts::MarkdownStableScriptPrefix<'job>,
    },
}

impl MarkdownShapingFrontierJob {
    /// 计划/标量拒绝并不等于缺字；这里只交付已经读到的完整GCB，供原字体试次确认。
    pub(crate) fn terminal_grapheme(
        &self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> FrontierResult<Option<MarkdownShapingTerminalGrapheme>> {
        self.guard(source, font)?;
        if self.open_scope
            || self.answer
                != MarkdownShapingFrontierAnswer::NeedsFrontier(
                    MarkdownShapingFrontierReason::Plan(
                        harfrust::AsciiFrontierRejection::LookupBehavior,
                    ),
                )
        {
            return Ok(None); // 构造/查表拒绝和开放SCX不取得这个自然尾部的特殊交接资格。
        }
        Ok(self.unicode.as_ref().and_then(|state| {
            state
                .terminal_grapheme(self.original.end)
                .filter(|(utf16, _)| {
                    !(utf16.start == self.original.start && self.partial_grapheme[0])
                        && !(utf16.end == self.original.end && self.partial_grapheme[1])
                }) // 局部 shaping GB1/GB2 不是完整外 GCB，不能发给原 terminal GCB 交接。
                .map(|(utf16, bytes)| MarkdownShapingTerminalGrapheme {
                    owner: Arc::downgrade(&self.identity),
                    source: self.source.clone(),
                    font: self.font.clone(),
                    utf16,
                    bytes,
                    item_end: self.original.end,
                    cancellation: self.cancellation.clone(),
                })
        }))
    }

    /// 同Font只允许自然尾片继续真实shape；不同Font只封口此前已认证的原Font残余。
    pub(crate) fn resume_terminal_grapheme(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        terminal: &MarkdownShapingTerminalGrapheme,
        same_font: bool,
    ) -> FrontierResult<()> {
        self.guard(source, font)?;
        if (same_font && terminal.utf16.end != self.original.end)
            || !Weak::ptr_eq(&terminal.owner, &Arc::downgrade(&self.identity))
            || terminal.source != self.source
            || !terminal.font.same_payload(&self.font)
            || self.terminal_grapheme(source, font)?.is_none_or(|actual| {
                actual.utf16 != terminal.utf16 || actual.bytes != terminal.bytes
            })
        {
            return Err(MarkdownShapingFrontierError::ContinuationMismatch);
        } // 原owner和实际阻塞位置必须仍相同；相同范围的另一个请求不能恢复本游标。
        self.candidate = Some(
            self.unicode
                .as_mut()
                .ok_or(MarkdownShapingError::Pending)?
                .resume_terminal_grapheme(same_font),
        );
        self.answer = MarkdownShapingFrontierAnswer::Pending;
        Ok(())
    }

    /// 前一真实Font段已经归还后，移交已由原GCB查询确认的下一个字体选择。
    /// 只继承同owner、同SCX和同取消位；调用者持有真实相邻GCB票及已完成字体试次。
    pub(crate) fn following_font_grapheme(
        &self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        previous: &MarkdownShapingTerminalGrapheme,
        endpoints: [&crate::markdown_layout_breaks::MarkdownCertifiedGraphemeEndpoint; 2],
    ) -> FrontierResult<MarkdownShapingTerminalGrapheme> {
        self.guard(source, font)?;
        let start = endpoints[0].boundary(source)?;
        let end = endpoints[1].boundary(source)?;
        if !Weak::ptr_eq(&previous.owner, &Arc::downgrade(&self.identity))
            || previous.source != self.source
            || !previous.font.same_payload(&self.font)
            || self.outstanding.is_some()
            || self.next != start.utf16
            || self.next_byte != start.byte
            || start.utf16 < previous.utf16.end
            || end.utf16 <= start.utf16
            || end.utf16 > self.original.end
            || !endpoints[0].same_scope(endpoints[1])
        {
            return Err(MarkdownShapingFrontierError::ContinuationMismatch);
        } // 不接受跳过尚未交付的原Font正文，也不把另一个索引的端点借给本续体。
        Ok(MarkdownShapingTerminalGrapheme {
            owner: previous.owner.clone(),
            source: self.source.clone(),
            font: self.font.clone(),
            utf16: start.utf16..end.utf16,
            bytes: start.byte..end.byte,
            item_end: self.original.end,
            cancellation: self.cancellation.clone(),
        })
    }

    /// 完整回退Font段进入原Store前，登记其实际child及唯一归还票。
    /// 此处不shape、不重建Source；只有真实Ready的原job能推进原Frontier游标。
    pub(crate) fn lend_font_run(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        proof: &MarkdownShapingTerminalGrapheme,
        job: &mut MarkdownShapingJob,
    ) -> FrontierResult<MarkdownShapingFrontierContinuation> {
        self.guard(source, font)?;
        let selected_font = job.font_authority.clone();
        job.guard(source, &selected_font)?;
        if !Weak::ptr_eq(&proof.owner, &Arc::downgrade(&self.identity))
            || proof.source != self.source
            || !proof.font.same_payload(&self.font)
            || self.outstanding.is_some()
            || self.next != proof.utf16.start
            || job.source_utf16.start != self.next
            || job.source_utf16.end < proof.utf16.end
            || job.source_utf16.end > self.original.end
            || job.font_authority.same_payload(&self.font)
            || job.status() != MarkdownShapingStatus::Ready
        {
            return Err(MarkdownShapingFrontierError::ContinuationMismatch);
        } // 新Font的真实输出才有交接资格；同Font不能绕过原plan的能力拒绝。
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or(MarkdownShapingError::CoordinateOverflow)?;
        let child = job.index_identity();
        self.outstanding = Some(child.clone());
        Ok(MarkdownShapingFrontierContinuation {
            owner: Arc::downgrade(&self.identity),
            serial: self.serial,
            next: job.source_utf16.end,
            next_byte: job.source_bytes.end,
            child,
            font_splice: true,
        })
    }

    /// 仅原协调器在验证完整item和样式响应后调用；不对外暴露短Range的升权接口。
    pub(crate) fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        original: Range<usize>,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> FrontierResult<Self> {
        Self::initialize(
            source,
            authority,
            font,
            font_authority,
            MarkdownShapingFrontierScope::Closed(original),
            options,
            cancellation,
        )
    }

    /// 有端点票的scope只取原GCB坐标；未提供原索引的Closed仍明确走同步定位合同。
    pub(crate) fn new_with_scope(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        scope: MarkdownShapingFrontierScope<'_, '_>,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> FrontierResult<Self> {
        match scope {
            MarkdownShapingFrontierScope::Closed(original) => Self::new(
                source,
                authority,
                font,
                font_authority,
                original,
                options,
                cancellation,
            ),
            certified => Self::initialize(
                source,
                authority,
                font,
                font_authority,
                certified,
                options,
                cancellation,
            ),
        } // 原无索引入口的定位工作保持同步；有票分支不为复用实现而重新读取正文。
    }

    /// 两条构造路径共享原身份、选项和计划装配；只有坐标的实际来源不同。
    fn initialize(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        scope: MarkdownShapingFrontierScope<'_, '_>,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> FrontierResult<Self> {
        source.health().map_err(MarkdownShapingError::from)?; // 原errno先于取消及权限不符。
        if !source.permits(authority) {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        if !font.permits(font_authority) {
            return Err(MarkdownShapingError::FontMismatch.into());
        }
        let original = match &scope {
            MarkdownShapingFrontierScope::Closed(original)
            | MarkdownShapingFrontierScope::CertifiedCoordinates { original, .. }
            | MarkdownShapingFrontierScope::Open { original, .. } => original.clone(),
        };
        if original.start > original.end || original.end > source.utf16_len() {
            return Err(MarkdownShapingError::InvalidRange.into());
        }
        u32::try_from(original.end).map_err(|_| MarkdownShapingError::CoordinateOverflow)?;
        let original_bytes = match &scope {
            MarkdownShapingFrontierScope::Closed(_) => {
                source
                    .byte_offset_for_utf16(original.start)
                    .map_err(MarkdownShapingError::from)?
                    ..source
                        .byte_offset_for_utf16(original.end)
                        .map_err(MarkdownShapingError::from)?
            }
            MarkdownShapingFrontierScope::Open { endpoints, .. }
            | MarkdownShapingFrontierScope::CertifiedCoordinates { endpoints, .. } => {
                let start = endpoints[0].boundary(authority)?;
                let end = endpoints[1].boundary(authority)?;
                endpoints[0].require_pair(endpoints[1], authority)?;
                if start.utf16 != original.start
                    || end.utf16 != original.end
                    || start.byte >= end.byte
                    || end.byte > source.byte_len()
                {
                    return Err(MarkdownShapingError::InvalidRange.into());
                }
                start.byte..end.byte
            }
        };
        let (shaping_seams, partial_grapheme) = match &scope {
            MarkdownShapingFrontierScope::Closed(_) => ([None, None], [false; 2]),
            MarkdownShapingFrontierScope::Open { endpoints, .. }
            | MarkdownShapingFrontierScope::CertifiedCoordinates { endpoints, .. } => (
                std::array::from_fn(|index| endpoints[index].seam().cloned()),
                std::array::from_fn(|index| !endpoints[index].is_grapheme()),
            ),
        };
        let context_bounds = shaping_seams.iter().any(Option::is_some).then(|| {
            (if shaping_seams[0].is_some() {
                original_bytes.start
            } else {
                0
            })..(if shaping_seams[1].is_some() {
                original_bytes.end
            } else {
                source.byte_len()
            })
        }); // 原 Style 请求消费后，唯一前沿继续持真实 seam，不从有限字节窗猜段落边界。
        let options = ValidatedOptions::new(options, &font)?;
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        let (plan, answer) = match FontFrontierPlan::new(
            &font,
            font_authority,
            options.direction,
            options.script,
            options.language.as_ref(),
            &options.features,
        ) {
            Ok(plan) => (Some(plan), MarkdownShapingFrontierAnswer::Pending),
            Err(reason) => (
                None,
                MarkdownShapingFrontierAnswer::NeedsFrontier(MarkdownShapingFrontierReason::Plan(
                    reason,
                )),
            ),
        }; // 未认证计划仍有真实typed结果；不改feature或回退整item后冒称lazy。
        source.health().map_err(MarkdownShapingError::from)?;
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        let unicode = (original_bytes.len() != original.len()).then(|| {
            Box::new(unicode::UnicodeState::new(
                original_bytes.start,
                original.start,
            ))
        });
        let mut result = Self {
            identity: Arc::new(()),
            payload: Some(FrontierPayload {
                source,
                font,
                options,
                plan,
            }),
            source: authority.clone(),
            font: font_authority.clone(),
            cancellation,
            next: original.start,
            next_byte: original_bytes.start,
            original,
            original_bytes,
            serial: 0,
            scan: [0; MARKDOWN_SHAPING_FRONTIER_UNITS + 1],
            scanned: 0,
            candidate: None,
            child: None,
            outstanding: None,
            answer,
            word_candidate: false,
            word_source_checked: false,
            word_closure_checked: false,
            word_closure: None,
            advance_scan: None,
            nonnegative: None,
            word_end_cursor: None,
            local_source_checked: false,
            local_source_cursor: None,
            local_source_bounds: None,
            local_scan: None,
            local_capability: None,
            local_certificate: None,
            positioned_scan: None,
            positioned_proof: None,
            local_verified: 0,
            resources: None,
            unicode,
            open_scope: false,
            shaping_seams,
            partial_grapheme,
            context_bounds,
        };
        if let MarkdownShapingFrontierScope::Open { script, .. } = scope {
            result.bind_open_script(script)?;
        }
        Ok(result)
    }

    /// 只有原Script借用证明能将本owner约束为开放支持域；公开短Range不能调用此升权。
    /// 字体仍需逐记录通过SimpleGcb计划，ASCII local/kern路径不获得此资格。
    fn bind_open_script(
        &mut self,
        prefix: &crate::markdown_scripts::MarkdownStableScriptPrefix<'_>,
    ) -> FrontierResult<()> {
        self.guard(&self.source, &self.font)?;
        let script_error = |error| match error {
            crate::MarkdownScriptError::Source(error) => MarkdownShapingError::Source(error),
            crate::MarkdownScriptError::Cancelled => MarkdownShapingError::Cancelled,
            _ => MarkdownShapingError::SourceMismatch,
        };
        let facts = prefix.facts().map_err(script_error)?;
        if prefix.source_authority().map_err(script_error)? != &self.source
            || prefix.kind().map_err(script_error)?
                != crate::markdown_scripts::MarkdownScriptPrefixKind::StableOpenPrefix
            || self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingError::Poisoned)?
                .options
                .script
                .tag()
                .to_be_bytes()
                != facts.script.iso15924_tag()
            || self.original.start < facts.start_utf16
            || self.original.end > facts.end_utf16
            || self.serial != 0
            || self.child.is_some()
            || self.open_scope
        {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        let mut unicode =
            unicode::UnicodeState::new(self.original_bytes.start, self.original.start);
        unicode.require_independent_route();
        self.unicode = Some(Box::new(unicode));
        self.open_scope = true;
        self.guard(&self.source, &self.font)
    }

    pub(crate) fn attach_composer_resources(
        &mut self,
        registration: crate::markdown_line_composer::worker_resources::FrontierResourceRegistration,
    ) -> FrontierResult<()> {
        self.guard(&self.source, &self.font)?;
        if self.resources.is_some()
            || self.next != self.original.start
            || self.serial != 0
            || self.child.is_some()
            || self.outstanding.is_some()
        {
            return Err(MarkdownShapingError::InvalidOptions.into());
        } // 先挂原ParentScope，再允许任意child创建；续片不接受新scope补签旧过程。
        self.resources = Some(
            registration
                .install()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?,
        );
        Ok(())
    }

    fn guard(
        &self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> FrontierResult<()> {
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        payload
            .source
            .health()
            .map_err(MarkdownShapingError::from)?;
        if source != &self.source || !payload.source.permits(source) {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        if !payload.font.permits(font) || !payload.font.permits(&self.font) {
            return Err(MarkdownShapingError::FontMismatch.into());
        }
        if payload
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.permits_font(&payload.font))
        {
            return Err(MarkdownShapingError::FontMismatch.into());
        } // 原Font计划弱身份每次消费仍复核，未来接入不能仅用同样的选项key代替owner。
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        Ok(())
    }

    fn fail<T>(&mut self, error: MarkdownShapingFrontierError) -> FrontierResult<T> {
        let backing = self
            .payload
            .as_ref()
            .and_then(|payload| payload.source.original_backing_observer());
        drop(self.child.take());
        drop(self.advance_scan.take());
        self.word_closure = None; // 只清固定bitset游标；它没有文件、映射或Source强claim。
        self.local_scan = None; // 同样仅归还固定字体编号集合，不改实际glyph/Source资源收据。
        self.local_source_cursor = None; // 取消/Source失败也实际归还尚在途的原树路径，不能靠owner壳留住元数据。
        self.local_source_bounds = None;
        self.positioned_scan = None;
        self.positioned_proof = None;
        self.local_certificate = None;
        drop(self.nonnegative.take());
        drop(self.payload.take()); // 先归还实际资源，旧cursor/票永久失效，不重采Source或样式。
        if let Some(errno) = backing.and_then(|observer| observer.first_errno()) {
            return Err(MarkdownShapingError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )
            .into());
        } // 最后Source归还时的实际错误也不能被取消、普通不适用或旧metadata失败覆盖。
        Err(error)
    }

    pub(crate) fn step(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownShapingBudget,
        metadata_budget: usize,
    ) -> FrontierResult<MarkdownShapingFrontierStep> {
        let result = self.advance(source, font, breaks, budget, metadata_budget);
        let result = match self.guard(source, font) {
            Err(MarkdownShapingFrontierError::Shaping(MarkdownShapingError::Source(error))) => {
                Err(MarkdownShapingError::Source(error).into())
            }
            guarded => result.and_then(|step| {
                guarded?;
                Ok(step)
            }),
        }; // 包括NeedsFrontier/Pending零工作出口，真实Source首错都不能被成功占位状态吞掉。
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail(error),
        }
    }

    fn advance(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownShapingBudget,
        metadata_budget: usize,
    ) -> FrontierResult<MarkdownShapingFrontierStep> {
        self.guard(source, font)?;
        if budget.bytes == 0 || budget.scalars == 0 || metadata_budget == 0 {
            return Err(MarkdownShapingError::InvalidOptions.into());
        }
        let mut step = MarkdownShapingFrontierStep {
            answer: self.answer,
            source_bytes: 0,
            input_bytes: 0,
            scalars: 0,
            context_bytes: 0,
            shapes: 0,
            metadata_glyphs: 0,
            plan_records: 0,
            boundary_records: 0,
        };
        if self.outstanding.is_some() {
            step.answer = MarkdownShapingFrontierAnswer::PendingConsumer;
            return Ok(step); // 即使child已死，也必须归还原ticket，不能从外部Range跳过FIFO。
        }
        if matches!(
            self.answer,
            MarkdownShapingFrontierAnswer::NeedsFrontier(_)
                | MarkdownShapingFrontierAnswer::Complete
                | MarkdownShapingFrontierAnswer::PartReady
        ) {
            return Ok(step);
        }
        if self.next == self.original.end {
            self.answer = MarkdownShapingFrontierAnswer::Complete;
            step.answer = self.answer;
            return Ok(step); // 真item端点才Complete；有限缓冲区填满不是EOF。
        }
        if self.unicode.is_some() {
            return self.advance_unicode(source, font, breaks, budget, metadata_budget, step);
        }
        let defer_local = self
            .payload
            .as_ref()
            .and_then(|payload| payload.plan.as_ref().map(|plan| (plan, &payload.font)))
            .is_some_and(|(plan, font)| plan.simple_gcb_job(font).is_ok());
        if !defer_local
            && self.local_plan()
            && !self.prepare_local(source, font, budget, metadata_budget, &mut step)?
        {
            step.answer = self.answer;
            return Ok(step);
        } // 连独立GCB资格也没有的计划保持原先验拒绝，不能为它扩展257byte扫描权限。
        if self.candidate.is_none() {
            let needed = (self.original_bytes.end - self.next_byte).min(self.scan.len());
            let count = (needed - self.scanned)
                .min(budget.bytes)
                .min(budget.scalars);
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let begin = self.next_byte + self.scanned;
            let mut copied = 0;
            payload
                .source
                .visit_byte_range(begin..begin + count, |bytes| {
                    self.scan[self.scanned + copied..self.scanned + copied + bytes.len()]
                        .copy_from_slice(bytes);
                    copied += bytes.len();
                    Ok(())
                })
                .map_err(MarkdownShapingError::from)?;
            self.guard(source, font)?;
            self.scanned += copied; // 真实lease归还成功后才发布已读事实；错误不改成早停成功。
            step.source_bytes = copied;
            if self.scanned != needed {
                return Ok(step);
            }
            let ascii = self.scan[..needed]
                .iter()
                .position(|byte| !(0x20..=0x7e).contains(byte))
                .unwrap_or(needed);
            let eof = self.next_byte + needed == self.original_bytes.end;
            let length = if eof && ascii == needed && needed <= MARKDOWN_SHAPING_FRONTIER_UNITS {
                needed // 最后短片沿真实item端点；无空间候选不冒充有自然软行机会。
            } else {
                // 在真实非数字space seam结束，另有已读ASCII首未消费标量。未来fraction
                // slash只能回写连续Decimal串，不能越过此space影响已经签发的前缀。
                (1..ascii.min(needed))
                    .rev()
                    .find(|&end| self.scan[end - 1] == b' ')
                    .unwrap_or(0)
            };
            if length == 0 {
                if ascii < needed
                    && let Some(job) = payload
                        .plan
                        .as_ref()
                        .and_then(|plan| plan.simple_gcb_job(&payload.font).ok())
                {
                    // 仅原≤257byte已收费扫描实际遇到非ASCII、且没有安全space seam时
                    // 才转到同Font/Options的独立字素认证。不是按字母或字体名猜Unicode可切。
                    // 原scratch不能充当GCB或编码证明；下一量子逐真实边界重读，读量照实收费。
                    self.unicode = Some(Box::new(unicode::UnicodeState::from_ascii_scan(
                        self.next_byte,
                        self.next,
                        job,
                    )));
                    self.scanned = 0;
                    return Ok(step);
                }
                if ascii == needed && needed > MARKDOWN_SHAPING_FRONTIER_UNITS {
                    self.word_candidate = true;
                    self.candidate = Some(self.next + MARKDOWN_SHAPING_FRONTIER_UNITS);
                } else {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(if ascii < needed {
                        MarkdownShapingFrontierReason::NonAscii
                    } else {
                        MarkdownShapingFrontierReason::WordBeyondBudget
                    });
                    step.answer = self.answer;
                    return Ok(step); // 不替长词强造新BOT/EOT或默认plain功能。
                }
            } else {
                self.candidate = Some(self.next + length);
                // 同一个已认证词项的最后短片仍沿原证明延续，不能因本片抵达真实EOF
                // 就丢掉跨前缀整行的安全GCB候选；这不改变本child的实际Ready范围。
                self.word_candidate = self.word_source_checked;
            }
        }
        if self.local_plan()
            && !self.prepare_local(source, font, budget, metadata_budget, &mut step)?
        {
            step.answer = self.answer;
            return Ok(step);
        } // 已选择的ASCII候选仍须完整source域与原local计划证书，之后才能签词许可或创建child。
        let end = self
            .candidate
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        for at in [self.next, end] {
            match breaks.grapheme_query(source, at, MarkdownGraphemeQuery::IsBoundary)? {
                MarkdownGraphemeAnswer::Pending => return Ok(step),
                MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == at => {}
                MarkdownGraphemeAnswer::Known(None)
                    if (at == self.original.start && self.partial_grapheme[0])
                        || (at == self.original.end && self.partial_grapheme[1]) => {}
                MarkdownGraphemeAnswer::Known(_) => {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::NotGraphemeBoundary,
                    );
                    step.answer = self.answer;
                    return Ok(step);
                }
            } // 字节均为ASCII仍不能假定原item首端未切开一个外部GCB。
        }
        if self.word_candidate
            && !self.prepare_word(source, font, breaks, metadata_budget, &mut step)?
        {
            step.answer = self.answer;
            return Ok(step);
        }
        if self.child.is_none() {
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let plan = payload
                .plan
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let capability = plan.capability();
            if capability.is_some_and(|capability| capability.requires_closed_decimal_seam)
                && !self.word_candidate
                && end < self.original.end
                && self.scan[end - self.next - 1].is_ascii_digit()
            {
                return Err(MarkdownShapingError::InvalidRange.into()); // 真whole-item ASCII另证明无U+2044。
            }
            let support = self.support_range(end)?;
            let support_bytes = (self.original_bytes.start + support.start - self.original.start)
                ..(self.original_bytes.start + support.end - self.original.start);
            let mut options = payload.options.clone();
            options.beginning_of_text &= support.start == self.original.start;
            options.end_of_text &= support.end == self.original.end;
            let prepared = self
                .context_bounds
                .clone()
                .map(|bounds| {
                    input::prepare_context(&payload.source, support_bytes.clone(), Some(bounds))
                })
                .transpose()?;
            let mut child = MarkdownShapingJob::assemble_with_context(
                payload.source.clone(),
                source,
                payload.font.clone(),
                font,
                (support, support_bytes, prepared),
                options,
                self.cancellation.clone(),
            )?;
            if let Some(resources) = &self.resources {
                child.attach_composer_resources(resources)?;
            } // Buffer刚装配还没有glyph FD/map；计划/词证书、输入和shape的任何失败都带原claim。
            // 原构造器的固定5标量邻接上下文与实际support ingest分开报告。它不是
            // 逐步输入预算内的免费读取，也不是将中心256字节伪报成所有Source访问。
            step.context_bytes = child.profile()?.source_bytes_read;
            child
                .payload
                .as_mut()
                .ok_or(MarkdownShapingError::Poisoned)?
                .frontier_plan = Some(plan.plan());
            self.child = Some(child); // 只有源/GCB/原计划都成立才申请实际glyph分页资源。
        }
        let child = self
            .child
            .as_mut()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        if child.status() == MarkdownShapingStatus::Ingesting {
            let bytes = budget.bytes - step.source_bytes;
            let scalars = budget.scalars - step.source_bytes;
            if bytes == 0 || scalars == 0 {
                return Ok(step);
            }
            let input = child.step(source, font, MarkdownShapingBudget { bytes, scalars })?;
            step.input_bytes = input.bytes;
            step.scalars = input.scalars;
        }
        if child.status() == MarkdownShapingStatus::AwaitingShape {
            child.shape(source, font)?;
            step.shapes = 1;
        }
        self.guard(source, font)?;
        if self
            .child
            .as_ref()
            .is_some_and(|child| child.status() == MarkdownShapingStatus::Ready)
            && self.seal_center(source, font, end, metadata_budget, &mut step)?
        {
            if self.word_candidate {
                let child = self
                    .child
                    .as_mut()
                    .ok_or(MarkdownShapingFrontierError::Poisoned)?;
                let proof = word::WordPrefixProof::new(
                    &self.identity,
                    child,
                    self.original.clone(),
                    &self.cancellation,
                )?;
                child
                    .payload
                    .as_mut()
                    .ok_or(MarkdownShapingError::Poisoned)?
                    .frontier_word = Some(proof);
            } // 局部计划先完成真实中心view；不能把支持区预签成词前缀的公开范围。
            self.answer = MarkdownShapingFrontierAnswer::PartReady;
        }
        step.answer = self.answer;
        Ok(step)
    }

    pub(crate) fn take_part(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> FrontierResult<MarkdownShapingFrontierPart> {
        let result = (|| {
            self.guard(source, font)?;
            if self.answer != MarkdownShapingFrontierAnswer::PartReady || self.outstanding.is_some()
            {
                return Err(MarkdownShapingError::Pending.into());
            }
            let job = self
                .child
                .take()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let range = job.source_range();
            let next_byte = self
                .unicode
                .as_ref()
                .map_or_else(|| self.next_byte + range.len(), |state| state.center_byte());
            self.serial = self
                .serial
                .checked_add(1)
                .ok_or(MarkdownShapingError::CoordinateOverflow)?;
            let child = Arc::downgrade(&job.identity);
            self.outstanding = Some(child.clone());
            Ok(MarkdownShapingFrontierPart {
                shaping_seams: self.shaping_seams.clone(), // 原上下文支持域也可能到达真实 seam，审核仍需保有它。
                scope_complete: range.end == self.original.end
                    || (self.open_scope
                        && self
                            .unicode
                            .as_ref()
                            .is_some_and(|state| state.support_end() == self.original.end)),
                job,
                original: self.original.clone(),
                continuation: MarkdownShapingFrontierContinuation {
                    owner: Arc::downgrade(&self.identity),
                    serial: self.serial,
                    next: range.end,
                    next_byte,
                    child,
                    font_splice: false,
                },
            })
        })();
        match result {
            Ok(part) => Ok(part),
            Err(error) => self.fail(error),
        }
    }

    pub(crate) fn resume(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        continuation: MarkdownShapingFrontierContinuation,
    ) -> FrontierResult<()> {
        let result = (|| {
            self.guard(source, font)?;
            if !Weak::ptr_eq(&continuation.owner, &Arc::downgrade(&self.identity))
                || continuation.serial != self.serial
                || self
                    .outstanding
                    .as_ref()
                    .is_none_or(|child| !Weak::ptr_eq(child, &continuation.child))
            {
                return Err(MarkdownShapingFrontierError::ContinuationMismatch);
            }
            if continuation.child.strong_count() != 0 {
                return Err(MarkdownShapingError::Pending.into());
            }
            self.next = continuation.next;
            self.next_byte = continuation.next_byte;
            self.scanned = 0;
            self.candidate = None;
            self.word_candidate = false;
            self.local_verified = 0;
            if let Some(unicode) = &mut self.unicode {
                if continuation.font_splice {
                    unicode.reset_after_font_run(self.next_byte, self.next);
                } else {
                    unicode.reset(self.next_byte, self.next);
                } // 外来Font段的GCB数不能由旧中心长度猜测，原Font子片仍走已知ordinal。
            }
            self.outstanding = None;
            self.answer = MarkdownShapingFrontierAnswer::Pending;
            Ok(()) // 真实child已归还才清有界scratch；未读tail仍只有原owner的私有cursor。
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.fail(error),
        }
    }
}

#[cfg(test)]
#[path = "frontier/tests.rs"]
mod tests;
