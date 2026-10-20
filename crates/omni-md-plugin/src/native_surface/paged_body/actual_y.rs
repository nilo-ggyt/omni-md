//! 同Producer实际行度量的目标续体；只有有限游标，没有额外Reader、全行索引或估计行高。

use super::super::navigation::{PendingVertical, VerticalMotion, VerticalSeed};
use super::super::target::SourceTargetRequest;
use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedNavigation as Nav;
use objc2_foundation::{NSRect, NSSize};
use omni_markdown_core::MarkdownComposedLineAffinity;

pub(super) enum TargetAdvance {
    NeedPrefix,
    Handled,
    VisibleBand,
}

#[derive(Clone)]
pub(super) struct MeasuredLineAnchor {
    pub(super) ordinal: usize,
    pub(super) top: f64,
    pub(super) bottom: f64,
}

struct Walk {
    cursor: Option<MarkdownComposedLine>,
    metrics: Option<LineMetrics>,
    bottom: f64,
}

struct BottomSeed {
    ordinal: usize,
    bottom: f64,
}

pub(super) struct ActualTarget {
    pub(super) request: Rc<SourceTargetRequest>,
    lookup: Option<MarkdownComposedLineLookup>,
    ordinal: Option<usize>,
    seed: Option<MarkdownComposedLineLookup>,
    walk: Option<Walk>,
    anchor: Option<MeasuredLineAnchor>,
}

/// 垂直导航只暂存一个原行查找/度量游标，沿唯一output累加真实相邻行高度。
pub(super) struct VerticalTarget {
    request: Rc<PendingVertical>,
    seed: VerticalSeed,
    motion: VerticalMotion,
    ordinal: Option<usize>,
    boundary: f64,
    previous: MeasuredLineAnchor,
    lookup: Option<MarkdownComposedLineLookup>,
    metrics: Option<LineMetrics>,
    anchor: Option<MeasuredLineAnchor>,
}
impl VerticalTarget {
    pub(super) fn reset_output(&mut self) {
        self.lookup = None;
        self.metrics = None; // 新prefix归还原弱游标；已完成的真实边界属于同一Source/宽度，不重读之前的行。
    }
    fn new(request: Rc<PendingVertical>, motion: VerticalMotion) -> Self {
        let seed = request.seed.get();
        let backwards = matches!(motion.action, Nav::Up | Nav::PageUp);
        Self {
            seed,
            motion,
            ordinal: if backwards {
                seed.ordinal.checked_sub(1)
            } else {
                seed.ordinal.checked_add(1)
            },
            boundary: if backwards { seed.top } else { seed.bottom },
            previous: MeasuredLineAnchor {
                ordinal: seed.ordinal,
                top: seed.top,
                bottom: seed.bottom,
            },
            request,
            lookup: None,
            metrics: None,
            anchor: None,
        }
    }
}

impl ActualTarget {
    pub(super) fn has_anchor(&self) -> bool {
        self.anchor.is_some()
    }
    fn new(request: Rc<SourceTargetRequest>) -> Self {
        Self {
            request,
            lookup: None,
            ordinal: None,
            seed: None,
            walk: None,
            anchor: None,
        }
    }
    pub(super) fn reset_output(&mut self) {
        // 新prefix释放旧Output后不能沿旧弱handle继续；已真实测出的坐标仍属于同Producer。
        self.lookup = None;
        self.ordinal = None;
        self.seed = None;
        self.walk = None;
    }
    pub(super) fn observe_line(
        &mut self,
        facts: &MarkdownComposedLineFacts,
        top: f64,
        bottom: f64,
    ) {
        let target = self.request.range.location;
        if target >= facts.source_utf16.start
            && target < facts.source_utf16.end
            && self.request.first_end <= facts.source_utf16.end
        {
            self.anchor = Some(MeasuredLineAnchor {
                ordinal: facts.ordinal,
                top,
                bottom,
            });
        }
    }
}

impl Producer {
    /// 一轮只查同一output至多256条真实目录记录和32个part；不足时续原游标，不同步排完整段。
    pub(super) fn advance_vertical_target(
        &mut self,
        owner: &Owner,
        actual: &NativeMarkdownPagedSceneRequest,
        ticket: &PreparationTicket,
    ) -> Result<TargetAdvance> {
        let pending = owner.pending_vertical.borrow().clone();
        let Some(pending) = pending.filter(|pending| {
            pending.matches(owner, actual) && pending.geometry.source == self.source.authority()
        }) else {
            self.vertical = None;
            return Ok(TargetAdvance::VisibleBand);
        };
        let motion = pending.motion().ok_or(ViewError::Revoked)?;
        if !self.vertical.as_ref().is_some_and(|target| {
            Rc::ptr_eq(&target.request, &pending)
                && target.seed == pending.seed.get()
                && target.motion == motion
        }) {
            self.vertical = Some(VerticalTarget::new(pending, motion));
        }
        let mut target = self.vertical.take().ok_or(Error::Closed)?;
        let result = self.advance_vertical_line(owner, ticket, &mut target);
        self.vertical = Some(target);
        result
    }

    fn advance_vertical_line(
        &mut self,
        owner: &Owner,
        ticket: &PreparationTicket,
        target: &mut VerticalTarget,
    ) -> Result<TargetAdvance> {
        ticket.require_pure(owner, &self.source)?;
        let backwards = matches!(target.motion.action, Nav::Up | Nav::PageUp);
        let page = matches!(target.motion.action, Nav::PageUp | Nav::PageDown);
        let desired_y = target.seed.caret_y
            + if backwards {
                -target.request.request.viewport.size.height
            } else {
                target.request.request.viewport.size.height
            };
        let mut records = 256usize;
        let mut parts = METADATA_PER_TURN;
        while target.anchor.is_none() {
            if !target.request.current(owner) {
                return Ok(TargetAdvance::VisibleBand);
            }
            let Some(ordinal) = target.ordinal else {
                target.anchor = Some(target.previous.clone()); // 真BOF已由原ordinal0证明，不把可见前缀当文首。
                break;
            };
            if ordinal >= self.lines {
                if !self.complete {
                    return Ok(TargetAdvance::NeedPrefix);
                }
                target.anchor = Some(target.previous.clone()); // 只有原Composer complete才可把末个真实行作为EOF边界。
                break;
            }
            let Some(output) = self.output.as_ref().cloned() else {
                return Ok(TargetAdvance::NeedPrefix);
            };
            if target.metrics.is_none() {
                if records == 0 {
                    owner.schedule()?;
                    return Ok(TargetAdvance::Handled);
                }
                if target.lookup.is_none() {
                    target.lookup = Some(
                        output
                            .borrow_mut()
                            .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(ordinal))?,
                    );
                }
                let step = output.borrow_mut().step_line_lookup(
                    target.lookup.as_mut().ok_or(Error::Closed)?,
                    records.min(METADATA_PER_TURN),
                )?;
                records -= step.records;
                self.target_lookup_records = self
                    .target_lookup_records
                    .checked_add(step.records)
                    .ok_or(Error::CounterExhausted)?;
                self.target_index_records = self
                    .target_index_records
                    .checked_add(step.records)
                    .ok_or(Error::CounterExhausted)?;
                match step.answer {
                    MarkdownComposedLineLookupAnswer::Pending => {
                        owner.schedule()?;
                        return Ok(TargetAdvance::Handled);
                    }
                    MarkdownComposedLineLookupAnswer::Found(line)
                        if line.facts().ordinal == ordinal =>
                    {
                        target.metrics =
                            Some(LineMetrics::new(line, owner.presentation_mode(), false));
                        target.lookup = None;
                    }
                    _ => return Err(Error::Unsupported), // 已度量前缀缺失原ordinal是源/目录错误，不签空目标。
                }
            }
            let mut metrics = target.metrics.take().ok_or(Error::Closed)?;
            // 一个part实际读取Line认证2条、Part2条、Directory3条；只领取本轮真实record余量。
            let allowed = parts.min(records / 7);
            let mut quota = MarkdownLineComposeBudget {
                metadata_units: allowed,
                records,
                ..LINE_METRIC_BUDGET
            };
            ticket.require(owner, &self.source)?;
            let mut work = grid::Work::default();
            let ready = metrics.advance(&output, owner, &mut quota, &mut work);
            self.record_grid_work(work)?;
            let ready = ready?;
            let used = allowed - quota.metadata_units;
            parts -= used;
            records = quota.records;
            self.target_metric_parts = self
                .target_metric_parts
                .checked_add(used)
                .ok_or(Error::CounterExhausted)?;
            self.target_index_records = self
                .target_index_records
                .checked_add(used * 7)
                .ok_or(Error::CounterExhausted)?;
            ticket.require(owner, &self.source)?;
            if !ready {
                target.metrics = Some(metrics);
                owner.schedule()?;
                return Ok(TargetAdvance::Handled);
            }
            let height = metrics.height()?;
            self.target_walk_lines = self
                .target_walk_lines
                .checked_add(1)
                .ok_or(Error::CounterExhausted)?;
            let (top, bottom) = if backwards {
                (target.boundary - height, target.boundary)
            } else {
                (target.boundary, target.boundary + height)
            };
            if !top.is_finite() || top < 0.0 || !bottom.is_finite() || bottom <= top {
                return Err(ViewError::InvalidGeometry.into());
            }
            let anchor = MeasuredLineAnchor {
                ordinal,
                top,
                bottom,
            };
            let reached = !page
                || if backwards {
                    desired_y >= top || ordinal == 0
                } else {
                    desired_y < bottom || self.complete && ordinal + 1 == self.lines
                };
            if reached {
                target.anchor = Some(anchor);
                break;
            }
            target.previous = anchor;
            target.boundary = if backwards { top } else { bottom };
            target.ordinal = if backwards {
                ordinal.checked_sub(1)
            } else {
                ordinal.checked_add(1)
            };
        }
        let anchor = target.anchor.as_ref().ok_or(Error::Closed)?;
        target
            .request
            .resolved
            .set(Some((anchor.ordinal, anchor.top, anchor.bottom)));
        self.last_target_anchor = Some(anchor.clone());
        ticket.require(owner, &self.source)?;
        let viewport = owner.viewport.borrow().clone().ok_or(Error::Closed)?;
        if viewport.geometry()?.content_width != self.maximum_content_width {
            owner.native_preparation_geometry(&ticket.request, &viewport, || {
                viewport.set_content_width(self.maximum_content_width)
            })?;
            owner.schedule()?;
            return Ok(TargetAdvance::Handled); // 先归还旧横向范围，原目标在后继真实geometry继续。
        }
        if viewport.geometry()?.content_height != self.height() {
            owner.native_preparation_geometry(&ticket.request, &viewport, || {
                viewport.set_content_height(self.height())
            })?;
            owner.schedule()?;
            return Ok(TargetAdvance::Handled); // 自有高度更新撤旧geometry，下一真实请求再滚动。
        }
        let rect = NSRect::new(
            NSPoint::new(
                ticket.request.virtual_viewport.layout_origin_x + self.padding,
                anchor.top,
            ),
            NSSize::new(1.0, anchor.bottom - anchor.top),
        );
        let revealed = owner.native_preparation_geometry(&ticket.request, &viewport, || {
            viewport.reveal_document_rect(rect, &|| target.request.current_native(owner))
        });
        self.target_source_health()?; // Native或current重入的普通撤权不得掩盖同一Source首错。
        if !target.request.current_native(owner) {
            return Err(Error::PreparationRevoked);
        }
        if revealed? {
            owner.schedule()?;
            return Ok(TargetAdvance::Handled);
        }
        ticket.require(owner, &self.source)?;
        Ok(TargetAdvance::VisibleBand)
    }

    pub(super) fn advance_target(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        ticket: &PreparationTicket,
    ) -> Result<TargetAdvance> {
        let pending = owner
            .source_target
            .borrow()
            .clone()
            .filter(|target| target.pending());
        let Some(pending) = pending else {
            self.target = None;
            return Ok(TargetAdvance::VisibleBand);
        };
        if !pending.matches(owner, request) || pending.source != self.source.authority() {
            pending.revoke();
            self.target = None;
            return Ok(TargetAdvance::VisibleBand);
        }
        if !self
            .target
            .as_ref()
            .is_some_and(|target| Rc::ptr_eq(&target.request, &pending))
        {
            self.target = Some(ActualTarget::new(pending));
        }
        // 先守原 Source/输入票，再借原槽判断真实前缀；尚未覆盖时没有查询要推进，
        // 不为每个 Composer 回合搬出并放回整份目标。失权仍撤原请求，源首错仍先返回。
        ticket.require_pure(owner, &self.source)?;
        let target = self.target.as_ref().ok_or(Error::Closed)?;
        if !target.request.current(owner) {
            target.request.revoke();
            return Ok(TargetAdvance::VisibleBand);
        }
        if target.anchor.is_none() {
            let at_eof = target.request.range.location == self.source.utf16_len();
            if self.output.is_none() || (at_eof && !self.complete) {
                return Ok(TargetAdvance::NeedPrefix);
            }
            let covered = self
                .output
                .as_ref()
                .ok_or(Error::Closed)?
                .borrow()
                .source_range()
                .end;
            if target.request.first_end > covered
                || (!at_eof && target.request.range.location >= covered)
            {
                if self.complete {
                    return Err(Error::Unsupported);
                }
                return Ok(TargetAdvance::NeedPrefix);
            }
        }
        // 以上只有纯票与同 Output 的范围观察，借用已结束；真正目录/Native 工作才取槽。
        let mut target = self.target.take().ok_or(Error::Closed)?;
        let result = self.advance_actual_target(owner, ticket, &mut target);
        self.target = Some(target);
        result
    }

    /// 唯一调用处已核原纯票、请求和未定锚点的前缀覆盖；取槽后不再重复这些检查。
    /// 后续目录读取、Native reveal 及各自前后的当前性复核仍由原消费路径负责。
    fn advance_actual_target(
        &mut self,
        owner: &Owner,
        ticket: &PreparationTicket,
        target: &mut ActualTarget,
    ) -> Result<TargetAdvance> {
        let at_eof = target.request.range.location == self.source.utf16_len();
        // 同一量子最多256个真实64B目录读取；每part包含Line认证2条、Part2条和Directory3条，
        // previous_line也包含原行认证与前驱两条。不能把API调用次数冒充真实record数。
        let mut records_left = 256usize;
        if target.anchor.is_none() {
            let output = self.output.as_ref().cloned().ok_or(Error::Closed)?;
            if target.ordinal.is_none() {
                if target.lookup.is_none() {
                    let upstream = if target.request.range.length == 0 {
                        let current = |epoch| owner.current(epoch) && target.request.current(owner);
                        let permit = owner
                            .input
                            .capture(owner.configuration.epoch, &current)
                            .map_err(ViewError::Input)?;
                        let state = owner.input.state().map_err(ViewError::Input)?;
                        let (upstream, _) = owner
                            .input
                            .navigation_selection_facts(&permit, &current)
                            .map_err(ViewError::Input)?;
                        state.selection.head_utf16 == target.request.range.location
                            && upstream == Some(true)
                    } else {
                        false
                    }; // 普通Find非空首scalar固定Downstream；零caret才能消费同一Input真实换行侧。
                    target.lookup = Some(output.borrow_mut().line_lookup(
                        MarkdownComposedLineLookupTarget::Source {
                            utf16: target.request.range.location,
                            // 真EOF与caret共用唯一文档末侧；若末尾有CRLF，必须滚入
                            // 原尾空行，不能把前行的同整数end当作其可见锚点。
                            affinity: if upstream && !at_eof {
                                MarkdownComposedLineAffinity::Upstream
                            } else {
                                // 非EOF仍沿原Input的换行侧；EOF下游查询返回真实last行，
                                // 使有限逆扫窗口包含随后caret/行号必须消费的同一ordinal。
                                MarkdownComposedLineAffinity::Downstream
                            },
                        },
                    )?);
                }
                let step = output.borrow_mut().step_line_lookup(
                    target.lookup.as_mut().ok_or(Error::Closed)?,
                    METADATA_PER_TURN,
                )?;
                self.target_lookup_records = self
                    .target_lookup_records
                    .checked_add(step.records)
                    .ok_or(Error::CounterExhausted)?;
                self.target_index_records = self
                    .target_index_records
                    .checked_add(step.records)
                    .ok_or(Error::CounterExhausted)?;
                records_left -= step.records;
                match step.answer {
                    MarkdownComposedLineLookupAnswer::Pending => {
                        owner.schedule()?;
                        return Ok(TargetAdvance::Handled);
                    }
                    MarkdownComposedLineLookupAnswer::Found(line) => {
                        target.ordinal = Some(line.facts().ordinal);
                        target.lookup = None;
                    }
                    MarkdownComposedLineLookupAnswer::PrefixNotCovered => {
                        return Err(Error::Unsupported);
                    }
                }
            }
            let ordinal = target.ordinal.ok_or(Error::Closed)?;
            if target.walk.is_none() {
                // 选择实际已度量且位于目标之后的最近锚；所有锚只活在原Source/宽度Producer中。
                let mut seed = BottomSeed {
                    ordinal: self.lines.checked_sub(1).ok_or(Error::Unsupported)?,
                    bottom: self.height + self.padding,
                };
                if let Some(top) = &self.top
                    && top.last_ordinal >= ordinal
                    && top.last_ordinal < seed.ordinal
                {
                    seed = BottomSeed {
                        ordinal: top.last_ordinal,
                        bottom: top.height + self.padding,
                    };
                }
                if let Some(last) = &self.last_target_anchor
                    && last.ordinal >= ordinal
                    && last.ordinal < seed.ordinal
                {
                    seed = BottomSeed {
                        ordinal: last.ordinal,
                        bottom: last.bottom,
                    };
                }
                if target.seed.is_none() {
                    target.seed =
                        Some(output.borrow_mut().line_lookup(
                            MarkdownComposedLineLookupTarget::Ordinal(seed.ordinal),
                        )?);
                }
                let step = output.borrow_mut().step_line_lookup(
                    target.seed.as_mut().ok_or(Error::Closed)?,
                    METADATA_PER_TURN,
                )?;
                self.target_lookup_records = self
                    .target_lookup_records
                    .checked_add(step.records)
                    .ok_or(Error::CounterExhausted)?;
                self.target_index_records = self
                    .target_index_records
                    .checked_add(step.records)
                    .ok_or(Error::CounterExhausted)?;
                records_left -= step.records;
                match step.answer {
                    MarkdownComposedLineLookupAnswer::Pending => {
                        owner.schedule()?;
                        return Ok(TargetAdvance::Handled);
                    }
                    MarkdownComposedLineLookupAnswer::Found(line) => {
                        if line.facts().ordinal != seed.ordinal {
                            return Err(Error::Unsupported);
                        }
                        target.walk = Some(Walk {
                            cursor: Some(line),
                            metrics: None,
                            bottom: seed.bottom,
                        });
                        target.seed = None;
                    }
                    MarkdownComposedLineLookupAnswer::PrefixNotCovered => {
                        return Err(Error::Unsupported);
                    }
                }
            }
            let walk = target.walk.as_mut().ok_or(Error::Closed)?;
            let mut parts = METADATA_PER_TURN;
            for _ in 0..METADATA_PER_TURN {
                if walk.metrics.is_none() {
                    walk.metrics = Some(LineMetrics::new(
                        walk.cursor.take().ok_or(Error::Unsupported)?,
                        owner.presentation_mode(),
                        false,
                    ));
                }
                let mut metrics = walk.metrics.take().ok_or(Error::Closed)?;
                ticket.require(owner, &self.source)?;
                let before = metrics.part;
                let allowed = parts.min(records_left / 7);
                let mut part_quota = MarkdownLineComposeBudget {
                    metadata_units: allowed,
                    records: records_left,
                    ..LINE_METRIC_BUDGET
                };
                let mut work = grid::Work::default();
                let ready = metrics.advance(&output, owner, &mut part_quota, &mut work);
                self.record_grid_work(work)?;
                let ready = ready?;
                let used = allowed - part_quota.metadata_units;
                parts -= used;
                records_left = part_quota.records;
                self.target_index_records = self
                    .target_index_records
                    .checked_add(used * 7)
                    .ok_or(Error::CounterExhausted)?;
                self.target_metric_parts = self
                    .target_metric_parts
                    .checked_add(metrics.part - before)
                    .ok_or(Error::CounterExhausted)?;
                ticket.require(owner, &self.source)?;
                if !ready {
                    walk.metrics = Some(metrics);
                    owner.schedule()?;
                    return Ok(TargetAdvance::Handled);
                }
                let top = walk.bottom - metrics.height()?;
                if !top.is_finite() || top < 0.0 {
                    return Err(ViewError::InvalidGeometry.into());
                }
                if metrics.handle.facts().ordinal == ordinal {
                    self.target_walk_lines = self
                        .target_walk_lines
                        .checked_add(1)
                        .ok_or(Error::CounterExhausted)?;
                    target.anchor = Some(MeasuredLineAnchor {
                        ordinal,
                        top,
                        bottom: walk.bottom,
                    });
                    target.walk = None;
                    break;
                }
                if metrics.handle.facts().ordinal < ordinal {
                    return Err(Error::Unsupported);
                }
                if records_left < 4 {
                    // 度量虽已完成，前驱认证仍需四条原record；保留完成游标，下一轮不重读part。
                    walk.metrics = Some(metrics);
                    owner.schedule()?;
                    return Ok(TargetAdvance::Handled);
                }
                walk.cursor = output.borrow_mut().previous_line(&metrics.handle)?;
                records_left -= 4;
                self.target_index_records = self
                    .target_index_records
                    .checked_add(4)
                    .ok_or(Error::CounterExhausted)?;
                self.target_walk_lines = self
                    .target_walk_lines
                    .checked_add(1)
                    .ok_or(Error::CounterExhausted)?;
                walk.bottom = top;
            }
            if target.anchor.is_none() {
                owner.schedule()?;
                return Ok(TargetAdvance::Handled);
            }
        }
        let anchor = target.anchor.as_ref().ok_or(Error::Closed)?;
        if owner
            .mode_viewport_required_bottom(
                &target.request,
                anchor.top,
                ticket.request.viewport.size.height,
            )
            .is_some_and(|bottom| self.needs_document_prefix(bottom))
        {
            return Ok(TargetAdvance::NeedPrefix); // 同一模式目标只补实际下方可见区，不用临时短前缀的页底约束认证原Y。
        }
        self.last_target_anchor = Some(anchor.clone());
        ticket.require(owner, &self.source)?;
        let viewport = owner.viewport.borrow().clone().ok_or(Error::Closed)?;
        if viewport.geometry()?.content_width != self.maximum_content_width {
            owner.native_preparation_geometry(&ticket.request, &viewport, || {
                viewport.set_content_width(self.maximum_content_width)
            })?;
            owner.schedule()?;
            return Ok(TargetAdvance::Handled); // 先归还旧横向范围，原目标在后继真实geometry继续。
        }
        if viewport.geometry()?.content_height != self.height() {
            owner.native_preparation_geometry(&ticket.request, &viewport, || {
                viewport.set_content_height(self.height())
            })?;
            owner.schedule()?;
            return Ok(TargetAdvance::Handled); // 自有高度改变后不捕后继request给旧准备票补签。
        }
        // 原模式已经从真实 Scene 测得格内字形时，由模式恢复它的屏内位置。
        // 整条表格行可能比首字形更高；再次最小滚入整行会与底边文字锚反复争夺滚动。
        // 这里只继续当前视口发布，模式仍须在新 Scene 上取得实际字形与可见证明才完成。
        if owner.mode_retains_measured_glyph(&target.request) {
            ticket.require(owner, &self.source)?;
            return Ok(TargetAdvance::VisibleBand);
        }
        // 只有真实文首位置和原ordinal0同时成立才涵盖已有顶部留白；PageUp提交head0后
        // 与DocStart共用此处，不得把首行top=padding的最小滚入误当文档已经返回y0。
        let top = if target.request.range.location == 0 && anchor.ordinal == 0 {
            0.0
        } else {
            anchor.top // 其他目标仍只滚入其真实行，不扩读前文或按估计行高调整页面。
        };
        let rect = NSRect::new(
            NSPoint::new(
                ticket.request.virtual_viewport.layout_origin_x + self.padding,
                top,
            ),
            NSSize::new(1.0, anchor.bottom - top),
        );
        let revealed = owner.native_preparation_geometry(&ticket.request, &viewport, || {
            viewport.reveal_document_rect(rect, &|| target.request.current_native(owner))
        });
        self.source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        let cancelled = owner.cancel_stale_find_target(&target.request)?;
        self.target_source_health()?;
        if cancelled {
            if let Err(cause) = revealed {
                let cause = Error::Viewport(cause);
                if !cause.is_foreground_revocation() {
                    return Err(cause); // 真Native几何错误也不能被同时发生的查询取消吞掉。
                }
            }
            ticket.require_pure(owner, &self.source)?;
            owner.schedule()?;
            return Ok(TargetAdvance::Handled); // 自有scroll可能已改变几何；只撤Find，下一回合捕实际请求继续原Source。
        }
        let changed = revealed?;
        let current = target.request.current_native(owner);
        self.target_source_health()?;
        if !current {
            target.request.revoke();
            return Err(Error::PreparationRevoked);
        }
        if changed {
            owner.schedule()?;
            return Ok(TargetAdvance::Handled);
        }
        ticket.require(owner, &self.source)?;
        Ok(TargetAdvance::VisibleBand)
    }
}
