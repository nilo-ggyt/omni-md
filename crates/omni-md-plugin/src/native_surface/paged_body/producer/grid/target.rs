//! Cell Find uses actual display mapping and the original line directory; a row-height rectangle cannot
//! substitute for the target scalar actual line.

use super::super::super::target::SourceTargetRequest;
use super::*;
use omni_markdown_core::MarkdownComposedLineAffinity;

/// The original Find intent holds the same request and next column to certify, without another Source,
/// Input graph, or text copy.
pub(super) struct RowTarget {
    pub(super) request: Rc<SourceTargetRequest>,
    column: usize,
}

/// Display endpoints come only from actual canonical-to-display mapping; anchors use actual font
/// heights from certified LineEnds.
pub(super) struct CellTarget {
    position: usize,
    first_end: usize,
    anchor: Option<CellTargetAnchor>,
    walk: Option<TargetWalk>,
}

/// Create an anchor only after measuring the original LineEnd and both first-grapheme edges, without
/// average-width or character-count guesses.
#[derive(Clone, Copy)]
struct CellTargetAnchor {
    top: f64,
    bottom: f64,
    left: f64,
    right: f64,
}

struct TargetWalk {
    // The cursor borrows the current output directory; the target request cannot extend a replaced
    // Reader or Native cell window lifetime.
    cursor: Option<MarkdownComposedLine>,
    metrics: Option<LineMetrics>,
    // Subtract actual measured line heights backward from the final measured line, without average-font
    // estimates for skipped lines.
    bottom: f64,
    /// After finding the target line, resume parts on the same handle without another backward scan or
    /// a full-line part array.
    target_top: Option<f64>,
    target_part: usize,
    // The original directory certifies the first complete grapheme; across Bidi items retain a bounded
    // envelope without part/font ownership.
    grapheme: std::ops::Range<usize>,
    left: Option<f64>,
    right: Option<f64>,
    covered: usize,
}

impl CellTarget {
    /// A successor prefix Reader invalidates the weak cursor, while measured line anchors for the same
    /// Source may remain.
    pub(super) fn reset_scan(&mut self) {
        self.walk = None;
    }
}

impl Grid {
    /// Take over cell glyph production only for an intent in this row; the same body Producer advances
    /// out-of-row targets in order.
    pub(super) fn advance_source_target(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        scene: &NativeMarkdownPagedSceneRequest,
        semantics: &mut super::super::super::semantics::RootLeaf,
    ) -> Result<Option<Advance>> {
        // Check original request, Scene, and canonical Source first; cancelled or other-row intents
        // cannot drive this cell reads.
        let pending = owner.source_target.borrow().clone().filter(|target| {
            target.pending()
                && target.source == source.authority()
                && target.matches(owner, scene)
                && self.source_range().contains(&target.range.location)
        });
        let Some(pending) = pending else {
            // Drop old target anchors while preserving actual cell layout still needed by the same
            // viewport.
            self.target = None;
            return Ok(None);
        };
        if !self
            .target
            .as_ref()
            .is_some_and(|target| Rc::ptr_eq(&target.request, &pending))
        {
            // A new request cannot reuse old Find display endpoints even if canonical coordinate values
            // match.
            for cell in &mut self.cells {
                cell.target = None;
            }
            self.target = Some(RowTarget {
                request: pending,
                column: 0,
            });
        }
        // Move out the unique target to avoid borrowing the entire row; restore it on failure for the
        // outer owner to report.
        let mut target = self.target.take().expect("真实格目标已装配");
        let result = self.advance_cell_target(owner, source, scene, semantics, &mut target);
        self.target = Some(target);
        result
    }

    /// Advance one column preparation or one directory quantum at a time; an uncertified suffix is not
    /// cell EOF.
    fn advance_cell_target(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        scene: &NativeMarkdownPagedSceneRequest,
        semantics: &mut super::super::super::semantics::RootLeaf,
        target: &mut RowTarget,
    ) -> Result<Option<Advance>> {
        // Visit this row bounded column metadata only; unknown cells use the original serial
        // preparation quantum without eager row reads.
        let columns = self.cells.len();
        let row_start_caret = target.request.range.length == 0
            && target.request.range.location == self.row.source_range().start;
        // Column advances across actual preparation turns; visit remaining columns only instead of
        // restarting a full-count walk from a later column.
        // Excluding every column still yields original Unsupported; invalid columns or unowned targets
        // must not become Pending.
        for _ in target.column..columns {
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
                // Even empty cells require actual raw authority; unfinished columns cannot contribute
                // guessed hit ranges.
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
                // Advance original CellInline/display conversion through the same RootLeaf Request,
                // without another parser.
                return cell
                    .advance(owner, source, &self.row, false, semantics, &mut self.work)
                    .map(Some);
            };
            // A zero-width row-start caret belongs to column zero, whose raw ticket comes from the
            // original row/column factory;
            // the first visible-array entry cannot replace its column identity. Change the internal
            // locator only, preserving the original canonical row start.
            // A huge row may have only a middle viewport prefix; locate its actual first line without
            // waiting for cell EOF or estimating parent-row height.
            let row_edge = row_start_caret && column == 0 && cell.column == 0;
            let canonical = if row_edge {
                range.start
            } else {
                target.request.range.location
            };
            let belongs = row_edge
                || range.contains(&canonical)
                || target.request.range.length == 0 && canonical == range.end;
            if !belongs {
                // Pipes and delimiter rows are not displayed cell text; advance the column cursor
                // without clamping into a neighboring fake hit.
                target.column += 1;
                continue;
            }
            let anchor = if let Some(display) = &cell.display {
                if cell.target.is_none() {
                    // Certify both endpoints through original display mapping; hidden-syntax gaps are
                    // not visible UTF-16 offsets.
                    let position = display
                        .canonical_to_display(canonical, MarkdownComposedLineAffinity::Downstream)
                        .map_err(ViewError::Document)?
                        .ok_or(Error::Unsupported)?;
                    let first_end = display
                        .canonical_to_display(
                            if row_edge {
                                canonical
                            } else {
                                target.request.first_end
                            },
                            MarkdownComposedLineAffinity::Downstream,
                        )
                        .map_err(ViewError::Document)?
                        .ok_or(Error::Unsupported)?;
                    // Equal endpoints mean a zero caret; original sealed mapping still handles leading
                    // hidden bold/code/link syntax.
                    // The final Scene validates the original zero range at row.start rather than
                    // certifying this display point as a source character.
                    cell.target = Some(CellTarget {
                        position,
                        first_end,
                        anchor: None,
                        walk: None,
                    });
                }
                if cell.pending.is_some() {
                    // A sealed LineEnd may still lack actual font height; finish measurement before
                    // querying target geometry.
                    return cell
                        .advance(owner, source, &self.row, false, semantics, &mut self.work)
                        .map(Some);
                }
                if !cell.find_target_anchor(owner, source, &target.request, &mut self.work)? {
                    // Check actual prefix coverage only; continue Composer for uncovered targets
                    // instead of declaring Source EOF.
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
                    // Coverage exists but the backward-scan budget expired; retain its cursor for the
                    // next turn without rescanning the cell.
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
                } // An actually empty cell has no glyphs; its target uses the original minimum cell content box.
            };
            // After actual same-Scene glyphs take over the mode anchor, do not overwrite it with the
            // minimal cell-target reveal.
            // Return to normal cell preparation rather than repeatedly yielding the same Target and
            // starving other visible columns/publication.
            if owner.mode_retains_measured_glyph(&target.request) {
                return Ok(None);
            }
            let top = self.top + VERTICAL + anchor.top;
            let bottom = self.top + VERTICAL + anchor.bottom;
            // Combine column-prefix width with actual first-grapheme carets so narrow/RTL viewports
            // reveal the target without requiring the whole column.
            let column_x = scene.virtual_viewport.layout_origin_x
                + owner.configuration.padding
                + self.widths[..column].iter().sum::<f64>()
                + HORIZONTAL;
            let x = column_x + anchor.left;
            let content_width = anchor.right - anchor.left; // Preserve a real zero-advance caret without inventing a one-point character box.
            let visible = NSRect::new(
                NSPoint::new(
                    scene.virtual_viewport.document_origin.x + scene.viewport.origin.x,
                    scene.virtual_viewport.document_origin.y,
                ),
                scene.viewport.size,
            );
            // A real grapheme or font line may exceed the viewport; require its actual leading band to
            // enter the current clip,
            // avoiding repeated reveal of an unfit full box. Ordinary targets still require full
            // containment in the original visible rectangle.
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
                // Send the actual column content box to original 2D reveal; Y-only scrolling could
                // incorrectly report an offscreen rightmost column as visible.

                return Ok(Some(Advance::Target {
                    request: target.request.clone(),
                    rect: NSRect::new(
                        NSPoint::new(x, top),
                        NSSize::new(content_width, bottom - top),
                    ),
                }));
            }
            return Ok(None); // The target line is in the actual band; finish other visible columns under the same viewport before publication.
        }
        Err(Error::Unsupported) // Table pipes and delimiters are not visible cells; never clamp into neighboring text to fabricate a target box.
    }
}

impl Cell {
    /// Report uncovered when the actual first scalar exceeds the prefix; otherwise continue the
    /// directory cursor with the fixed part allowance.
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
            // Without a complete LineEnd the cell has no certifiable glyphs; Source length cannot
            // synthesize a first line.
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
        }; // Return the actual document lease before any Native glyph/caret getter.
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
                    } // A zero range is the original insertion point; do not silently move an internal scalar to the preceding grapheme boundary.
                    match reader.grapheme_query(target.position, MarkdownGraphemeQuery::Previous)? {
                        MarkdownGraphemeAnswer::Known(Some(boundary)) => boundary.utf16,
                        _ => return Err(ViewError::InvalidRange.into()),
                    }
                };
                let end = if target.position == target.first_end {
                    start // An empty target preserves its original caret; only nonempty targets consume the next complete grapheme.
                } else {
                    match reader.grapheme_query(start, MarkdownGraphemeQuery::Next)? {
                        MarkdownGraphemeAnswer::Known(Some(boundary)) => boundary.utf16,
                        _ => return Err(ViewError::InvalidRange.into()),
                    }
                };
                start..end
            };
            // Reader lends paged directory handles only, without a whole-cell line vector or string
            // read.
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
                return Ok(false); // Line measurement and target-part scanning share the original allowance so one complex cell cannot occupy the whole main-thread turn.
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
                    return Ok(false); // An unfinished mixed-font line cannot contribute to accumulated height.
                }
                let top = walk.bottom - metrics.height()?;
                let range = &metrics.handle.facts().source_utf16;
                let at_eof = self.complete && target.position == display.source().utf16_len();
                if !(range.contains(&target.position) || at_eof && target.position == range.end)
                    || target.first_end > range.end
                {
                    // Cross actually measured original directory lines only, without interpreting an
                    // unknown prefix end as cell EOF.
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
                    continue; // Combine actual pieces of the original first grapheme only, excluding other line glyphs from its envelope.
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
                        // Actual insertion positions still pass original grapheme/cluster checks;
                        // internal seams use certified paint edges above only.
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
                // GDEF/Native callbacks may revoke authority or expose a first Source failure; end
                // Reader leases before checking typed primary errors.
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
                    // All certified pieces come from one complete line, original font, and grapheme;
                    // publish the target only after actual full coverage.
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
                return Err(ViewError::InvalidRange.into()); // The original line has no such target part; do not clamp into a neighboring cell.
            }
        }
        Ok(false)
    }
}
