//! 原协调器签发单个真实字素的字体试次权限；不建立第二份GCB、Script或UBA索引。

use std::sync::Weak;

use super::*;

/// 同一个完整编辑字素中的真实整形段准备结果；Ready 不表示整个字素已经选定字体。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineFontProbeSegmentAnswer {
    Pending,
    Ready,
    Complete,
}

/// 只报告本次实际读取的原目录工作；字体 worker 的正文、glyph 费用仍由原试次归属。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLineFontProbeSegmentStep {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
    pub answer: MarkdownLineFontProbeSegmentAnswer,
}

/// 任意时刻只有一个真实 level 段；不按完整 GCB 长度保存段表或复制正文。
#[derive(Clone)]
struct ProbeSegment {
    utf16: Range<usize>,
    bytes: Range<usize>,
    level: u8,
    // 只有混合 level 字素需要额外裁定邻接上下文，普通单段保持原热路径。
    context: Option<Range<usize>>,
}

/// 流式扫描只借原 Bidi 的逐 UTF16 记录，下一段首记录留在这一固定槽，避免重复 I/O。
#[derive(Clone)]
struct SegmentScan {
    start: usize,
    byte: usize,
    next: usize,
    level: Option<u8>,
    held: Option<crate::markdown_bidi::MarkdownBidiShapingUnit>,
    context_phase: usize,
    context_cursor: usize,
    context: Range<usize>,
}

impl SegmentScan {
    fn new(
        start: usize,
        byte: usize,
        held: Option<crate::markdown_bidi::MarkdownBidiShapingUnit>,
    ) -> Self {
        Self {
            start,
            byte,
            next: start,
            level: None,
            held,
            context_phase: 0,
            context_cursor: start,
            context: byte..byte,
        }
    }
}

/// 只共享原冻结Source，不复制文字；同一请求被响应、取消或归还后，弱scope立即失效。
/// 一个完整GCB可以包含很多标量，凭此票不能宣称底层整形已具有固定时间上界。
#[derive(Clone)]
pub struct MarkdownLineFontProbe {
    source: MarkdownLayoutTextSource,
    request: MarkdownLineStyleRequest,
    range: Range<usize>,
    // 两端来自同一原 GCB 目录或其已经持有的认证端点，不在 Native 入口重查 UTF16。
    bytes: Range<usize>,
    scope: Weak<()>,
    cancellation: MarkdownLineComposeCancellation,
    segment: Option<ProbeSegment>,
    scan: Option<SegmentScan>,
    // 普通单段从签发时就有原 item 的完整 level 证明，绝不由“首段成功”推导此事实。
    single_segment: bool,
    complete: bool,
}

impl MarkdownLineFontProbe {
    /// 优先核原Source首错，再核原请求和取消位；不查活文档或申请任何文本窗口。
    ///
    /// # Errors
    /// 原Source失败、取消或原样式阶段已结束时拒绝，旧探测不能由后继请求重新启用。
    pub fn validate(&self) -> Result<()> {
        self.source.health()?;
        if self.cancellation.is_cancelled() {
            return Err(MarkdownLineComposeError::Cancelled);
        }
        if self.scope.strong_count() == 0 {
            return Err(MarkdownLineComposeError::Poisoned);
        }
        Ok(())
    }

    /// 只借同一个不可变来源供纯Core worker共享；不会再捕获Doc或额外构造索引。
    pub fn source(&self) -> Result<&MarkdownLayoutTextSource> {
        self.validate()?;
        Ok(&self.source)
    }

    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// 借原两张GCB端点已经认证的UTF8坐标；不再从远端UTF16位置重扫冻结源。
    ///
    /// # Errors
    /// 原Source首错、取消或请求已消费时拒绝，数值端点自身不能延长试次权限。
    pub fn byte_range(&self) -> Result<Range<usize>> {
        self.validate()?;
        Ok(self.bytes.clone())
    }

    /// 只有已取到真实两端和上下文范围的当前段才能交 worker，Pending/Complete 不重放旧段。
    pub fn shaping_segment_ready(&self) -> Result<bool> {
        self.validate()?;
        Ok(!self.complete
            && self.segment.is_some()
            && self
                .scan
                .as_ref()
                .is_none_or(|scan| scan.context_phase == 3))
    }

    /// 字体复用只能用于原分析已经证明的整个单段 GCB；混合字素永远不能缓存首段结论。
    pub fn complete_single_segment(&self) -> Result<bool> {
        self.validate()?;
        Ok(self.single_segment)
    }

    fn ready_segment(&self) -> Result<&ProbeSegment> {
        self.validate()?;
        if !self.shaping_segment_ready()? {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        self.segment
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)
    }

    /// 为已完成选择的有界复用读取精确输入；只借本GCB和原shaper两侧20B上下文包络。
    /// 每次最多72B（32B小GCB加40B上下文），不扩大Source公开读取权限，也不解析第二套上下文。
    /// 已交付piece之后失败仍返回真实read，调用方必须收费，不能把失败或命中当零工作。
    pub fn copy_choice_bytes(&self, range: Range<usize>, target: &mut [u8]) -> (usize, Result<()>) {
        let mut read = 0;
        let result = (|| {
            self.validate()?;
            let end = self
                .bytes
                .end
                .saturating_add(20)
                .min(self.source.byte_len());
            if range.start > range.end
                || range.start < self.bytes.start.saturating_sub(20)
                || range.end > end
                || range.len() != target.len()
                || range.len() > 72
            {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            self.source.visit_byte_range(range, |piece| {
                target[read..read + piece.len()].copy_from_slice(piece);
                read += piece.len();
                Ok(())
            })?;
            self.validate()
        })();
        // 原健康优先于操作中的较早错误；已读取的字节保留，不据成功与否推导资源事实。
        (read, self.validate().and(result))
    }

    /// 原试次唯一整形入口直接转移已认证的两套坐标，不重定位远端UTF16端点。
    /// 正文仍由原Job按调用方预算流入；上下文、flags、资源与取消继续使用唯一assemble路径。
    ///
    /// # Errors
    /// 同源试次、原Font、真实选项或取消失效时拒绝；构造后再次核原probe，失败真实丢弃候选Job。
    pub fn shaping_job(
        &self,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<MarkdownShapingJob> {
        self.validate()?;
        let segment = self.ready_segment()?;
        let result = MarkdownShapingJob::from_certified_item_input(
            self.source.clone(),
            &self.request.source,
            font,
            font_authority,
            (
                segment.utf16.clone(),
                segment.bytes.clone(),
                segment.context.clone(),
            ),
            options,
            cancellation,
        )
        .map_err(MarkdownLineComposeError::from);
        self.validate()?; // 即使候选创建失败，原Source首因也不能被Font/选项或取消错误遮住。
        result
    }

    /// 只读取原 GCB 最前面的至多四个 UTF8 字节，首 scalar 仅用于系统候选选择，不证明完整 GCB 覆盖。
    /// 返回实际已借字节数及原 typed 结果：读取后失权、解码或来源失败也必须由调用方收费。
    /// 范围和原票在读取前后核验；部分 piece 已交付后发生 I/O 失败时，错误仍保留原 Source 首因。
    pub fn first_scalar(&self) -> (usize, Result<char>) {
        let mut read = 0;
        let result = (|| {
            self.validate()?;
            if self.bytes.is_empty() {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            let mut bytes = [0_u8; 4];
            let count = self.bytes.len().min(bytes.len());
            self.source
                .visit_byte_range(self.bytes.start..self.bytes.start + count, |piece| {
                    bytes[read..read + piece.len()].copy_from_slice(piece);
                    read += piece.len();
                    Ok(())
                })?;
            self.validate()?; // 即使这里失权，已读取的原字节数仍通过外层 tuple 交还。
            let width = match bytes[0] {
                0x00..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            if width > read {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            // 有限窗口可以止于下一个 scalar 内部；只解码首个完整 scalar，不把其后不完整字节当正文错误。
            let value = std::str::from_utf8(&bytes[..width])
                .ok()
                .and_then(|value| value.chars().next())
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            self.validate()?;
            Ok(value)
        })();
        (read, result)
    }

    /// 字体选择只能服务签发时的原nonce；相同范围、同文本或同Source都不能代替它。
    pub fn accepts_request(&self, request: &MarkdownLineStyleRequest) -> Result<bool> {
        self.validate()?;
        Ok(Arc::ptr_eq(&self.request.nonce, &request.nonce)
            && self.request.source == request.source)
    }

    /// 方向、Script及BOT/EOT沿原UBA/段落事实；调用者仅提供原有限样式。
    ///
    /// # Errors
    /// 原权限失效或样式超出原Core固定预算时拒绝，不截断language/features或回退默认值。
    pub fn shaping_options(
        &self,
        style: &MarkdownLineComposeStyle,
    ) -> Result<MarkdownShapingOptions> {
        self.validate()?;
        let text = self.request.text_scope()?;
        let segment = self.ready_segment()?;
        if !style.points.is_finite()
            || style.points <= 0.0
            || style.features.len() > crate::MARKDOWN_SHAPING_MAX_FEATURES
            || style
                .language
                .as_ref()
                .is_some_and(|value| value.len() > 64)
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        Ok(MarkdownShapingOptions {
            direction: if segment.level & 1 == 0 {
                MarkdownShapingDirection::LeftToRight
            } else {
                MarkdownShapingDirection::RightToLeft
            },
            script: text.script,
            language: style.language.clone(),
            points: style.points,
            beginning_of_text: segment.utf16.start == text.paragraph.start,
            end_of_text: segment.utf16.end == text.paragraph.end,
            features: style.features.clone(),
        })
    }
}

impl MarkdownLineItemCoordinator {
    /// 下一段只能继续当前原 Style nonce；另一个协调器、旧 scope 或已取消来源不能借整数复位。
    fn require_font_probe(&mut self, probe: &MarkdownLineFontProbe) -> Result<()> {
        self.guard()?;
        probe.validate()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        let request = payload
            .request
            .as_ref()
            .ok_or(MarkdownLineComposeError::InvalidRequest)?;
        let scope = payload
            .font_probe_scope
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        if !matches!(payload.stage, Stage::Style)
            || !Arc::ptr_eq(&request.nonce, &probe.request.nonce)
            || request.source != probe.request.source
            || !std::sync::Weak::ptr_eq(&probe.scope, &Arc::downgrade(scope))
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        Ok(())
    }

    /// 一个真实候选的 worker 已结束当前段后才调用；每次只保一个实际 level 段。
    /// 原 Bidi/Script 目录仍在唯一协调器，worker 不带这些非 Send owner，也不建立新索引。
    pub fn step_font_probe_segment(
        &mut self,
        probe: &mut MarkdownLineFontProbe,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineFontProbeSegmentStep> {
        let result = (|| {
            self.require_font_probe(probe)?;
            if budget.metadata_units == 0 {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let payload = self.payload.as_mut().expect("原试次已经核同唯一协调器");
            let bidi = payload
                .bidi
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let mut step = MarkdownLineFontProbeSegmentStep {
                bytes: 0,
                scalars: 0,
                records: 0,
                metadata_units: 0,
                answer: MarkdownLineFontProbeSegmentAnswer::Pending,
            };
            if probe.complete {
                step.answer = MarkdownLineFontProbeSegmentAnswer::Complete;
                return Ok(step);
            }
            if probe.shaping_segment_ready()? {
                let prior = probe.segment.take().expect("Ready 持唯一实际段");
                if prior.utf16.end == probe.range.end {
                    probe.complete = true;
                    probe.scan = None;
                    step.answer = MarkdownLineFontProbeSegmentAnswer::Complete;
                    return Ok(step);
                }
                let held = probe.scan.take().and_then(|scan| scan.held);
                probe.scan = Some(SegmentScan::new(prior.utf16.end, prior.bytes.end, held));
            }
            let limit = budget.metadata_units.min(METADATA_UNITS);
            while step.metadata_units < limit {
                let scan = probe
                    .scan
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                if scan.context_phase == 0 {
                    if scan.next == probe.range.end {
                        let level = scan
                            .level
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        probe.segment = Some(ProbeSegment {
                            utf16: scan.start..scan.next,
                            bytes: scan.byte..probe.bytes.end,
                            level,
                            context: None,
                        });
                        scan.context_phase = 1;
                        scan.context_cursor = scan.start;
                        continue;
                    }
                    let unit = if let Some(unit) = scan.held.take() {
                        unit // 原上一步已收费的右首记录只移交一次，不重复借目录或重复计费。
                    } else {
                        step.metadata_units += 1;
                        bidi.shaping_unit_at_utf16(&self.authority, scan.next)?
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                    };
                    if unit.utf16 != scan.next || !unit.permits(&self.authority) {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    }
                    if let Some(level) = scan.level {
                        if unit.level != level {
                            if unit.continuation {
                                return Err(MarkdownLineComposeError::InconsistentCoverage);
                            }
                            probe.segment = Some(ProbeSegment {
                                utf16: scan.start..unit.utf16,
                                bytes: scan.byte..unit.byte,
                                level,
                                context: None,
                            });
                            scan.held = Some(unit);
                            scan.context_phase = 1;
                            scan.context_cursor = scan.start;
                            continue;
                        }
                    } else {
                        if unit.continuation || unit.byte != scan.byte {
                            return Err(MarkdownLineComposeError::InconsistentCoverage);
                        }
                        scan.level = Some(unit.level);
                    }
                    scan.next += 1;
                    continue;
                }
                let segment = probe.segment.as_mut().expect("实际段已由原 level 记录封闭");
                let paragraph = &probe.request.text_scope()?.paragraph;
                if scan.context_phase == 1 {
                    // 原 shaper 至多借前 20B；仅在这个固定包络内寻找实际 level 屏障。
                    // 允许窗首落 UTF8 中部，仍交原 decode_context 舍弃不完整的首 scalar。
                    let floor = segment.bytes.start.saturating_sub(20);
                    if scan.context_cursor == paragraph.start || scan.context.start <= floor {
                        scan.context_phase = 2;
                        scan.context_cursor = segment.utf16.end;
                        scan.context.end = segment.bytes.end;
                        continue;
                    }
                    step.metadata_units += 1;
                    let unit = bidi
                        .shaping_unit_at_utf16(&self.authority, scan.context_cursor - 1)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                    if unit.level != segment.level {
                        scan.context_phase = 2;
                        scan.context_cursor = segment.utf16.end;
                        scan.context.end = segment.bytes.end;
                    } else {
                        scan.context.start = unit.byte.max(floor);
                        scan.context_cursor -= 1;
                    }
                    continue;
                }
                if scan.context_phase == 2 {
                    let ceiling = segment
                        .bytes
                        .end
                        .saturating_add(20)
                        .min(probe.source.byte_len());
                    if scan.context.end >= ceiling || scan.context_cursor == paragraph.end {
                        segment.context = Some(scan.context.clone());
                        scan.context_phase = 3;
                        step.answer = MarkdownLineFontProbeSegmentAnswer::Ready;
                        return Ok(step);
                    }
                    step.metadata_units += 1;
                    let unit = bidi
                        .shaping_unit_at_utf16(&self.authority, scan.context_cursor)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                    if unit.level != segment.level || unit.byte >= ceiling {
                        scan.context.end = unit.byte.min(ceiling);
                        segment.context = Some(scan.context.clone());
                        scan.context_phase = 3;
                        step.answer = MarkdownLineFontProbeSegmentAnswer::Ready;
                        return Ok(step);
                    }
                    // 同一条记录给出完整 scalar 尾；到当前段末即可结束，不向尚未准备的后段借票。
                    // 原上下文最多 20B，窗尾切在 UTF8 内仍由唯一 decode_context 合同处理。
                    scan.context.end = unit.byte_end.min(ceiling);
                    scan.context_cursor += 1;
                    continue;
                }
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            probe.validate()?;
            Ok(step)
        })();
        self.finish(result)
    }

    /// 当前候选任一段缺字后，原唯一 owner 才能重试同 GCB 的下一真实字体。
    /// 不替换 Source/nonce/取消位；已失败、取消或已消费请求绝不能复活。
    pub fn restart_font_probe(&mut self, probe: &mut MarkdownLineFontProbe) -> Result<()> {
        let result = (|| {
            self.require_font_probe(probe)?;
            probe.complete = false;
            if probe.single_segment {
                probe.segment = Some(ProbeSegment {
                    utf16: probe.range.clone(),
                    bytes: probe.bytes.clone(),
                    level: probe.request.text_scope()?.level,
                    context: None,
                });
                probe.scan = None;
            } else {
                probe.segment = None;
                probe.scan = Some(SegmentScan::new(probe.range.start, probe.bytes.start, None));
            }
            probe.validate()
        })();
        self.finish(result)
    }

    /// 从当前唯一NeedStyle的原依赖领取首个完整GCB；固定次数目录查询不扫描字素正文。
    ///
    /// 返回值只证明此一个GCB的Script、UBA和来源，不能将其字体选择扩展到整个item。
    ///
    /// # Errors
    /// 非当前nonce、阶段不符或原依赖不连续时撤销该协调器；失败不能另外创建索引补签。
    pub fn font_probe(
        &mut self,
        request: &MarkdownLineStyleRequest,
    ) -> Result<MarkdownLineFontProbe> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let original = payload
                .request
                .as_ref()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            if !matches!(payload.stage, Stage::Style)
                || !Arc::ptr_eq(&original.nonce, &request.nonce)
                || original.source != request.source
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            // 真实结构握手没有 Unicode/Font 权限；必须在 GCB、Script 查询之前明确拒绝。
            let text = original.text_scope()?;
            #[cfg(feature = "paged-shaping-frontier")]
            if original.continuation_font.is_some()
                && original.shaping_seams.iter().all(Option::is_none)
            {
                // 原Frontier或自然闭项只在缺字/相邻GCB两张原端点票齐备后签发request；它本身
                // 就是完整GCB，继续借同一个Style scope，不再同步重查原索引或正文。
                let scope = payload
                    .font_probe_scope
                    .as_ref()
                    .expect("原字体交接请求必须持试次scope");
                return Ok(MarkdownLineFontProbe {
                    source: payload.source.clone(),
                    request: original.clone(),
                    range: original.range.clone(),
                    bytes: original
                        .continuation_bytes
                        .clone()
                        .expect("原字体交接已持同 GCB 字节端点"),
                    scope: Arc::downgrade(scope),
                    cancellation: self.cancellation.clone(),
                    segment: Some(ProbeSegment {
                        utf16: original.range.clone(),
                        bytes: original
                            .continuation_bytes
                            .clone()
                            .expect("原交接持真实字节端点"),
                        level: text.level,
                        context: None,
                    }),
                    scan: None,
                    single_segment: true,
                    complete: false,
                });
            }
            // 原 shaping seam 可以在真实 GCB 内部；只从同一个原 GCB 目录取外层完整字素。
            // 它用于候选字体的全字素试次，不能把原样式响应范围扩大到这个外层范围。
            let start = request.range.start;
            let start_boundary = match payload.breaks.grapheme_query(
                &self.authority,
                start,
                MarkdownGraphemeQuery::IsBoundary,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 == start => value,
                MarkdownGraphemeAnswer::Known(None) if original.shaping_seams[0].is_some() => {
                    match payload.breaks.grapheme_query(
                        &self.authority,
                        start,
                        MarkdownGraphemeQuery::Previous,
                    )? {
                        MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 < start => value,
                        _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
                    }
                }
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            let end_boundary = match payload.breaks.grapheme_query(
                &self.authority,
                start_boundary.utf16,
                MarkdownGraphemeQuery::Next,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value))
                    if value.utf16 > start && value.utf16 <= text.paragraph.end =>
                {
                    value
                }
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            let end = end_boundary.utf16;
            let whole_start = start_boundary.utf16;
            let single_segment = whole_start == start && end <= request.range.end;
            // Script来自原已完成的同一个Job，不能只看首scalar或新建局部run猜整个GCB。
            #[cfg(feature = "paged-shaping-frontier")]
            let script = match payload.scripts.stable_prefix_at(
                &self.authority,
                MarkdownScriptUnit::Utf16,
                whole_start,
            )? {
                crate::markdown_scripts::MarkdownScriptPrefixAnswer::Known(Some(prefix)) => {
                    Some(prefix.facts()?)
                }
                _ => None,
            };
            #[cfg(not(feature = "paged-shaping-frontier"))]
            let script = match payload.scripts.run_at(
                &self.authority,
                MarkdownScriptUnit::Utf16,
                whole_start,
            )? {
                MarkdownScriptAnswer::Known(value) => value,
                MarkdownScriptAnswer::Pending => None,
            };
            match script {
                Some(run)
                    if run.start_utf16 <= whole_start
                        && run.end_utf16 >= end
                        && run.script.iso15924_tag() == text.script => {}
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            }
            let scope = payload
                .font_probe_scope
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            Ok(MarkdownLineFontProbe {
                source: payload.source.clone(),
                request: original.clone(),
                range: whole_start..end,
                bytes: start_boundary.byte..end_boundary.byte,
                scope: Arc::downgrade(scope),
                cancellation: self.cancellation.clone(),
                segment: single_segment.then_some(ProbeSegment {
                    utf16: whole_start..end,
                    bytes: start_boundary.byte..end_boundary.byte,
                    level: text.level,
                    context: None,
                }),
                scan: (!single_segment)
                    .then(|| SegmentScan::new(whole_start, start_boundary.byte, None)),
                single_segment,
                complete: false,
            })
        })();
        self.finish(result)
    }
}
