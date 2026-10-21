//! Resumable decorations for the original body or table cell. Every visited line,
//! part, and selection entry consumes the shared metadata allowance.
//!
//! The synchronous background and selection entry points remain complete pixel
//! comparisons; asynchronous Body and Grid use this same original-window traversal.
//! All backgrounds precede selections, and all selections precede glyphs, including
//! across suspension. The cursor owns no context: Scene Paint alone owns the
//! unpublished candidate and discards it on failure. Original Line/Part records and
//! display mappings are already sealed, so only bounded indexes survive a quantum.
//! No glyphs, source text, selected content, or duplicate layout enters this state;
//! metadata work counts actual visited records rather than estimating whole cells.

use super::*;
use omni_markdown_core::MarkdownComposedLineAffinity;

/// Holds original-window traversal positions and one mapped range, without document
/// text, a context claim, or a second layout graph.
pub(in crate::native_surface) struct NativeMarkdownComposedViewportDecorationCursor {
    cursor: Decoration,
    // Background work has no canonical selection. Selection work retains only the
    // two endpoints needed to intersect the original window's canonical range.
    canonical: Option<Range<usize>>,
    // Keep this selection's display endpoints until it finishes, then overwrite
    // them with the next selection instead of collecting all mapping results.
    selected: Option<Range<usize>>,
    // Marked styling changes presentation only. Original Input ranges, glyphs, and
    // line slots remain unchanged; the old Scene retires when composition commits.
    marked: bool,
    native_draw_returned: bool,
}

#[derive(Clone, Copy)]
// States mirror the original line/part loops and outer selection loop. Observing
// the end also costs one record. Stage transitions do not certify glyph completion;
// the Scene's subsequent glyph session must finish the candidate itself.
enum Decoration {
    BackgroundLine(usize),
    BackgroundPart {
        line: usize,
        part: usize,
    },
    SelectionEntry(usize),
    SelectionLine {
        selection: usize,
        line: usize,
    },
    SelectionPart {
        selection: usize,
        line: usize,
        part: usize,
    },
    Done,
}

pub(in crate::native_surface) struct NativeMarkdownComposedViewportDecorationStep {
    pub metadata_records: usize,
    pub complete: bool,
}

impl NativeMarkdownComposedViewportDecorationCursor {
    /// Background work consumes sealed original lines/parts. Construction calls
    /// no Native API and acquires no additional Source or font.
    pub(in crate::native_surface) fn backgrounds() -> Self {
        Self {
            cursor: Decoration::BackgroundLine(0),
            canonical: None,
            selected: None,
            marked: false,
            native_draw_returned: false,
        }
    }

    /// Intersects canonical selections with the sealed window. Body coordinates
    /// stay unchanged; a cell still maps through its original display Source.
    pub(in crate::native_surface) fn selections(
        canonical: Range<usize>,
        selections: &[Range<usize>],
    ) -> Self {
        // Input supplies ordered disjoint ranges. Skip metadata ending before this
        // window without reading that text; the first range after the window ends
        // SelectionEntry rather than scanning every document-wide Find result.
        let first = selections.partition_point(|selection| selection.end <= canonical.start);
        Self {
            cursor: Decoration::SelectionEntry(first),
            canonical: Some(canonical),
            selected: None,
            marked: false,
            native_draw_returned: false,
        }
    }

    /// Uses the same actual selection mapping for composition, replacing filled
    /// rectangles with each part's original font underline. It does not interpret
    /// IME attributes, duplicate marked state, or change candidate/selection ranges.
    pub(in crate::native_surface) fn marked_text(mut self) -> Self {
        self.marked = true;
        self
    }
}

impl NativeMarkdownComposedViewport {
    /// Consumes only the caller's remaining metadata allowance. Any failure makes
    /// the outer owner discard the unpublished canvas without replaying draws.
    pub(in crate::native_surface) fn advance_decorations(
        &self,
        cursor: &mut NativeMarkdownComposedViewportDecorationCursor,
        context: &CGContext,
        selections: &[Range<usize>],
        selection_color: [f64; 4],
        budget: usize,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportDecorationStep,
        NativeMarkdownComposedViewportPaintError,
    > {
        let result = (|| -> std::result::Result<_, NativeMarkdownComposedViewportPaintCause> {
            self.require(current)?;
            // Balance colors and background clipping within this call so another
            // cell cannot inherit them. Backgrounds use unclipped actual line/part
            // frames and therefore need the same window clip as synchronous paint.
            // Selection rectangles are already clipped below; clipping again would
            // multiply fractional-edge coverage. The outer border stage builds its
            // path after backgrounds finish, so ClipToRect cannot clear pending borders.
            CGContext::save_g_state(Some(context));
            let restore = Restore(context);
            if cursor.canonical.is_none() {
                CGContext::clip_to_rect(Some(context), self.clip());
            }
            let mut records = 0;
            while records < budget && !matches!(cursor.cursor, Decoration::Done) {
                self.require(current)?;
                records += 1; // Each iteration visits one actual record, without a hidden whole-cell scan.
                match cursor.cursor {
                    Decoration::BackgroundLine(line) => {
                        // A line background has a fixed rectangle count; its parts
                        // are visited individually in the next stage. Offscreen
                        // overscan retains metadata but produces no pixels or part work.
                        if let Some(actual) = self.lines.get(line) {
                            if self.visible(actual.frame) {
                                self.line_background(
                                    actual,
                                    context,
                                    &mut cursor.native_draw_returned,
                                    current,
                                )?;
                                // The line record and block background are complete.
                                // Sealed facts can prove all parts lack code background,
                                // skipping only a zero-draw loop, without estimating ink.
                                cursor.cursor = if actual.has_inline_code_background {
                                    Decoration::BackgroundPart { line, part: 0 }
                                } else {
                                    Decoration::BackgroundLine(line + 1)
                                };
                            } else {
                                cursor.cursor = Decoration::BackgroundLine(line + 1);
                            }
                        } else {
                            cursor.cursor = Decoration::Done;
                        }
                    }
                    Decoration::BackgroundPart { line, part } => {
                        // Inspect at most one original part, charging equally whether
                        // or not it has inline code. Advance only after its rectangle
                        // operation returns so suspension cannot replay a Native fill.
                        if let Some(actual) = self.lines[line].parts.get(part) {
                            self.part_background(
                                &self.lines[line],
                                actual,
                                context,
                                &mut cursor.native_draw_returned,
                                current,
                            )?;
                            cursor.cursor = Decoration::BackgroundPart {
                                line,
                                part: part + 1,
                            };
                        } else {
                            cursor.cursor = Decoration::BackgroundLine(line + 1);
                        }
                    }
                    Decoration::SelectionEntry(selection) => {
                        // Intersect canonical ranges with the original window first.
                        // Body endpoints stay unchanged; cell syntax maps through its
                        // existing directory. No line/part is visited here, and endpoint
                        // mapping releases Source leases before later Native drawing.
                        let canonical = cursor.canonical.as_ref().expect("选区续体持原格范围");
                        if let Some(actual) = selections
                            .get(selection)
                            .filter(|range| range.start < canonical.end)
                        {
                            let start = actual.start.max(canonical.start);
                            let end = actual.end.min(canonical.end);
                            if start >= end {
                                cursor.cursor = Decoration::SelectionEntry(selection + 1);
                            } else {
                                let (start, end) = match &self.domain {
                                    LayoutDomain::Body => (start, end),
                                    LayoutDomain::TableCell { display, .. } => {
                                        // Map this cell's start downstream and end
                                        // upstream, leaving hidden URLs no selection
                                        // area. Preserve either Source read failure
                                        // instead of using unmapped display endpoints.
                                        let start = display
                                            .canonical_to_display(
                                                start,
                                                MarkdownComposedLineAffinity::Downstream,
                                            )
                                            .map_err(|cause| {
                                                NativeMarkdownComposedViewportError::Composed(
                                                    MarkdownLineComposeError::Source(cause),
                                                )
                                            })?
                                            .expect("原格交集在显示映射范围内");
                                        let end = display
                                            .canonical_to_display(
                                                end,
                                                MarkdownComposedLineAffinity::Upstream,
                                            )
                                            .map_err(|cause| {
                                                NativeMarkdownComposedViewportError::Composed(
                                                    MarkdownLineComposeError::Source(cause),
                                                )
                                            })?
                                            .expect("原格交集在显示映射范围内");
                                        (start, end)
                                    }
                                };
                                if end < start {
                                    return Err(
                                        NativeMarkdownComposedViewportError::InvalidRange.into()
                                    );
                                } // Like selection_rects, reversed mapping is a coordinate error, not absent highlighting.
                                if start == end {
                                    // Hidden syntax has no visible area; no part scan
                                    // is needed merely to confirm an empty rectangle.
                                    cursor.cursor = Decoration::SelectionEntry(selection + 1);
                                } else {
                                    cursor.selected = Some(start..end);
                                    cursor.cursor =
                                        Decoration::SelectionLine { selection, line: 0 };
                                }
                            }
                        } else {
                            cursor.cursor = Decoration::Done;
                        }
                    }
                    Decoration::SelectionLine { selection, line } => {
                        // A selection can span several prepared soft lines. Charge
                        // each line before its parts and use the synchronous
                        // selection_rects visibility test, not a selection envelope.
                        if let Some(actual) = self.lines.get(line) {
                            let selected =
                                cursor.selected.as_ref().expect("逐行绘制保同一已映射选区");
                            let source = &actual.handle.facts().source_utf16;
                            // The original Reader verified every part is contained
                            // in its line's Source range. A disjoint line therefore
                            // proves every part intersection empty. Charge the line
                            // but skip that zero-draw loop. Actual Source endpoints,
                            // rather than visual/ink order, preserve RTL and GCB seams.
                            let intersects =
                                selected.start < source.end && source.start < selected.end;
                            cursor.cursor = if intersects && self.visible(actual.frame) {
                                Decoration::SelectionPart {
                                    selection,
                                    line,
                                    part: 0,
                                }
                            } else {
                                Decoration::SelectionLine {
                                    selection,
                                    line: line + 1,
                                }
                            };
                        } else {
                            cursor.selected = None;
                            cursor.cursor = Decoration::SelectionEntry(selection + 1);
                        }
                    }
                    Decoration::SelectionPart {
                        selection,
                        line,
                        part,
                    } => {
                        // One actual part yields at most one intersecting rectangle,
                        // without collecting all its GCBs. Fill only after resolving
                        // real GCB endpoints and part geometry; a directory/font
                        // failure invalidates the entire candidate.
                        if let Some(actual) = self.lines[line].parts.get(part) {
                            let facts = actual.handle.facts();
                            if !matches!(
                                facts.kind,
                                MarkdownComposedLinePartKind::FoldedSection
                                    | MarkdownComposedLinePartKind::TableRow
                            ) {
                                let selected = cursor
                                    .selected
                                    .as_ref()
                                    .expect("逐 part 绘制保同一已映射选区");
                                let start = selected.start.max(facts.source_utf16.start);
                                let end = selected.end.min(facts.source_utf16.end);
                                if start < end {
                                    // Expand to the original whole GCB, then intersect
                                    // with this shaping part. A GCB may span UBA/font
                                    // runs; each owns only its actual geometry and
                                    // cannot reuse adjacent endpoints or foreign carets.
                                    let range =
                                        self.boundary(start, false)?.max(facts.source_utf16.start)
                                            ..self.boundary(end, true)?.min(facts.source_utf16.end);
                                    let mut frame =
                                        self.part_rect(&self.lines[line], actual, range, current)?;
                                    if cursor.marked {
                                        // Partial, wrapped, and RTL ranges share the
                                        // actual part_rect horizontal endpoints. The
                                        // original part font alone supplies vertical
                                        // metrics, not string length or line maxima.
                                        let metrics = self.palette[actual.font]
                                            .metrics()
                                            .map_err(NativeMarkdownComposedViewportError::Font)?;
                                        frame.origin.y =
                                            self.part_baseline(&self.lines[line], actual).y
                                                + facts.origin_y_26_6 as f64 / 64.0
                                                + metrics.underline_position
                                                - metrics.underline_thickness / 2.0;
                                        frame.size.height = metrics.underline_thickness;
                                    }
                                    if let Some(frame) =
                                        super::super::geometry::clip_selection(frame, self.clip())
                                    {
                                        let [r, g, b, a] = selection_color;
                                        self.fill_color(context, [r, g, b, a]);
                                        // Reader and temporary font borrows have
                                        // ended. Check the same Source/current before
                                        // fill and again below; the unpublished canvas
                                        // prevents cancellation from publishing this rectangle.
                                        self.require(current)?;
                                        CGContext::fill_rect(Some(context), frame);
                                        cursor.native_draw_returned = true;
                                    }
                                }
                            }
                            cursor.cursor = Decoration::SelectionPart {
                                selection,
                                line,
                                part: part + 1,
                            };
                        } else {
                            cursor.cursor = Decoration::SelectionLine {
                                selection,
                                line: line + 1,
                            };
                        }
                    }
                    Decoration::Done => unreachable!("完成续体不再消费 metadata"),
                }
                self.require(current)?;
            }
            // Restore graphics state before reentrant current so it cannot inherit
            // a cell-local clip or selection color.
            drop(restore);
            self.require(current)?;
            Ok(NativeMarkdownComposedViewportDecorationStep {
                metadata_records: records,
                complete: matches!(cursor.cursor, Decoration::Done),
            })
        })();
        // Recheck the original Source even after geometry failure or revocation,
        // preserving actual read failure rather than rewriting it as cancellation.
        let result = match self.require(current) {
            Ok(()) => result,
            Err(cause) => Err(cause.into()),
        };
        result.map_err(|cause| NativeMarkdownComposedViewportPaintError {
            cause,
            native_draw_returned: cursor.native_draw_returned,
        })
    }

    /// Uses one sealed line's block geometry, drawing at most five rectangles
    /// without enumerating its parts.
    fn line_background(
        &self,
        line: &Line,
        context: &CGContext,
        native_draw_returned: &mut bool,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintCause> {
        // Existing Native constants supply colors without changing font size,
        // baseline, or paragraph spacing. The synchronous comparison retains its
        // original loop; this step consumes one line of the same sealed geometry.
        let [red, green, blue, alpha] = NATIVE_MARKDOWN_CODE_BACKGROUND;
        self.fill_color(context, [red, green, blue, alpha]);
        // Reuse the frame that already determines wrapping, baseline, and scroll
        // height; painting cannot expand the body layout.
        use crate::native_surface::preview_paragraph::PreviewParagraphBackground as Background;
        match line.paragraph.background {
            Some(Background::Code) => {
                // Cover the actual code line slot without recalculating wrapping
                // or scroll height during drawing.
                CGContext::fill_rect(Some(context), line.frame);
                *native_draw_returned = true;
            }
            Some(Background::Quote) => {
                // The original paragraph policy supplies quote fill and vertical
                // borders. Only its actual first/last soft lines own horizontal
                // caps; at most five rectangles require no text or neighboring parts.
                self.fill_color(context, [0.10, 0.12, 0.16, 1.0]);
                CGContext::fill_rect(Some(context), line.frame);
                self.fill_color(context, [0.28, 0.34, 0.44, 1.0]);
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(line.frame.origin, NSSize::new(1.0, line.frame.size.height)),
                );
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(
                            line.frame.origin.x + line.frame.size.width - 1.0,
                            line.frame.origin.y,
                        ),
                        NSSize::new(1.0, line.frame.size.height),
                    ),
                );
                // Avoid horizontal borders between soft lines: only the original
                // paragraph's actual first/last lines own the corresponding edge.
                let facts = line.handle.facts();
                if facts.source_utf16.start == facts.paragraph_utf16.start {
                    CGContext::fill_rect(
                        Some(context),
                        NSRect::new(
                            NSPoint::new(
                                line.frame.origin.x,
                                line.frame.origin.y + line.frame.size.height - 1.0,
                            ),
                            NSSize::new(line.frame.size.width, 1.0),
                        ),
                    );
                }
                if facts.source_utf16.end == facts.paragraph_utf16.end {
                    CGContext::fill_rect(
                        Some(context),
                        NSRect::new(line.frame.origin, NSSize::new(line.frame.size.width, 1.0)),
                    );
                }
                *native_draw_returned = true;
            }
            Some(Background::Rule) => {
                // Position the rule inside the original paragraph text slot,
                // independent of viewport edges or suspension. Paragraph spacing
                // does not shift the thin line out of its actual 12-point text slot.
                let low = line.frame.origin.y + line.geometry.after;
                let y = low + line.geometry.text_height / 2.0;
                self.fill_color(context, [0.28, 0.34, 0.44, 1.0]);
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(line.frame.origin.x, y),
                        NSSize::new(line.frame.size.width, 1.0),
                    ),
                );
                *native_draw_returned = true;
            }
            None => {}
        }
        self.fill_color(context, [red, green, blue, alpha]);
        self.require(current)?;
        Ok(())
    }

    /// Reads this inline-code part's actual font metrics without scanning adjacent
    /// glyphs, lines, or Sources.
    fn part_background(
        &self,
        line: &Line,
        part: &Part,
        context: &CGContext,
        native_draw_returned: &mut bool,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintCause> {
        let [red, green, blue, alpha] = NATIVE_MARKDOWN_CODE_BACKGROUND;
        self.fill_color(context, [red, green, blue, alpha]);
        // The original part already binds its font and Source. Determine only
        // whether this item has code-background area; zero advance gets no
        // placeholder rectangle, and hidden markers cannot gain color from INLINE_CODE.
        let facts = part.handle.facts();
        if !part.has_inline_code_background() {
            return Ok(()); // Hidden syntax, hard separators, and zero-width characters have no code-background area.
        }
        self.require(current)?;
        // Use this part's original Native font ascent/descent, not another cell's
        // size or the line's maximum height. Min/max of actual endpoints preserves
        // RTL code coverage even when the advance is negative.
        let metrics = self.palette[part.font]
            .metrics()
            .map_err(NativeMarkdownComposedViewportError::Font)?;
        let start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
        let end = start + facts.x_advance_26_6 as f64 / 64.0;
        let baseline = self.part_baseline(line, part).y + facts.origin_y_26_6 as f64 / 64.0;
        CGContext::fill_rect(
            Some(context),
            NSRect::new(
                NSPoint::new(start.min(end), baseline - metrics.descent),
                NSSize::new((end - start).abs(), metrics.ascent + metrics.descent),
            ),
        ); // Original font metrics bound the background; later selection paint must remain above it.
        *native_draw_returned = true;
        // The actual fill has returned. A failed recheck preserves that fact and
        // makes the outer owner discard the entire candidate.
        self.require(current)?;
        Ok(())
    }
}
