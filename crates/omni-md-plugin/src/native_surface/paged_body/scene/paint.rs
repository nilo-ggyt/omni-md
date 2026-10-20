//! 同一CPU候选串行消费真实正文与可见格；所有实际游标完成后才交付整场景完成票。

use super::*;
use crate::native_surface::paged_line_viewport::{
    NativeMarkdownComposedViewportDecorationCursor, NativeMarkdownComposedViewportPaintWork,
};

/// 真实光栅批次的位置，不保事件或正文；取消直接析构当前游标和原context claim。
#[derive(Clone, Copy)]
enum Cursor {
    // 背景/边线在任何字形前完成，逐格暂停不会改变原RGBA覆盖顺序。
    GridRow(usize),
    GridBackground { row: usize, cell: usize },
    GridBorderStart(usize),
    GridBorder { row: usize, cell: usize },
    GridBorderEnd(usize),
    // 正文背景、活动行、正文选区也逐原窗口项推进，随后才轮到Grid选区与全部字形。
    TextBackground,
    ActiveLine(usize),
    // Find 在普通选区下层；复用同一个canonical→display装饰游标，不能另估文字矩形。
    TextFind,
    GridFind { row: usize, cell: usize },
    TextSelection,
    GridSelection { row: usize, cell: usize },
    Text,
    Cell { row: usize, cell: usize },
    Done,
}

pub(super) struct Paint {
    scene: Rc<Scene>,
    context: CFRetained<CGContext>,
    cursor: Cursor,
    session: Option<NativeMarkdownComposedViewportPaintSession>,
    // 正文与格串行共用一个纯数值装饰游标，完成后归还，不累积每格context claim。
    decoration: Option<NativeMarkdownComposedViewportDecorationCursor>,
    /// 同一行未stroke前持有原context的一层gstate，取消必须归还，不能污染后继候选。
    border_open: bool,
    total: NativeMarkdownPagedScenePaintWork,
}

impl Drop for Paint {
    /// 取消可能发生在任一列边线之间，context仍由self强持有，归还原gstate后再析构claim。
    fn drop(&mut self) {
        if self.border_open {
            CGContext::restore_g_state(Some(&self.context));
            CGContext::begin_path(Some(&self.context)); // 本候选未提交的原行路径不能流到后继使用者。
        }
    }
}

impl Paint {
    /// 构造不发起绘图；第一个实际量子依原预算进入同一个Text或Cell窗口。
    pub(super) fn new(scene: Rc<Scene>, context: CFRetained<CGContext>) -> Self {
        // 无Grid也必须先逐项完成正文装饰，不能在begin_cpu_paint同步扫完窗口再只计字形。
        let cursor = if scene.grids.is_empty() {
            Cursor::TextBackground
        } else {
            Cursor::GridRow(0)
        };
        Self {
            scene,
            context,
            cursor,
            session: None,
            decoration: None,
            border_open: false,
            total: NativeMarkdownPagedScenePaintWork::default(),
        }
    }

    /// 内层真实工作只合计一次，跨窗口共用外层量子，不能每格重领256字形配额。
    fn account(
        &mut self,
        work: &mut NativeMarkdownPagedScenePaintWork,
        actual: NativeMarkdownComposedViewportPaintWork,
    ) {
        work.glyphs += actual.glyphs;
        work.batches += actual.batches;
        work.metadata_records += actual.metadata_records;
        self.total.glyphs += actual.glyphs;
        self.total.batches += actual.batches;
        self.total.metadata_records += actual.metadata_records;
    }
    /// 每个结构项或装饰项只扣外层同一metadata余量；字形阶段开始前所有背景和选区都已真正完成。
    fn decorate(
        &mut self,
        work: &mut NativeMarkdownPagedScenePaintWork,
        budget: usize,
        current: &dyn Fn() -> bool,
    ) -> Result<bool> {
        let scene = self.scene.clone(); // 仅原Scene的本栈纯Rc租约，不创建context claim或新布局图。
        let permit = || current() && scene.current();
        let mut records = 1;
        match self.cursor {
            Cursor::GridRow(row) => {
                // 一行背景是固定一次矩形操作；后续格代码与列边线各自支付剩余预算。
                if row < scene.grids.len() {
                    scene.paint_grid_row_background(row, &self.context, &permit)?;
                    self.cursor = Cursor::GridBackground { row, cell: 0 };
                } else {
                    self.cursor = Cursor::TextBackground;
                }
            }
            Cursor::GridBackground { row, cell }
            | Cursor::GridSelection { row, cell }
            | Cursor::GridFind { row, cell } => {
                // 两个pass共享同一格顺序，但分别保原背景先于正文装饰、选区晚于正文装饰的顺序。
                // 行/格终点只是有限metadata迭代，不启动新的排版或Glyph会话。
                let finding = matches!(self.cursor, Cursor::GridFind { .. });
                let selection = finding || matches!(self.cursor, Cursor::GridSelection { .. });
                let ranges = if finding {
                    &scene.find_highlights
                } else {
                    &scene.selections
                };
                let fill = if finding {
                    [1.0, 0.75, 0.12, 0.25]
                } else {
                    scene.colors.selection
                };
                if row == scene.grids.len() {
                    self.cursor = if finding {
                        Cursor::TextSelection
                    } else {
                        Cursor::Text
                    };
                } else if cell == scene.grids[row].cells.len() {
                    self.cursor = if finding {
                        Cursor::GridFind {
                            row: row + 1,
                            cell: 0,
                        }
                    } else if selection {
                        Cursor::GridSelection {
                            row: row + 1,
                            cell: 0,
                        }
                    } else {
                        Cursor::GridBorderStart(row)
                    };
                } else {
                    let content = &scene.grids[row].cells[cell].content;
                    if let GridCellContent::Composed(window) = content {
                        // 进入格时只建立有限游标，下一实际步骤才访问line/part/selection。
                        // 显式领取与具体装饰工作分别收费，不能把整格成本折算成一次领取。
                        if self.decoration.is_none() {
                            self.decoration = Some(if selection {
                                window
                                    .selection_cursor(ranges, &permit)
                                    .map_err(scene_error)?
                            } else {
                                window.code_background_cursor()
                            });
                        } else {
                            let step = window
                                .advance_decorations(
                                    self.decoration.as_mut().expect("当前真实格持唯一装饰游标"),
                                    &self.context,
                                    ranges,
                                    fill,
                                    budget,
                                    &permit,
                                )
                                .map_err(paint_error)?;
                            // 原Viewport只用调用者本轮余量；不得为下一格重发完整32项预算。
                            records = step.metadata_records;
                            if !step.complete {
                                work.metadata_records += records;
                                self.total.metadata_records += records;
                                return Ok(true);
                            }
                            // 只有真实完成才移交下一格，没有截断末part或遗漏后续选区。
                            self.decoration = None;
                            self.cursor = if finding {
                                Cursor::GridFind {
                                    row,
                                    cell: cell + 1,
                                }
                            } else if selection {
                                Cursor::GridSelection {
                                    row,
                                    cell: cell + 1,
                                }
                            } else {
                                Cursor::GridBackground {
                                    row,
                                    cell: cell + 1,
                                }
                            };
                        }
                    } else {
                        self.cursor = if finding {
                            Cursor::GridFind {
                                row,
                                cell: cell + 1,
                            }
                        } else if selection {
                            Cursor::GridSelection {
                                row,
                                cell: cell + 1,
                            }
                        } else {
                            Cursor::GridBackground {
                                row,
                                cell: cell + 1,
                            }
                        };
                    }
                }
            }
            Cursor::GridBorderStart(row) => {
                // 全部代码背景已结束，才能持一条跨量子的CG路径，避免clip清掉路径。
                scene.begin_grid_border(row, &self.context, &permit)?;
                self.border_open = true;
                self.cursor = Cursor::GridBorder { row, cell: 0 };
            }
            Cursor::GridBorder { row, cell } => {
                // 每个可见格对应一条真实列边线，最大列数不再隐藏在一个同步循环内。
                if cell < scene.grids[row].cells.len() {
                    scene.append_grid_border(row, cell, &self.context, &permit)?;
                    self.cursor = Cursor::GridBorder {
                        row,
                        cell: cell + 1,
                    };
                } else {
                    self.cursor = Cursor::GridBorderEnd(row);
                }
            }
            Cursor::GridBorderEnd(row) => {
                // 所有线段在同一路径一次stroke，保持原接缝抗锯齿和边缘alpha。
                let result = scene.finish_grid_border(row, &self.context, &permit);
                // stroke失败/撤销也必须配平begin时的save；实际路径只属于这次未发布候选。
                CGContext::restore_g_state(Some(&self.context));
                self.border_open = false;
                if let Err(cause) = result {
                    // current path 不属于 gstate；源失效发生在 stroke 之前时也须归还未提交线段。
                    CGContext::begin_path(Some(&self.context));
                    return Err(cause);
                }
                self.cursor = Cursor::GridRow(row + 1);
            }
            Cursor::TextBackground | Cursor::TextSelection | Cursor::TextFind => {
                // 正文与格消费同一封存line/part几何和同一装饰游标，Body选区保持canonical坐标。
                // 创建游标与实际遍历分别付费；当前量子余量不会因进入正文而重置为32。
                let finding = matches!(self.cursor, Cursor::TextFind);
                let selection = finding || matches!(self.cursor, Cursor::TextSelection);
                let ranges = if finding {
                    &scene.find_highlights
                } else {
                    &scene.selections
                };
                let fill = if finding {
                    [1.0, 0.75, 0.12, 0.25]
                } else {
                    scene.colors.selection
                };
                let complete = if let Some(window) = &scene.window {
                    if let Some(decoration) = &mut self.decoration {
                        let step = window
                            .advance_decorations(
                                decoration,
                                &self.context,
                                ranges,
                                fill,
                                budget,
                                &permit,
                            )
                            .map_err(paint_error)?;
                        records = step.metadata_records;
                        step.complete
                    } else {
                        self.decoration = Some(if selection {
                            NativeMarkdownComposedViewportDecorationCursor::selections(
                                window.source_range(),
                                ranges,
                            )
                        } else {
                            NativeMarkdownComposedViewportDecorationCursor::backgrounds()
                        });
                        false
                    }
                } else {
                    true // Grid-only场景没有正文窗口，不创建空Source、字体或第二个绘图会话。
                };
                if complete {
                    self.decoration = None;
                    self.cursor = if finding {
                        Cursor::GridFind { row: 0, cell: 0 }
                    } else if selection {
                        scene.color(&self.context, scene.colors.text);
                        Cursor::GridSelection { row: 0, cell: 0 }
                    } else {
                        Cursor::ActiveLine(0)
                    };
                }
            }
            Cursor::ActiveLine(index) => {
                // 活动逻辑行只消费原窗口的一条真实软行；没有活动行时一次跳过，无需枚举全部ranges。
                // 不把行框包络当作已选择正文，也不在此读取、复制或重排任何文字。
                if let (Some(active), Some(range)) = (&scene.active_line, scene.ranges.get(index)) {
                    let window = scene.text_window()?;
                    window.require(&permit).map_err(scene_error)?;
                    let selected = if active.is_empty() {
                        range.is_empty() && range.start == active.start
                    } else {
                        range.start < active.end && active.start < range.end
                    };
                    if selected {
                        let frame = window.line_frame(index).ok_or(Error::InvalidGeometry)?;
                        let viewport = scene.key.request.viewport;
                        let bottom = frame.origin.y.max(viewport.origin.y);
                        let top = (frame.origin.y + frame.size.height)
                            .min(viewport.origin.y + viewport.size.height);
                        if bottom < top {
                            scene.color(&self.context, scene.colors.active_line);
                            CGContext::fill_rect(
                                Some(&self.context),
                                NSRect::new(
                                    NSPoint::new(frame.origin.x, bottom),
                                    NSSize::new(frame.size.width, top - bottom),
                                ),
                            );
                        }
                    }
                    window.require(&permit).map_err(scene_error)?;
                    self.cursor = Cursor::ActiveLine(index + 1);
                } else {
                    self.cursor = if scene.find_highlights.is_empty() {
                        Cursor::TextSelection
                    } else {
                        Cursor::TextFind
                    };
                }
            }
            Cursor::Text | Cursor::Cell { .. } | Cursor::Done => return Ok(false),
        }
        work.metadata_records += records;
        self.total.metadata_records += records;
        // 单项已由原窗口核其Source与目录；整场景字体/来源深核只在本量子的入口、出口执行。
        // 这里保原输入/几何取消，任何失败仍由step出口的真实Source检查决定首因。
        if !permit() {
            return Err(Error::Revoked);
        }
        Ok(true)
    }
    /// 推进只保本量子的真实工作和原游标；入口/出口全图检查由唯一step边界负责。
    fn advance(
        &mut self,
        budget: NativeMarkdownPagedScenePaintBudget,
        work: &mut NativeMarkdownPagedScenePaintWork,
        current: &dyn Fn() -> bool,
    ) -> Result<bool> {
        // 组合场景沿用原实际光栅入口的单量子上限；不能靠拆格绕过调用者预算合同。
        let maximum = NativeMarkdownComposedViewportPaintBudget::default();
        if budget.glyphs > maximum.glyphs || budget.metadata_records > maximum.metadata_records {
            return Err(Error::Raster(
                NativeMarkdownComposedViewportPaintCause::InvalidBudget.to_string(),
            ));
        }
        loop {
            // 同步量子内部只核原取消票，各真实窗口在实际读取/绘制时检查自己的资源。
            // 每条格边线不能重复扫描整个Grid和全部字体；出口仍无条件检查整场景。
            if !current() || !self.scene.current() {
                return Err(Error::Revoked);
            }
            if matches!(self.cursor, Cursor::Done) {
                self.scene.caret(&self.context, current)?;
                return Ok(true); // 只有全部真实装饰/字形和caret结束后才允许step签整帧完成票。
            }
            if work.metadata_records == budget.metadata_records || work.glyphs == budget.glyphs {
                return Ok(false);
            }
            // 背景、选区与字形共用本轮metadata余额，任何阶段用尽都保原游标暂停。
            let remaining_metadata = budget.metadata_records - work.metadata_records;
            if self.decorate(work, remaining_metadata, current)? {
                continue;
            }
            // 进入真实窗口/空格各计一次metadata，空格不创建字体、来源或字形会话。
            if self.session.is_none() {
                work.metadata_records += 1;
                self.total.metadata_records += 1;
                match self.cursor {
                    Cursor::Text => {
                        if let Some(window) = &self.scene.window {
                            self.session = Some(
                                window
                                    .clone()
                                    .begin_paint(self.context.clone(), &|| {
                                        current() && self.scene.current()
                                    })
                                    .map_err(paint_error)?,
                            );
                        } else {
                            self.cursor = Cursor::Cell { row: 0, cell: 0 };
                            continue;
                        }
                    }
                    Cursor::Cell { row, cell } => {
                        let Some(row_value) = self.scene.grids.get(row) else {
                            self.cursor = Cursor::Done;
                            continue;
                        };
                        let Some(cell_value) = row_value.cells.get(cell) else {
                            self.cursor = Cursor::Cell {
                                row: row + 1,
                                cell: 0,
                            };
                            continue;
                        };
                        if let GridCellContent::Composed(window) = &cell_value.content {
                            self.session = Some(
                                window
                                    .begin_paint(self.context.clone(), &|| {
                                        current() && self.scene.current()
                                    })
                                    .map_err(paint_error)?,
                            );
                        } else {
                            self.cursor = Cursor::Cell {
                                row,
                                cell: cell + 1,
                            };
                            continue;
                        }
                    }
                    _ => unreachable!("装饰和完成游标已由前置分派处理"),
                }
            }
            // 窗口领取也属于本轮 metadata；额度恰好用尽时保留原 session，下一轮接续。
            if work.metadata_records == budget.metadata_records {
                return Ok(false);
            }
            let session = self
                .session
                .take()
                .expect("composed window has one actual paint continuation");
            let step = session.step(
                NativeMarkdownComposedViewportPaintBudget {
                    glyphs: budget.glyphs - work.glyphs,
                    metadata_records: budget.metadata_records - work.metadata_records,
                },
                &|| current() && self.scene.current(),
            );
            let step = step.map_err(paint_error)?; // 实际失败消费原会话，step出口仍优先检查Source首错。
            self.account(work, step.work);
            match step.outcome {
                NativeMarkdownComposedViewportPaintOutcome::Pending(session) => {
                    self.session = Some(session);
                    return Ok(false);
                }
                NativeMarkdownComposedViewportPaintOutcome::Complete(completion) => {
                    if completion.receipt().missing_glyphs != 0 {
                        return Err(Error::UnsupportedCommand);
                    }
                    let permitted = match self.cursor {
                        Cursor::Text => completion.permits(&self.scene.key, &self.context),
                        Cursor::Cell { row, cell } => {
                            match &self.scene.grids[row].cells[cell].content {
                                GridCellContent::Composed(window) => {
                                    window.permits_completion(&completion, &self.context)
                                }
                                GridCellContent::Empty(_)
                                | GridCellContent::BeforeText(_)
                                | GridCellContent::OutsideText(_) => false,
                            }
                        }
                        _ => false,
                    };
                    if !permitted {
                        return Err(Error::Revoked);
                    }
                    drop(completion); // 每格真实完成claim立即归还，不累积全部context收据。
                    self.cursor = match self.cursor {
                        Cursor::Text => Cursor::Cell { row: 0, cell: 0 },
                        Cursor::Cell { row, cell } => Cursor::Cell {
                            row,
                            cell: cell + 1,
                        },
                        _ => unreachable!("只有真实字形游标取得绘制完成票"),
                    };
                }
            }
        }
    }
}

impl NativeMarkdownPagedScenePaintSession for Paint {
    /// 每个量子只在入口和出口深核整场景；失败同样归还Native状态并检查真实Source首错。
    fn step(
        mut self: Box<Self>,
        budget: NativeMarkdownPagedScenePaintBudget,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedScenePaintStep> {
        use super::super::producer::view_source_io;
        self.scene
            .validate_ready(&|| current() && self.scene.current())?;
        let mut work = NativeMarkdownPagedScenePaintWork::default();
        let result = self.advance(budget, &mut work, current);
        // 不在推进的?路径提前退出，Native或普通取消失败也必须走原全图Source/字体检查。
        // 已有Source首errno保原顺序；出口新发现的Source故障只提升普通失败，不伪造磁盘错误。
        let checked = self
            .scene
            .validate_ready(&|| current() && self.scene.current());
        let complete = match (result, checked) {
            (Err(cause), Err(check)) if !view_source_io(&cause) && view_source_io(&check) => {
                return Err(check);
            }
            (Err(cause), _) => return Err(cause),
            (Ok(_), Err(cause)) => return Err(cause),
            (Ok(complete), Ok(())) => complete,
        };
        let outcome = if complete {
            let completion = NativeMarkdownPagedScenePaintCompletion::from_native_composite(
                &self.scene.key,
                self.context.clone(),
                self.total,
            );
            NativeMarkdownPagedScenePaintOutcome::Complete(completion)
        } else {
            NativeMarkdownPagedScenePaintOutcome::Pending(self)
        };
        Ok(NativeMarkdownPagedScenePaintStep { work, outcome })
    }
}
