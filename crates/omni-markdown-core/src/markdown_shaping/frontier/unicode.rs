//! 同一前沿的真实单scalar字素域；源批次、元数据页与原始段落边界互不混同。
use super::*;
use crate::MarkdownGraphemeBoundary;

const EMPTY: MarkdownGraphemeBoundary = MarkdownGraphemeBoundary { byte: 0, utf16: 0 };
const LIMIT: usize = MARKDOWN_SHAPING_FRONTIER_UNITS;

/// 固定坐标元数据和一个4KiB窗口；不保存全文、不建立第二份GCB/cluster索引。
pub(super) struct UnicodeState {
    plan: Option<harfrust::SimpleGcbFrontierJob>,
    certified: bool,
    first_checked: bool,
    boundaries: [MarkdownGraphemeBoundary; LIMIT + 2],
    count: usize,
    ordinal: Option<usize>,
    locate: Option<(usize, usize)>,
    total: Option<usize>,
    next_boundary: Option<MarkdownGraphemeBoundary>,
    bytes: [u8; 4096],
    from: usize,
    available: usize,
    byte: usize,
    utf16: usize,
    pending: [u8; 4],
    used: usize,
    needed: usize,
    scanned: bool,
    center_count: usize,
    verified: usize,
}

impl UnicodeState {
    /// 仅交付已完整读到的阻塞GCB；原自然SCX末端仍限制范围，不把预算边界当末端。
    pub(super) fn terminal_grapheme(
        &self,
        original_end: usize,
    ) -> Option<(Range<usize>, Range<usize>)> {
        let end = self.next_boundary?;
        let start = self.boundaries[self.count];
        (self.certified
            && !self.scanned
            && self.count != 0
            && self.count < LIMIT
            && self.used == self.needed
            && self.used != 0
            && self.byte == end.byte
            && self.utf16 == start.utf16
            && end.utf16 <= original_end)
            .then_some((start.utf16..end.utf16, start.byte..end.byte))
    }

    /// 本次scalar在暂停前已计费，恢复只消费原固定scratch，不再次读取或计数。
    pub(super) fn resume_terminal_grapheme(&mut self, same_font: bool) -> usize {
        if same_font {
            let end = self
                .next_boundary
                .take()
                .expect("原末GCB仍在唯一Unicode游标内");
            self.count += 1;
            self.boundaries[self.count] = end;
            self.utf16 = end.utf16;
        } else {
            // 已证实字体变化时保留旧Font的完整残余；阻塞GCB稍后由新Font单独消费。
            self.byte = self.boundaries[self.count].byte;
            self.next_boundary = None;
        }
        self.used = 0;
        self.needed = 0;
        self.scanned = true;
        self.center_count = self.count;
        self.boundaries[self.count].utf16
    }

    pub(super) fn new(byte: usize, utf16: usize) -> Self {
        Self {
            plan: None,
            certified: false,
            first_checked: false,
            boundaries: [EMPTY; LIMIT + 2],
            count: 0,
            ordinal: None,
            locate: None,
            total: None,
            next_boundary: None,
            bytes: [0; 4096],
            from: 0,
            available: 0,
            byte,
            utf16,
            pending: [0; 4],
            used: 0,
            needed: 0,
            scanned: false,
            center_count: 0,
            verified: 0,
        }
    }
    pub(super) fn center_byte(&self) -> usize {
        self.boundaries[self.center_count].byte
    }
    pub(super) fn support_end(&self) -> usize {
        self.boundaries[self.count].utf16
    }
    pub(super) fn require_independent_route(&mut self) {
        self.first_checked = true; // 路由由原开放SCX票约束，不读取首byte后转入有上下文的ASCII计划。
    }
    pub(super) fn from_ascii_scan(
        byte: usize,
        utf16: usize,
        plan: harfrust::SimpleGcbFrontierJob,
    ) -> Self {
        let mut state = Self::new(byte, utf16);
        state.first_checked = true;
        state.plan = Some(plan);
        state // 仅跳过已发生的路由探针；Source/GCB/normalizer和真实计划仍须原逐项认证。
    }
    pub(super) fn reset(&mut self, byte: usize, utf16: usize) {
        // 仅原child实际归还后的FIFO续票可进入。保留已验Font计划，不保留旧正文scratch。
        self.ordinal = self.ordinal.map(|start| start + self.center_count);
        self.boundaries[0] = MarkdownGraphemeBoundary { byte, utf16 };
        self.count = 0;
        self.from = 0;
        self.available = 0;
        self.byte = byte;
        self.utf16 = utf16;
        self.used = 0;
        self.needed = 0;
        self.next_boundary = None;
        self.scanned = false;
        self.center_count = 0;
        self.verified = 0;
    }

    /// 外来Font段消耗了不同数量的真实GCB，不能继续用旧center_count推算ordinal。
    /// 保留原Font计划，下一量子在同一原GCB索引中有界定位实际返回点。
    pub(super) fn reset_after_font_run(&mut self, byte: usize, utf16: usize) {
        self.reset(byte, utf16);
        self.ordinal = None;
        self.locate = None;
        self.total = None;
    }
    fn remaining(step: &MarkdownShapingFrontierStep, budget: usize) -> FrontierResult<usize> {
        budget
            .checked_sub(step.plan_records + step.metadata_glyphs + step.boundary_records)
            .ok_or_else(|| MarkdownShapingError::InvalidOptions.into())
    }
    fn boundary(
        breaks: &mut MarkdownLayoutBreakJob,
        source: &MarkdownLayoutTextAuthority,
        ordinal: usize,
    ) -> FrontierResult<Option<MarkdownGraphemeBoundary>> {
        let mut one = [EMPTY];
        if !breaks.read_grapheme_prefix(source, ordinal..ordinal + 1, &mut one)? {
            return Ok(None);
        }
        Ok(Some(one[0])) // 原页已结束借用；只保留真实byte/UTF16坐标，不持索引或Source owner。
    }

    fn locate(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        source: &MarkdownLayoutTextAuthority,
        step: &mut MarkdownShapingFrontierStep,
        budget: usize,
    ) -> FrontierResult<bool> {
        if self.ordinal.is_some() {
            return Ok(true);
        }
        let prefix = breaks.grapheme_prefix_facts(source)?;
        if self.total != Some(prefix.known_boundary_count) {
            self.total = Some(prefix.known_boundary_count);
            self.locate = Some((0, prefix.known_boundary_count));
        } // 只在真实已提交的ordinal前缀中定位；这里的count不表示完整总数。
        while Self::remaining(step, budget)? != 0 {
            let (low, high) = self.locate.ok_or(MarkdownShapingError::Pending)?;
            if low >= high {
                if prefix.complete {
                    return Err(MarkdownShapingError::InvalidRange.into());
                }
                step.boundary_records += 1;
                Self::boundary(breaks, source, prefix.known_boundary_count)?;
                return Ok(false); // 未知后继只留下同GCB需求，不从GB2坐标猜ordinal。
            }
            let middle = low + (high - low) / 2;
            step.boundary_records += 1;
            let Some(value) = Self::boundary(breaks, source, middle)? else {
                return Ok(false);
            };
            if value.utf16 < self.utf16 {
                self.locate = Some((middle + 1, high));
            } else if value.utf16 > self.utf16 {
                self.locate = Some((low, middle));
            } else {
                if value.byte != self.byte {
                    return Err(MarkdownShapingError::InvalidRange.into());
                }
                self.ordinal = Some(middle);
                self.boundaries[0] = value;
                self.locate = None;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl MarkdownShapingFrontierJob {
    /// 与原ASCII分支共享owner/计划/资源scope；新的坐标验证不接受外部公开Range签权。
    pub(super) fn advance_unicode(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownShapingBudget,
        metadata_budget: usize,
        mut step: MarkdownShapingFrontierStep,
    ) -> FrontierResult<MarkdownShapingFrontierStep> {
        let state = self.unicode.as_mut().ok_or(MarkdownShapingError::Pending)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        let plan = payload
            .plan
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        if !state.first_checked {
            let mut first = None;
            payload
                .source
                .visit_byte_range(self.next_byte..self.next_byte + 1, |part| {
                    first = part.first().copied();
                    Ok(())
                })
                .map_err(MarkdownShapingError::from)?;
            step.source_bytes = 1;
            state.first_checked = true;
            let first = first.ok_or(MarkdownShapingError::InvalidUtf8)?;
            if first < 0x80 {
                if (first == b' ' || first.is_ascii_punctuation())
                    && let Ok(job) = plan.simple_gcb_job(&payload.font)
                {
                    // Common标点/space本来可以与后继Han等同属原item。只有原实际计划
                    // 确认有窄作业资格才保留本路由；真实GCB、所有stage和glyph仍逐项验证。
                    state.plan = Some(job);
                } else {
                    // 路由探针已经在同一原Source票下取得首byte；转回ASCII时直接移交
                    // 固定scratch，避免下一步再读同一byte。实际读取仍在本步计费一次，
                    // 后继step先核原Source/Font再消费，不把探针结果当后续字符的证明。
                    self.scan[0] = first;
                    self.scanned = 1;
                    self.unicode = None;
                    return Ok(step); // 不可认证计划和字母/数字起点保留原ASCII/Body及SHY负控路径。
                }
            }
        }
        if !state.certified {
            if state.plan.is_none() {
                match plan.simple_gcb_job(&payload.font) {
                    Ok(job) => state.plan = Some(job),
                    Err(reason) => {
                        self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                            MarkdownShapingFrontierReason::Plan(reason),
                        );
                        step.answer = self.answer;
                        return Ok(step);
                    }
                }
            }
            let job = state.plan.as_mut().ok_or(MarkdownShapingError::Pending)?;
            let before = job.checked_records();
            let result = job.step(&plan.plan(), payload.font.font(), metadata_budget);
            step.plan_records = job.checked_records() - before;
            match result {
                Ok(Some(_)) => {
                    state.certified = true;
                    state.plan = None;
                }
                Ok(None) => return Ok(step),
                Err(reason) => {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::Plan(reason),
                    );
                    step.answer = self.answer;
                    return Ok(step);
                }
            }
        }
        if !state.scanned {
            if !state.locate(breaks, source, &mut step, metadata_budget)? {
                return Ok(step);
            }
            while state.count <= LIMIT && step.scalars < budget.scalars {
                if state.byte == self.original_bytes.end && state.used == 0 {
                    if state.utf16 != self.original.end {
                        return Err(MarkdownShapingError::InvalidUtf8.into());
                    }
                    state.scanned = true;
                    break;
                }
                if state.next_boundary.is_none() {
                    if UnicodeState::remaining(&step, metadata_budget)? == 0 {
                        break;
                    }
                    let ordinal =
                        state.ordinal.ok_or(MarkdownShapingError::Pending)? + state.count + 1;
                    step.boundary_records += 1;
                    let Some(next) = UnicodeState::boundary(breaks, source, ordinal)? else {
                        return Ok(step);
                    };
                    let previous = state.boundaries[state.count];
                    if next.byte <= previous.byte
                        || next.utf16 <= previous.utf16
                        || next.utf16 > self.original.end
                    {
                        return Err(MarkdownShapingError::InvalidRange.into());
                    }
                    if next.byte - previous.byte > 4 {
                        self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                            MarkdownShapingFrontierReason::NonSimpleGrapheme,
                        );
                        step.answer = self.answer;
                        return Ok(step); // 完整大GCB保持未解析，绝不读取前4字节后冒充可切分。
                    }
                    state.next_boundary = Some(next);
                }
                if (state.used == 0 || state.used != state.needed) && state.from == state.available
                {
                    if state.byte == self.original_bytes.end {
                        return Err(MarkdownShapingError::InvalidUtf8.into());
                    }
                    let available = budget.bytes - step.source_bytes;
                    let count = available
                        .min(budget.scalars - step.scalars)
                        .min(4096)
                        .min(self.original_bytes.end - state.byte)
                        .min(
                            state
                                .next_boundary
                                .ok_or(MarkdownShapingError::Pending)?
                                .byte
                                - state.byte,
                        );
                    // 只读同一个已定位GCB的剩余字节；3字节见证后不能预取未知第258个标量。
                    if count == 0 {
                        break;
                    }
                    let mut copied = 0;
                    payload
                        .source
                        .visit_byte_range(state.byte..state.byte + count, |part| {
                            state.bytes[copied..copied + part.len()].copy_from_slice(part);
                            copied += part.len();
                            Ok(())
                        })
                        .map_err(MarkdownShapingError::from)?;
                    step.source_bytes += copied;
                    if copied != count {
                        return Err(MarkdownShapingError::InvalidUtf8.into());
                    }
                    state.from = 0;
                    state.available = count;
                }
                while (state.used == 0 || state.used != state.needed)
                    && state.from < state.available
                    && state.used < 4
                {
                    let byte = state.bytes[state.from];
                    if state.used == 0 {
                        state.needed = match byte {
                            0..=0x7f => 1,
                            0xc2..=0xdf => 2,
                            0xe0..=0xef => 3,
                            0xf0..=0xf4 => 4,
                            _ => return Err(MarkdownShapingError::InvalidUtf8.into()),
                        };
                    }
                    state.pending[state.used] = byte;
                    state.used += 1;
                    state.from += 1;
                    state.byte += 1;
                    if state.used == state.needed {
                        break;
                    }
                }
                if state.used != state.needed {
                    continue;
                } // 1byte量子保留同一个UTF8残余，不当作EOF。
                if UnicodeState::remaining(&step, metadata_budget)? == 0 {
                    break;
                }
                let scalar = std::str::from_utf8(&state.pending[..state.used])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .ok_or(MarkdownShapingError::InvalidUtf8)?;
                // 解码已真实完成，后续GCB或类别拒绝也要保留这次工作，不能把负控记成零scalar。
                step.scalars += 1;
                let next = state.next_boundary.ok_or(MarkdownShapingError::Pending)?;
                if state.byte != next.byte || state.utf16 + scalar.len_utf16() != next.utf16 {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::NonSimpleGrapheme,
                    );
                    step.answer = self.answer;
                    return Ok(step); // 两个scalar组成的真实字素不能靠single-input字体许可拆开。
                }
                step.metadata_glyphs += 1;
                match harfrust::simple_gcb_nominal(payload.font.font(), scalar) {
                    Ok(_) => {}
                    Err(reason) => {
                        self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                            MarkdownShapingFrontierReason::Plan(reason),
                        );
                        step.answer = self.answer;
                        return Ok(step);
                    }
                } // nominal只是normalizer短路见证；最终无缺字仍必须真实shape逐glyph核。
                state.utf16 = next.utf16;
                state.used = 0;
                state.needed = 0;
                state.count += 1;
                state.boundaries[state.count] = next;
                state.next_boundary = None;
                if state.count == LIMIT + 1 {
                    state.scanned = true;
                    break;
                }
            }
            if !state.scanned {
                return Ok(step);
            }
            state.center_count = if self.open_scope && state.support_end() == self.original.end {
                state.count.saturating_sub(1).min(LIMIT)
            } else {
                state.count.min(LIMIT)
            };
            if state.center_count == 0 {
                return Err(MarkdownShapingError::InvalidRange.into());
            }
            // 除真实item尾，保留一个已证独立的first-unread GCB。tracking逐原grapheme只写自身；
            // 原fallback mark扫描在这个真实非mark后继处结束，不能借未知未来修改中心flags。
            self.candidate = Some(state.boundaries[state.center_count].utf16);
        }
        let end = self.candidate.ok_or(MarkdownShapingError::Pending)?;
        if self.child.is_none() {
            let support_end = state.boundaries[state.count];
            let mut options = payload.options.clone();
            options.beginning_of_text &= self.next == self.original.start;
            options.end_of_text &= support_end.utf16 == self.original.end;
            let mut child = MarkdownShapingJob::assemble(
                payload.source.clone(),
                source,
                payload.font.clone(),
                font,
                (
                    self.next..support_end.utf16,
                    self.next_byte..support_end.byte,
                ),
                options,
                self.cancellation.clone(),
            )?;
            if let Some(resources) = &self.resources {
                child.attach_composer_resources(resources)?;
            }
            step.context_bytes = child.profile()?.source_bytes_read;
            child
                .payload
                .as_mut()
                .ok_or(MarkdownShapingError::Poisoned)?
                .frontier_plan = Some(plan.plan());
            self.child = Some(child); // 和原ASCII一样在任何ingest/shape前纳入原scope。
        }
        let child = self.child.as_mut().ok_or(MarkdownShapingError::Poisoned)?;
        if child.status() == MarkdownShapingStatus::Ingesting {
            let bytes = budget.bytes - step.source_bytes;
            let scalars = budget.scalars - step.scalars;
            if bytes == 0 || scalars == 0 {
                return Ok(step);
            }
            let input = child.step(source, font, MarkdownShapingBudget { bytes, scalars })?;
            step.input_bytes = input.bytes;
            step.scalars += input.scalars;
        }
        if child.status() == MarkdownShapingStatus::AwaitingShape {
            child.shape(source, font)?;
            step.shapes = 1;
        }
        if child.status() == MarkdownShapingStatus::Ready {
            child.guard(source, font)?;
            let raw = child
                .payload
                .as_ref()
                .ok_or(MarkdownShapingError::Poisoned)?;
            if raw.frontier_view.is_none() {
                if raw.buffer.len() != state.count {
                    return Err(MarkdownShapingError::InvalidRange.into());
                }
                while state.verified < state.count
                    && UnicodeState::remaining(&step, metadata_budget)? != 0
                {
                    let (info, _) = raw
                        .buffer
                        .paged_glyph_record(state.verified)
                        .ok_or(MarkdownShapingError::ShaperStorage)?;
                    step.metadata_glyphs += 1;
                    if info.cluster as usize != state.boundaries[state.verified].utf16
                        || !harfrust::simple_gcb_output_is_base(payload.font.font(), info.glyph_id)
                    {
                        return Err(MarkdownShapingError::InvalidRange.into());
                    }
                    state.verified += 1;
                } // 逐真实1→1与global cluster核查，非BMP的2个UTF16单位也只有一个真实ordinal。
                if state.verified != state.count {
                    return Ok(step);
                }
                let support = child.source_utf16.clone();
                let view = local::CertifiedCenterView::new(
                    &self.identity,
                    child,
                    self.next..end,
                    0..state.center_count,
                    support,
                )?;
                child
                    .payload
                    .as_mut()
                    .ok_or(MarkdownShapingError::Poisoned)?
                    .frontier_view = Some(view);
            }
            child.guard(source, font)?;
            self.answer = MarkdownShapingFrontierAnswer::PartReady;
        }
        step.answer = self.answer;
        Ok(step)
    }
}
