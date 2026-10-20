//! 格内 Find 沿真实显示映射和原行目录定位；原 row 高框不能替代目标首标量的实际行。

use super::super::super::target::SourceTargetRequest;
use super::*;
use omni_markdown_core::MarkdownComposedLineAffinity;

/// 原 Find 意图只固定同一请求和下一待认证列，不持另一 Source、输入图或正文副本。
pub(super) struct RowTarget {
    pub(super) request: Rc<SourceTargetRequest>,
    column: usize,
}

/// 显示端点只由真实 canonical→display 映射取得；锚点来自已认证 LineEnd 的实际字体高度。
pub(super) struct CellTarget {
    position: usize,
    first_end: usize,
    anchor: Option<CellTargetAnchor>,
    walk: Option<TargetWalk>,
}

/// 只有原LineEnd和首GCB两端实际度量后才存在，不按列均宽或字符数估计横向位置。
#[derive(Clone, Copy)]
struct CellTargetAnchor {
    top: f64,
    bottom: f64,
    left: f64,
    right: f64,
}

struct TargetWalk {
    // 游标借当前输出目录；目标请求本身不能延长已经替换的 Reader 或 Native 格窗口生命。
    cursor: Option<MarkdownComposedLine>,
    metrics: Option<LineMetrics>,
    // 从真实已测末行向前累减，不能按平均字号估计被跳过行的高度。
    bottom: f64,
    /// 找到目标行后沿同一handle续扫part，不重复逆扫前文，也不保存整行part数组。
    target_top: Option<f64>,
    target_part: usize,
    // 原目录签出的首个完整 GCB；跨真实 UBA 项时仅保有限包络，不保任何 part 或字体强边。
    grapheme: std::ops::Range<usize>,
    left: Option<f64>,
    right: Option<f64>,
    covered: usize,
}

impl CellTarget {
    /// 后继 prefix 换真实 Reader 后，原弱游标失效；已经实测的同 Source 行锚仍可保留。
    pub(super) fn reset_scan(&mut self) {
        self.walk = None;
    }
}

impl Grid {
    /// 原意图在此行内才接管 Cell 字形生产，行外仍交同一个正文 Producer 顺序推进。
    pub(super) fn advance_source_target(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        scene: &NativeMarkdownPagedSceneRequest,
        semantics: &mut super::super::super::semantics::RootLeaf,
    ) -> Result<Option<Advance>> {
        // 先核原请求、Scene 和 canonical 来源；已取消或落在别行的意图不驱动本格读取。
        let pending = owner.source_target.borrow().clone().filter(|target| {
            target.pending()
                && target.source == source.authority()
                && target.matches(owner, scene)
                && self.source_range().contains(&target.range.location)
        });
        let Some(pending) = pending else {
            // 丢弃旧目标锚，但保留同一视口仍需使用的实际格布局。
            self.target = None;
            return Ok(None);
        };
        if !self
            .target
            .as_ref()
            .is_some_and(|target| Rc::ptr_eq(&target.request, &pending))
        {
            // 新请求不能复用旧 Find 的显示端点，哪怕两个 canonical 数值恰好相同。
            for cell in &mut self.cells {
                cell.target = None;
            }
            self.target = Some(RowTarget {
                request: pending,
                column: 0,
            });
        }
        // 暂取唯一目标以免同时借用整行；出错也放回原请求，外层负责记录真实失败。
        let mut target = self.target.take().expect("真实格目标已装配");
        let result = self.advance_cell_target(owner, source, scene, semantics, &mut target);
        self.target = Some(target);
        result
    }

    /// 每次只推进一列的真实准备或一轮目录；未认证的后缀不被误判为格 EOF。
    fn advance_cell_target(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        scene: &NativeMarkdownPagedSceneRequest,
        semantics: &mut super::super::super::semantics::RootLeaf,
        target: &mut RowTarget,
    ) -> Result<Option<Advance>> {
        // 只走本行有限列 metadata；未知格先交原串行准备量子，不抢先读取整行内容。
        for _ in 0..self.cells.len() {
            let column = target.column;
            let cell = self.cells.get_mut(column).ok_or(Error::Unsupported)?;
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("Find格沿原Body文档")
                .clone();
            let range = {
                let document = document.borrow();
                // 空格也必须来自真实 raw 票；尚未完成准备的列不能用猜测范围参与命中。
                if let Some(empty) = &cell.empty {
                    Some(empty.canonical_range_for(&document)?.clone())
                } else if let Some(display) = &cell.display {
                    Some(
                        display
                            .canonical_raw_cell()
                            .canonical_range_for(&document)?
                            .clone(),
                    )
                } else {
                    None
                }
            };
            let Some(range) = range else {
                // 原 CellInline/显示转换沿同一个 RootLeaf Request 推进；本次不创建第二 Parser。
                return cell
                    .advance(owner, source, &self.row, false, semantics, &mut self.work)
                    .map(Some);
            };
            let canonical = target.request.range.location;
            let belongs = range.contains(&canonical)
                || target.request.range.length == 0 && canonical == range.end;
            if !belongs {
                // 格间管道和分隔行不属于显示正文；仅前进列游标，不夹到邻格伪造命中。
                target.column += 1;
                continue;
            }
            let anchor = if let Some(display) = &cell.display {
                if cell.target.is_none() {
                    // 两端都经原显示映射认证；语法隐藏产生的空洞不能直接当可见 UTF-16 偏移。
                    let position = display
                        .canonical_to_display(canonical, MarkdownComposedLineAffinity::Downstream)
                        .map_err(ViewError::Document)?
                        .ok_or(Error::Unsupported)?;
                    let first_end = display
                        .canonical_to_display(
                            target.request.first_end,
                            MarkdownComposedLineAffinity::Downstream,
                        )
                        .map_err(ViewError::Document)?
                        .ok_or(Error::Unsupported)?;
                    cell.target = Some(CellTarget {
                        position,
                        first_end,
                        anchor: None,
                        walk: None,
                    });
                }
                if cell.pending.is_some() {
                    // LineEnd 已封存但字体实际高度仍未完成，必须先度量再查目标几何。
                    return cell
                        .advance(owner, source, &self.row, false, semantics, &mut self.work)
                        .map(Some);
                }
                if !cell.find_target_anchor(owner, source, &target.request, &mut self.work)? {
                    // 只比较真实 prefix 覆盖；尚未覆盖目标就继续 Composer，而非宣告 Source EOF。
                    let covered = cell
                        .output
                        .as_ref()
                        .map(|output| output.borrow().source_range().end)
                        .unwrap_or(0);
                    let display_target = cell.target.as_ref().expect("真实显示目标已绑定");
                    if display_target.first_end > covered
                        || display_target.position >= covered && !cell.complete
                    {
                        return cell
                            .advance(owner, source, &self.row, false, semantics, &mut self.work)
                            .map(Some);
                    }
                    // 已覆盖但逆扫预算耗尽，保留原游标留待下一轮，不重复全格遍历。
                    return Ok(Some(Advance::Progress));
                }
                cell.target
                    .as_ref()
                    .expect("原格目标仍存活")
                    .anchor
                    .expect("真实锚点已经完成")
            } else {
                CellTargetAnchor {
                    top: 0.0,
                    bottom: MINIMUM_HEIGHT - VERTICAL * 2.0,
                    left: 0.0,
                    right: self.widths[column] - HORIZONTAL * 2.0,
                } // 真空格没有字形，目标只有原Cell最小内容框。
            };
            // 模式已由同 Scene 的实际字形接管后，不再用格目标的最小 reveal 覆盖屏内锚。
            // 返回普通格准备路径，不能不断返回同一个 Target 并饿死其余可见列及最终发布。
            if owner.mode_retains_measured_glyph(&target.request) {
                return Ok(None);
            }
            let top = self.top + VERTICAL + anchor.top;
            let bottom = self.top + VERTICAL + anchor.bottom;
            // 列前缀与首GCB真实caret相加，窄视口和RTL也只滚到实际目标，不要求整列同时可见。
            let column_x = scene.virtual_viewport.layout_origin_x
                + owner.configuration.padding
                + self.widths[..column].iter().sum::<f64>()
                + HORIZONTAL;
            let x = column_x + anchor.left;
            let content_width = anchor.right - anchor.left; // 零advance保留真实caret，不制造1pt字符框。
            let visible = NSRect::new(
                NSPoint::new(
                    scene.virtual_viewport.document_origin.x + scene.viewport.origin.x,
                    scene.virtual_viewport.document_origin.y,
                ),
                scene.viewport.size,
            );
            // 单个真实GCB或字体行本身也可大于视口；只要求其真实首部band进入当前clip，
            // 否则完整框永远放不下会反复reveal。普通目标仍须完整进入原可见矩形。
            let horizontal_visible = if content_width > visible.size.width {
                x <= visible.origin.x && x + content_width >= visible.origin.x + visible.size.width
            } else {
                x >= visible.origin.x && x + content_width <= visible.origin.x + visible.size.width
            };
            let vertical_visible = if bottom - top > visible.size.height {
                top <= visible.origin.y && bottom >= visible.origin.y + visible.size.height
            } else {
                top >= visible.origin.y && bottom <= visible.origin.y + visible.size.height
            };
            if !horizontal_visible || !vertical_visible {
                // 真实列内容框也交原二维reveal；只滚Y会把屏外最右列误报成可见。

                return Ok(Some(Advance::Target {
                    request: target.request.clone(),
                    rect: NSRect::new(
                        NSPoint::new(x, top),
                        NSSize::new(content_width, bottom - top),
                    ),
                }));
            }
            return Ok(None); // 原目标所在行已在真实 band；其他可见列仍须按同一视口完成再发布。
        }
        Err(Error::Unsupported) // 表格管道和delimiter不属于可见格，不能夹到相邻正文伪造目标框。
    }
}

impl Cell {
    /// 真实首标量尚在前缀外时只报告未覆盖；已有目录内的查找按固定part余量续原游标。
    fn find_target_anchor(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &SourceTargetRequest,
        work: &mut Work,
    ) -> Result<bool> {
        use crate::native_surface::paged_line_geometry::{
            ComposedPartSourceEdge, composed_part_caret, composed_part_extent_edge,
        };
        use omni_markdown_core::{MarkdownGraphemeAnswer, MarkdownGraphemeQuery};

        let target = self.target.as_mut().expect("本格已绑定原Find端点");
        if target.anchor.is_some() {
            return Ok(true);
        }
        let Some(output) = &self.output else {
            // 尚无完整 LineEnd 的格还没有可签字形，不能从 Source 长度合成首行。
            return Ok(false);
        };
        let display = self.display.as_ref().expect("目标持同一真实格Source");
        let alignment = {
            let original = owner
                .document
                .borrow()
                .as_ref()
                .expect("原格目标持正文身份")
                .clone();
            let document = original.borrow();
            display.canonical_raw_cell().alignment_for(&document)?
        }; // 真实 Doc 租约在任何 Native 字形/caret getter 前归还。
        let covered = output.borrow().source_range().end;
        if target.first_end > covered || target.position >= covered && !self.complete {
            return Ok(false);
        }
        if target.walk.is_none() {
            let grapheme = {
                let mut reader = output.borrow_mut();
                let start = if reader.grapheme_ordinal(target.position)?.is_some() {
                    target.position
                } else {
                    if target.position == target.first_end {
                        return Err(ViewError::InvalidRange.into());
                    } // 零范围是原插入位置，不能把内部 scalar 默默移到前一 GCB。
                    match reader.grapheme_query(target.position, MarkdownGraphemeQuery::Previous)? {
                        MarkdownGraphemeAnswer::Known(Some(boundary)) => boundary.utf16,
                        _ => return Err(ViewError::InvalidRange.into()),
                    }
                };
                let end = if target.position == target.first_end {
                    start // 空目标保原真实 caret；非空目标才消费下一个完整 GCB。
                } else {
                    match reader.grapheme_query(start, MarkdownGraphemeQuery::Next)? {
                        MarkdownGraphemeAnswer::Known(Some(boundary)) => boundary.utf16,
                        _ => return Err(ViewError::InvalidRange.into()),
                    }
                };
                start..end
            };
            // Reader 只提供分页目录句柄；不构造整格行Vec或读取整格字符串。
            target.walk = Some(TargetWalk {
                cursor: output.borrow_mut().last_line()?,
                metrics: None,
                bottom: self.height,
                target_top: None,
                target_part: 0,
                grapheme,
                left: None,
                right: None,
                covered: 0,
            });
        }
        let walk = target.walk.as_mut().expect("原目标逆扫已创建");
        let mut budget = LINE_METRIC_BUDGET;
        for _ in 0..METADATA_PER_TURN {
            if budget.metadata_units == 0 || budget.records < 7 {
                return Ok(false); // 行度量和目标part扫描共用原余量，不能让一个复杂格占满主线程。
            }
            if walk.target_top.is_none() {
                if walk.metrics.is_none() {
                    let handle = walk.cursor.take().ok_or(Error::Unsupported)?;
                    walk.metrics = Some(LineMetrics::new(
                        handle,
                        omni_markdown_core::MarkdownPresentationMode::InlinePreview,
                        alignment != omni_markdown_core::MarkdownTableAlignment::Left,
                    ));
                }
                let mut metrics = walk.metrics.take().expect("原目标持本行度量");
                if !metrics.advance(output, owner, &mut budget, work)? {
                    walk.metrics = Some(metrics);
                    return Ok(false); // 未完成的混合字体行不能进入累计高度。
                }
                let top = walk.bottom - metrics.height()?;
                let range = &metrics.handle.facts().source_utf16;
                let at_eof = self.complete && target.position == display.source().utf16_len();
                if !(range.contains(&target.position) || at_eof && target.position == range.end)
                    || target.first_end > range.end
                {
                    // 本次只跨已实际度量的原目录行；未知prefix末端从不当作整格EOF。
                    walk.cursor = output.borrow_mut().previous_line(&metrics.handle)?;
                    walk.bottom = top;
                    continue;
                }
                walk.target_top = Some(top);
                walk.metrics = Some(metrics);
            }
            let metrics = walk.metrics.as_ref().expect("已定位行保原完整度量");
            while walk.target_part < metrics.handle.facts().parts
                && budget.metadata_units != 0
                && budget.records >= 7
            {
                let ordinal = walk.target_part;
                let part = output
                    .borrow_mut()
                    .part(&metrics.handle, ordinal)?
                    .ok_or(Error::Unsupported)?;
                walk.target_part += 1;
                budget.metadata_units -= 1;
                budget.records -= 7;
                let range = &part.facts().source_utf16;
                let overlap =
                    walk.grapheme.start.max(range.start)..walk.grapheme.end.min(range.end);
                let caret = walk.grapheme.is_empty()
                    && (range.contains(&walk.grapheme.start)
                        || walk.grapheme.start == range.end
                            && walk.target_part == metrics.handle.facts().parts);
                if !caret && overlap.start >= overlap.end {
                    continue; // 只合并原首 GCB 的实际片段；同行其余字形不进入目标包络。
                }
                let authority = part.font_registration().authority()?;
                let palette = owner.fonts.borrow().clone();
                let font = palette
                    .iter()
                    .find(|font| font.native().core_font().permits(&authority))
                    .ok_or(Error::Unsupported)?;
                let content_width = native_markdown_table_content_width(self.width)
                    .ok_or(ViewError::InvalidGeometry)?;
                let baseline =
                    NSPoint::new(metrics.aligned_origin_x(alignment, content_width)?, 0.0);
                let current = || request.current_native(owner);
                let position = |at| {
                    let edge = if !caret && at == range.start {
                        Some(ComposedPartSourceEdge::Start)
                    } else if !caret && at == range.end {
                        Some(ComposedPartSourceEdge::End)
                    } else {
                        None
                    };
                    if let Some(edge) = edge {
                        composed_part_extent_edge(output, &part, font, edge, baseline, &current)
                    } else {
                        // 真正的插入位置仍经过原 GCB/cluster；内部 seam 仅能通过上方认证绘制边缘使用。
                        composed_part_caret(output, &part, font, at, baseline, &current)
                            .map(|caret| caret.baseline)
                    }
                };
                let left = position(if caret {
                    walk.grapheme.start
                } else {
                    overlap.start
                });
                output.borrow().require(&display.authority())?;
                source
                    .byte_offset_for_utf16(0)
                    .map_err(ViewError::Document)?;
                let left = left.map_err(|cause| scene_error(
                    crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError::Geometry(cause),
                ))?;
                let right = position(if caret {
                    walk.grapheme.end
                } else {
                    overlap.end
                });
                // GDEF/Native回调可能撤权或使Source首次失败；先归还Reader租约再核typed首错。
                output.borrow().require(&display.authority())?;
                source
                    .byte_offset_for_utf16(0)
                    .map_err(ViewError::Document)?;
                let right = right.map_err(|cause| scene_error(
                    crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError::Geometry(cause),
                ))?;
                let left_edge = left.x.min(right.x);
                let right_edge = left.x.max(right.x);
                walk.left = Some(walk.left.map_or(left_edge, |old| old.min(left_edge)));
                walk.right = Some(walk.right.map_or(right_edge, |old| old.max(right_edge)));
                walk.covered = walk
                    .covered
                    .checked_add(if caret { 0 } else { overlap.len() })
                    .ok_or(ViewError::InvalidRange)?;
                if caret || walk.covered == walk.grapheme.len() {
                    // 已认证的所有片段都来自同一完整行、原字体和原 GCB；只在实际覆盖完整后签目标。
                    target.anchor = Some(CellTargetAnchor {
                        top: walk.target_top.expect("目标行顶来自真实行高"),
                        bottom: walk.bottom,
                        left: walk.left.expect("实际目标已贡献左边缘"),
                        right: walk.right.expect("实际目标已贡献右边缘"),
                    });
                    target.walk = None;
                    return Ok(true);
                }
            }
            if walk.target_part == metrics.handle.facts().parts {
                return Err(ViewError::InvalidRange.into()); // 原Line无此目标part，不能夹到相邻格。
            }
        }
        Ok(false)
    }
}
