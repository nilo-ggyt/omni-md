//! One CPU candidate serially consumes actual Body and visible cells; Scene completion requires every actual cursor to finish.

use super::*;
use crate::native_surface::paged_line_viewport::{
    NativeMarkdownComposedViewportDecorationCursor, NativeMarkdownComposedViewportPaintWork,
};

/// Actual raster-batch position, without events or text; cancellation drops the current cursor and original context claim.
#[derive(Clone, Copy)]
enum Cursor {
    // Backgrounds/borders finish before glyphs; cell-by-cell suspension preserves the original RGBA coverage order.
    GridRow(usize),
    GridBackground { row: usize, cell: usize },
    GridBorderStart(usize),
    GridBorder { row: usize, cell: usize },
    GridBorderEnd(usize),
    // Text backgrounds, active line, and selections also advance by original window items before Grid selections and all glyphs.
    TextBackground,
    ActiveLine(usize),
    // Find lies below user selection and reuses the same canonical-to-display decoration cursor, never estimated text rectangles.
    TextFind,
    GridFind { row: usize, cell: usize },
    TextSelection,
    GridSelection { row: usize, cell: usize },
    Text,
    Cell { row: usize, cell: usize },
    // Composition styling follows actual glyphs and precedes the caret, so selection fill cannot cover it with committed-text appearance.
    TextMarked,
    GridMarked { row: usize, cell: usize },
    Done,
}

pub(super) struct Paint {
    scene: Rc<Scene>,
    context: CFRetained<CGContext>,
    cursor: Cursor,
    session: Option<NativeMarkdownComposedViewportPaintSession>,
    // Body and cells serially reuse one numeric decoration cursor, returning it on completion without accumulating cell context claims.
    decoration: Option<NativeMarkdownComposedViewportDecorationCursor>,
    /// Own one original-context gstate until this row strokes; cancellation restores it before any successor candidate.
    border_open: bool,
    total: NativeMarkdownPagedScenePaintWork,
}

impl Drop for Paint {
    /// Cancellation can occur between any column borders. Self still owns the context while restoring gstate, then drops that claim.
    fn drop(&mut self) {
        if self.border_open {
            CGContext::restore_g_state(Some(&self.context));
            CGContext::begin_path(Some(&self.context)); // This candidate's uncommitted row path must not reach a successor context user.
        }
    }
}

impl Paint {
    /// Construction draws nothing; the first actual quantum enters this same Text/Cell window under its original budget.
    pub(super) fn new(scene: Rc<Scene>, context: CFRetained<CGContext>) -> Self {
        // Even without Grid, decorations advance record by record; begin_cpu_paint cannot scan the whole window and charge only glyph work.
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

    /// Account actual inner work once, sharing the outer quantum across windows rather than renewing a 256-glyph allowance for each cell.
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
    /// Each structure/decoration record uses the same outer metadata allowance; all backgrounds/selections actually finish before glyphs.
    fn decorate(
        &mut self,
        work: &mut NativeMarkdownPagedScenePaintWork,
        budget: usize,
        current: &dyn Fn() -> bool,
    ) -> Result<bool> {
        let scene = self.scene.clone(); // A stack-local Rust Rc claim on the original Scene only, without a context claim or another layout graph.
        let permit = || current() && scene.current();
        let mut records = 1;
        match self.cursor {
            Cursor::GridRow(row) => {
                // A row background performs one fixed rectangle operation; later code backgrounds and column borders each consume remaining allowance.
                if row < scene.grids.len() {
                    scene.paint_grid_row_background(row, &self.context, &permit)?;
                    self.cursor = Cursor::GridBackground { row, cell: 0 };
                } else {
                    self.cursor = Cursor::TextBackground;
                }
            }
            Cursor::GridBackground { row, cell }
            | Cursor::GridSelection { row, cell }
            | Cursor::GridFind { row, cell }
            | Cursor::GridMarked { row, cell } => {
                // Four passes share cell order: backgrounds precede text decorations, Find lies below user selection, and marked styling follows glyphs.
                // Row/cell end observations are bounded metadata steps, not new layout or glyph sessions.
                let finding = matches!(self.cursor, Cursor::GridFind { .. });
                let marking = matches!(self.cursor, Cursor::GridMarked { .. });
                let selection =
                    finding || marking || matches!(self.cursor, Cursor::GridSelection { .. });
                // Ranges belong to the original key's complete Input, without duplicate publication or a successor client's newer marked range.
                let marked = scene
                    .key
                    .request
                    .input
                    .marked
                    .map(|range| range.location..range.location + range.length);
                let ranges: &[Range<usize>] = if marking {
                    marked.as_slice()
                } else if finding {
                    &scene.find_highlights
                } else {
                    &scene.selections
                };
                let fill = if marking {
                    scene.colors.text
                } else if finding {
                    [1.0, 0.75, 0.12, 0.25]
                } else {
                    scene.colors.selection
                };
                if row == scene.grids.len() {
                    self.cursor = if marking {
                        Cursor::Done
                    } else if finding {
                        Cursor::TextSelection
                    } else {
                        Cursor::Text
                    };
                } else if cell == scene.grids[row].cells.len() {
                    self.cursor = if marking {
                        Cursor::GridMarked {
                            row: row + 1,
                            cell: 0,
                        }
                    } else if finding {
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
                        // Entering a cell creates a bounded cursor; a later actual step visits lines, parts, or selections.
                        // Cursor acquisition and actual decoration work are charged separately; one acquisition does not pay for the entire cell.
                        if self.decoration.is_none() {
                            self.decoration = Some(if selection {
                                let cursor = window
                                    .selection_cursor(ranges, &permit)
                                    .map_err(scene_error)?;
                                if marking {
                                    cursor.marked_text()
                                } else {
                                    cursor
                                }
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
                            // Viewport uses only this turn's remaining allowance; the next cell cannot receive a fresh 32 records.
                            records = step.metadata_records;
                            if !step.complete {
                                work.metadata_records += records;
                                self.total.metadata_records += records;
                                return Ok(true);
                            }
                            // Advance cells only after actual completion, without truncating the last part or omitting later selections.
                            self.decoration = None;
                            self.cursor = if marking {
                                Cursor::GridMarked {
                                    row,
                                    cell: cell + 1,
                                }
                            } else if finding {
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
                        self.cursor = if marking {
                            Cursor::GridMarked {
                                row,
                                cell: cell + 1,
                            }
                        } else if finding {
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
                // Open a cross-quantum CG path only after code backgrounds finish, because clipping would clear that path.
                scene.begin_grid_border(row, &self.context, &permit)?;
                self.border_open = true;
                self.cursor = Cursor::GridBorder { row, cell: 0 };
            }
            Cursor::GridBorder { row, cell } => {
                // Each visible cell contributes one actual column border; no single synchronous step hides all columns.
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
                // Stroke all segments once in one path, preserving seam antialiasing and edge alpha.
                let result = scene.finish_grid_border(row, &self.context, &permit);
                // Stroke failure/revocation still balances the saved state; this path belongs only to the unpublished candidate.
                CGContext::restore_g_state(Some(&self.context));
                self.border_open = false;
                if let Err(cause) = result {
                    // Current path is outside gstate; invalidation before stroke must also clear uncommitted segments.
                    CGContext::begin_path(Some(&self.context));
                    return Err(cause);
                }
                self.cursor = Cursor::GridRow(row + 1);
            }
            Cursor::TextBackground
            | Cursor::TextSelection
            | Cursor::TextFind
            | Cursor::TextMarked => {
                // Body/cells consume sealed line/part geometry through the same decoration cursor; Body selection stays canonical.
                // Cursor creation and traversal pay separately; entering Body does not reset this quantum to 32 records.
                let finding = matches!(self.cursor, Cursor::TextFind);
                let marking = matches!(self.cursor, Cursor::TextMarked);
                let selection = finding || marking || matches!(self.cursor, Cursor::TextSelection);
                let marked = scene
                    .key
                    .request
                    .input
                    .marked
                    .map(|range| range.location..range.location + range.length);
                let ranges: &[Range<usize>] = if marking {
                    marked.as_slice()
                } else if finding {
                    &scene.find_highlights
                } else {
                    &scene.selections
                };
                let fill = if marking {
                    scene.colors.text
                } else if finding {
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
                            let cursor = NativeMarkdownComposedViewportDecorationCursor::selections(
                                window.source_range(),
                                ranges,
                            );
                            if marking {
                                cursor.marked_text()
                            } else {
                                cursor
                            }
                        } else {
                            NativeMarkdownComposedViewportDecorationCursor::backgrounds()
                        });
                        false
                    }
                } else {
                    true // A Grid-only Scene has no Body text window and creates no empty Source, font, or second paint session.
                };
                if complete {
                    self.decoration = None;
                    self.cursor = if marking {
                        Cursor::GridMarked { row: 0, cell: 0 }
                    } else if finding {
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
                // Active-line work visits one actual soft line; without an active line it skips directly instead of traversing every range.
                // A line frame is not selected text; this stage neither reads nor copies text or recomputes its layout.
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
        // Each window verifies its own Source/directory; full Scene font/Source validation occurs at quantum entry and exit.
        // Retain original Input/geometry cancellation here; the step exit's actual Source check still determines failure precedence.
        if !permit() {
            return Err(Error::Revoked);
        }
        Ok(true)
    }
    /// Advance retains actual work and the original cursor; the sole step boundary owns whole-Scene entry/exit checks.
    fn advance(
        &mut self,
        budget: NativeMarkdownPagedScenePaintBudget,
        work: &mut NativeMarkdownPagedScenePaintWork,
        current: &dyn Fn() -> bool,
    ) -> Result<bool> {
        // Composite Scenes retain the actual raster entry's quantum limits; dividing into cells cannot evade the caller's budget.
        let maximum = NativeMarkdownComposedViewportPaintBudget::default();
        if budget.glyphs > maximum.glyphs || budget.metadata_records > maximum.metadata_records {
            return Err(Error::Raster(
                NativeMarkdownComposedViewportPaintCause::InvalidBudget.to_string(),
            ));
        }
        loop {
            // Within a synchronous quantum, check original cancellation; actual windows verify resources during their own reads/draws.
            // A border must not rescan all Grid/fonts; the exit still unconditionally validates the entire Scene.
            if !current() || !self.scene.current() {
                return Err(Error::Revoked);
            }
            if matches!(self.cursor, Cursor::Done) {
                self.scene.caret(&self.context, current)?;
                return Ok(true); // Only actual decoration/glyph/caret completion lets step certify the whole frame.
            }
            if work.metadata_records == budget.metadata_records || work.glyphs == budget.glyphs {
                return Ok(false);
            }
            // Backgrounds, selections, and glyphs share this turn's metadata remainder; exhaustion suspends the original cursor in any stage.
            let remaining_metadata = budget.metadata_records - work.metadata_records;
            if self.decorate(work, remaining_metadata, current)? {
                continue;
            }
            // Entering an actual window or empty cell costs one metadata record; empty cells create no font, Source, or glyph session.
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
                            // Without marked input, finish directly rather than adding an empty per-line decoration pass to ordinary typing.
                            self.cursor = if self.scene.key.request.input.marked.is_some() {
                                Cursor::TextMarked
                            } else {
                                Cursor::Done
                            };
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
            // Window acquisition also consumes this turn's metadata; exact exhaustion retains its original session for the next turn.
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
            let step = step.map_err(paint_error)?; // Actual failure consumes the session; step exit still preserves original Source failure priority.
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
                    drop(completion); // Return each cell's actual completion claim immediately instead of accumulating all context receipts.
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
    /// Validate the whole Scene at quantum entry/exit; failure also restores owned Native state and checks original Source failure.
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
        // Do not exit directly through advance's ? path: Native/cancellation failures still require the original full Source/font check.
        // Preserve an existing Source errno's order; a Source failure discovered at exit supersedes only non-Source failure without inventing disk faults.
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
