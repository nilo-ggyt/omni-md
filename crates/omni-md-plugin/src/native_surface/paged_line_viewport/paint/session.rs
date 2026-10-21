//! A consuming cursor for one unpublished bitmap. Each step reads bounded original glyphs;
//! the synchronous paint limit never truncates the resumable document output.

use super::*;
use objc2_core_foundation::{CFRetained, CGAffineTransform};
use objc2_core_graphics::{
    CGBitmapContextGetBitsPerPixel, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData,
    CGBitmapContextGetHeight, CGBitmapContextGetWidth,
};
use objc2_foundation::MainThreadMarker;
use std::{fmt, ptr};

const METADATA_LIMIT: usize = 32;

/// Allowances constrain this step alone. Exhaustion preserves the actual continuation
/// position without replaying a Native draw that has already returned.
#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownComposedViewportPaintBudget {
    pub glyphs: usize,
    pub metadata_records: usize,
}
impl Default for NativeMarkdownComposedViewportPaintBudget {
    fn default() -> Self {
        Self {
            glyphs: NATIVE_MARKDOWN_PAGED_GLYPH_BATCH,
            metadata_records: METADATA_LIMIT,
        }
    }
}

/// Metadata work counts actual line entries, part entries, and advance checks;
/// it does not pretend that those records are document input bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeMarkdownComposedViewportPaintWork {
    pub glyphs: usize,
    pub batches: usize,
    pub metadata_records: usize,
}

#[derive(Debug)]
pub enum NativeMarkdownComposedViewportPaintOutcome {
    Pending(NativeMarkdownComposedViewportPaintSession),
    Complete(NativeMarkdownComposedViewportPaintCompletion),
}
#[derive(Debug)]
pub struct NativeMarkdownComposedViewportPaintStep {
    pub work: NativeMarkdownComposedViewportPaintWork,
    pub outcome: NativeMarkdownComposedViewportPaintOutcome,
}

/// Issued only after the original cursor finishes. Retaining the actual context beside
/// the Scene identity prevents a recycled context address from authorizing another bitmap.
/// Consumers must release this receipt and their final context claim, then verify the
/// actual release callback before sealing pixels.
pub struct NativeMarkdownComposedViewportPaintCompletion {
    key: NativeMarkdownPagedSceneKey,
    cell_window: Option<std::rc::Weak<()>>,
    context: CFRetained<CGContext>,
    receipt: NativeMarkdownComposedViewportPaintReceipt,
    work: NativeMarkdownComposedViewportPaintWork,
}
impl NativeMarkdownComposedViewportPaintCompletion {
    /// Checks this candidate's identity only. Publication still requires the original
    /// Scene health, geometry, and actual context release to be verified by the consumer.
    #[must_use]
    pub fn permits(&self, key: &NativeMarkdownPagedSceneKey, context: &CGContext) -> bool {
        self.cell_window.is_none() && self.key.same(key) && ptr::eq(&*self.context, context)
    }
    /// Completion belongs to the exact inner cell that created the session. A shared
    /// Source or outer nonce cannot substitute another cell window.
    pub(in crate::native_surface::paged_line_viewport) fn permits_cell(
        &self,
        window: &NativeMarkdownComposedViewport,
        context: &CGContext,
    ) -> bool {
        let LayoutDomain::TableCell { identity, .. } = &window.domain else {
            return false;
        };
        self.cell_window.as_ref().is_some_and(|owner| {
            std::rc::Weak::ptr_eq(owner, &Rc::downgrade(identity)) && owner.strong_count() != 0
        }) && self.key.same(&window.key)
            && ptr::eq(&*self.context, context)
    }
    /// Transfers another claim on this exact context. Retaining it prevents pixel sealing;
    /// callers cannot cache it as though the receipt owned no resources.
    #[must_use]
    pub fn context_claim(&self) -> CFRetained<CGContext> {
        self.context.clone()
    }
    #[must_use]
    pub fn receipt(&self) -> NativeMarkdownComposedViewportPaintReceipt {
        self.receipt
    }
    #[must_use]
    pub fn work(&self) -> NativeMarkdownComposedViewportPaintWork {
        self.work
    }
}
impl fmt::Debug for NativeMarkdownComposedViewportPaintCompletion {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("NativeMarkdownComposedViewportPaintCompletion")
            .field("context", &ptr::from_ref(&*self.context))
            .field("receipt", &self.receipt)
            .field("work", &self.work)
            .finish_non_exhaustive()
    }
}

/// States retain only original window indexes and the actual pen. A replacement Line,
/// Part, or character-based advance estimate cannot complete the original paint.
#[derive(Clone, Copy, Debug)]
enum Cursor {
    Line(usize),
    Part {
        line: usize,
        part: usize,
    },
    Glyphs {
        line: usize,
        part: usize,
        start: usize,
        pen_x: i64,
        pen_y: i64,
    },
    Finish {
        line: usize,
        part: usize,
        pen_x: i64,
        pen_y: i64,
    },
}

#[derive(Clone, Copy, PartialEq)]
struct ContextGeometry {
    transform: CGAffineTransform,
    clip: NSRect,
}
impl ContextGeometry {
    fn read(
        context: &CGContext,
    ) -> std::result::Result<Self, NativeMarkdownComposedViewportPaintCause> {
        let width = CGBitmapContextGetWidth(Some(context));
        let height = CGBitmapContextGetHeight(Some(context));
        let row = CGBitmapContextGetBytesPerRow(Some(context));
        // Accept only the private RGBA bitmap, rejecting window and PDF display lists.
        // The context's actual release callback protects the backing allocation's lifetime.
        if CGBitmapContextGetData(Some(context)).is_null()
            || CGBitmapContextGetBitsPerPixel(Some(context)) != 32
            || width == 0
            || height == 0
            || width.checked_mul(4).is_none_or(|bytes| bytes > row)
            || row
                .checked_mul(height)
                .is_none_or(|bytes| bytes > 32 * 1024 * 1024)
        {
            return Err(NativeMarkdownComposedViewportPaintCause::ContextChanged);
        }
        let transform = CGContext::ctm(Some(context));
        let clip = CGContext::clip_bounding_box(Some(context));
        let determinant = transform.a * transform.d - transform.b * transform.c;
        if ![
            transform.a,
            transform.b,
            transform.c,
            transform.d,
            transform.tx,
            transform.ty,
        ]
        .into_iter()
        .all(f64::is_finite)
            || !determinant.is_finite()
            || determinant == 0.0
            || !point(clip.origin)
            || !clip.size.width.is_finite()
            || !clip.size.height.is_finite()
            || clip.size.width < 0.0
            || clip.size.height < 0.0
            || !point(NSPoint::new(
                clip.origin.x + clip.size.width,
                clip.origin.y + clip.size.height,
            ))
        {
            return Err(NativeMarkdownComposedViewportPaintCause::ContextChanged);
        } // Reject invalid geometry instead of clamping it; later steps must match these exact values.
        Ok(Self { transform, clip })
    }
}

/// An unfinished candidate owns its bounded window and original context. Cancellation
/// and failure drop those claims; completion transfers the context into a completion ticket.
///
/// The caller exclusively owns writes and style state for the unpublished context;
/// other events must not change its colors or graphics stack. This layer also checks
/// context identity, CTM, and clip. Any mismatch invalidates the entire candidate,
/// and a completed tile alone does not authorize early display.
pub struct NativeMarkdownComposedViewportPaintSession {
    window: Rc<NativeMarkdownComposedViewport>,
    context: CFRetained<CGContext>,
    geometry: ContextGeometry,
    cursor: Cursor,
    receipt: NativeMarkdownComposedViewportPaintReceipt,
    total: NativeMarkdownComposedViewportPaintWork,
    native_draw_returned: bool,
}
impl fmt::Debug for NativeMarkdownComposedViewportPaintSession {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Diagnostics expose scalar progress only: no Native getter, Reader access,
        // or document materialization occurs while formatting Debug output.
        out.debug_struct("NativeMarkdownComposedViewportPaintSession")
            .field("cursor", &self.cursor)
            .field("receipt", &self.receipt)
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

impl NativeMarkdownComposedViewport {
    /// Starts the original cursor on this unpublished CPU candidate. The synchronous
    /// 4096-glyph limit does not apply, and construction paints no premature prefix.
    ///
    /// # Errors
    /// Rejects lost Source/Scene authority, the wrong thread, or a bitmap outside the
    /// bounded private RGBA contract; failure releases the supplied claims.
    pub fn begin_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintSession,
        NativeMarkdownComposedViewportPaintError,
    > {
        let result = (|| {
            self.require(current)?;
            if MainThreadMarker::new().is_none() {
                return Err(NativeMarkdownComposedViewportPaintCause::WrongThread);
            }
            let geometry = ContextGeometry::read(&context);
            // The original Source error takes precedence over context inspection;
            // current may synchronously revoke the original Reader.
            self.require(current)?;
            geometry
        })();
        let geometry = result.map_err(|cause| NativeMarkdownComposedViewportPaintError {
            cause,
            native_draw_returned: false,
        })?;
        Ok(NativeMarkdownComposedViewportPaintSession {
            window: self,
            context,
            geometry,
            cursor: Cursor::Line(0),
            receipt: NativeMarkdownComposedViewportPaintReceipt {
                glyphs: 0,
                batches: 0,
                missing_glyphs: 0,
            },
            total: NativeMarkdownComposedViewportPaintWork::default(),
            native_draw_returned: false,
        })
    }
}

impl NativeMarkdownComposedViewportPaintSession {
    fn require(
        &self,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintCause> {
        self.window.require(current)?;
        if MainThreadMarker::new().is_none() {
            return Err(NativeMarkdownComposedViewportPaintCause::WrongThread);
        }
        let actual = ContextGeometry::read(&self.context);
        // Reader leases end before Native/current calls. Preserve an original Source
        // failure before interpreting the inspected context geometry.
        self.window.require(current)?;
        if actual? != self.geometry {
            return Err(NativeMarkdownComposedViewportPaintCause::ContextChanged);
        }
        Ok(())
    }

    /// Consumes the state and returns either its sole Pending continuation or a
    /// completion receipt. Failure leaves no session that can replay the painted prefix.
    ///
    /// # Errors
    /// Invalid budgets, changed authority/bitmap, I/O failure, or inconsistent actual
    /// advances discard the candidate while preserving whether Native drawing returned.
    pub fn step(
        mut self,
        budget: NativeMarkdownComposedViewportPaintBudget,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintStep,
        NativeMarkdownComposedViewportPaintError,
    > {
        let mut work = NativeMarkdownComposedViewportPaintWork::default();
        let result = self.advance(budget, &mut work, current);
        match result {
            Ok(false) => Ok(NativeMarkdownComposedViewportPaintStep {
                work,
                outcome: NativeMarkdownComposedViewportPaintOutcome::Pending(self),
            }), // Resume this cursor alone; do not recapture Source, create a Scene, or replay batches.
            Ok(true) => {
                let completion = NativeMarkdownComposedViewportPaintCompletion {
                    key: self.window.key.clone(),
                    cell_window: match &self.window.domain {
                        LayoutDomain::Body => None,
                        LayoutDomain::TableCell { identity, .. } => Some(Rc::downgrade(identity)),
                    },
                    context: self.context.clone(),
                    receipt: self.receipt,
                    work: self.total,
                };
                // Release the window's business ownership. Completion and WritableTile
                // still explicitly own the context, so its release is not yet established.
                drop(self);
                Ok(NativeMarkdownComposedViewportPaintStep {
                    work,
                    outcome: NativeMarkdownComposedViewportPaintOutcome::Complete(completion),
                })
            }
            Err(cause) => Err(NativeMarkdownComposedViewportPaintError {
                cause,
                native_draw_returned: self.native_draw_returned,
            }), // Dropping self releases its claims; later cancellation cannot replace I/O failure or publish partial pixels.
        }
    }

    fn advance(
        &mut self,
        budget: NativeMarkdownComposedViewportPaintBudget,
        work: &mut NativeMarkdownComposedViewportPaintWork,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<bool, NativeMarkdownComposedViewportPaintCause> {
        self.require(current)?;
        if budget.glyphs > NATIVE_MARKDOWN_PAGED_GLYPH_BATCH
            || budget.metadata_records > METADATA_LIMIT
        {
            return Err(NativeMarkdownComposedViewportPaintCause::InvalidBudget);
        }
        loop {
            self.require(current)?;
            match self.cursor {
                Cursor::Line(line) => {
                    if line == self.window.lines.len() {
                        return Ok(true); // All actual lines and parts finished; no total-glyph limit cut off the tail.
                    }
                    if !self.take_metadata(budget, work)? {
                        return Ok(false);
                    }
                    self.cursor = if self.window.visible(self.window.lines[line].frame) {
                        Cursor::Part { line, part: 0 }
                    } else {
                        Cursor::Line(line + 1)
                    }; // Match the synchronous viewport test; tile bounds cannot approximate glyph ink and drop combining marks.
                }
                Cursor::Part { line, part } => {
                    if !self.take_metadata(budget, work)? {
                        return Ok(false);
                    }
                    let Some(original) = self.window.lines[line].parts.get(part) else {
                        self.cursor = Cursor::Line(line + 1);
                        continue; // Advance past the prepared final part, without treating a missing page as document EOF.
                    };
                    let facts = original.handle.facts();
                    self.cursor = if facts.kind == MarkdownComposedLinePartKind::Glyphs {
                        Cursor::Glyphs {
                            line,
                            part,
                            start: 0,
                            pen_x: facts.origin_x_26_6,
                            pen_y: facts.origin_y_26_6,
                        }
                    } else {
                        Cursor::Part {
                            line,
                            part: part + 1,
                        }
                    }; // Tabs and separators retain their layout contribution without invented glyphs or .notdef placeholders.
                }
                Cursor::Glyphs {
                    line,
                    part,
                    start,
                    pen_x,
                    pen_y,
                } => {
                    let original = &self.window.lines[line].parts[part];
                    let facts = original.handle.facts();
                    if start == facts.glyphs {
                        self.cursor = Cursor::Finish {
                            line,
                            part,
                            pen_x,
                            pen_y,
                        };
                        continue; // Even a zero-glyph part must pass the final advance check despite issuing no draw.
                    }
                    if work.glyphs == budget.glyphs {
                        return Ok(false); // Resume within this original part; its prefix cannot complete the tile.
                    }
                    let end = start
                        .saturating_add(budget.glyphs - work.glyphs)
                        .min(facts.glyphs);
                    let font = &self.window.palette[original.font];
                    let authority = font.native().core_font().authority();
                    CGContext::save_g_state(Some(&self.context));
                    let restore = Restore(&self.context);
                    CGContext::clip_to_rect(Some(&self.context), self.window.clip());
                    self.window.ink_style(original, &self.context);
                    let painted = paint_composed_glyphs(
                        &self.window.output,
                        &original.handle,
                        NativeMarkdownPagedGlyphPaintRequest {
                            source: &self.window.source(),
                            font_authority: &authority,
                            native_font: font.native(),
                            glyph_range: start..end,
                            origin: NativeMarkdownPagedGlyphOrigin {
                                baseline: self
                                    .window
                                    .part_baseline(&self.window.lines[line], original),
                                pen_x_26_6: pen_x,
                                pen_y_26_6: pen_y,
                            },
                        },
                        &self.context,
                        &|| self.window.require(current).is_ok(),
                    );
                    self.native_draw_returned |= painted
                        .as_ref()
                        .map_or_else(|error| error.native_draw_returned, |_| true);
                    // Restore this step's clip before checking the original geometry;
                    // the temporary clip does not become the candidate's new coordinate state.
                    drop(restore);
                    self.require(current)?;
                    let painted =
                        painted.map_err(NativeMarkdownComposedViewportPaintCause::Glyph)?;
                    work.glyphs += end - start;
                    work.batches += 1;
                    self.receipt.glyphs = checked_add(self.receipt.glyphs, end - start)?;
                    self.receipt.batches = checked_add(self.receipt.batches, 1)?;
                    self.receipt.missing_glyphs =
                        checked_add(self.receipt.missing_glyphs, painted.missing_glyphs)?;
                    self.total.glyphs = self.receipt.glyphs;
                    self.total.batches = self.receipt.batches;
                    self.cursor = Cursor::Glyphs {
                        line,
                        part,
                        start: end,
                        pen_x: painted.final_pen_x_26_6,
                        pen_y: painted.final_pen_y_26_6,
                    }; // Only the painter's actual receipt advances the pen, never glyph counts, average widths, or another line's origin.
                }
                Cursor::Finish {
                    line,
                    part,
                    pen_x,
                    pen_y,
                } => {
                    if !self.take_metadata(budget, work)? {
                        return Ok(false);
                    }
                    let facts = self.window.lines[line].parts[part].handle.facts();
                    if facts.origin_x_26_6.checked_add(facts.x_advance_26_6) != Some(pen_x)
                        || facts.origin_y_26_6.checked_add(facts.y_advance_26_6) != Some(pen_y)
                    {
                        return Err(NativeMarkdownComposedViewportPaintCause::InconsistentAdvance);
                    }
                    // Decorate once after all original glyphs and their final advance
                    // are verified; resumed glyph batches must not overpaint the same line.
                    self.window.paint_decorations(
                        &self.window.lines[line],
                        &self.window.lines[line].parts[part],
                        &self.context,
                        &mut self.native_draw_returned,
                        current,
                    )?;
                    self.cursor = Cursor::Part {
                        line,
                        part: part + 1,
                    };
                }
            }
        }
    }

    fn take_metadata(
        &mut self,
        budget: NativeMarkdownComposedViewportPaintBudget,
        work: &mut NativeMarkdownComposedViewportPaintWork,
    ) -> std::result::Result<bool, NativeMarkdownComposedViewportPaintCause> {
        if work.metadata_records == budget.metadata_records {
            // Metadata also suspends at the original cursor, bounding work even when
            // many control parts contain no glyphs.
            return Ok(false);
        }
        work.metadata_records += 1;
        self.total.metadata_records = checked_add(self.total.metadata_records, 1)?;
        Ok(true)
    }
}

fn checked_add(
    left: usize,
    right: usize,
) -> std::result::Result<usize, NativeMarkdownComposedViewportPaintCause> {
    left.checked_add(right)
        .ok_or(NativeMarkdownComposedViewportPaintCause::CounterExhausted)
}
