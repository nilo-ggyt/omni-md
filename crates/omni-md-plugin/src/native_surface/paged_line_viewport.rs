//! A bounded window of actual sealed lines, consuming original Line/Part, font, and Scene authority without parsing/shaping document text.

use std::{cell::RefCell, ops::Range, rc::Rc};

use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{CGColorSpace, kCGColorSpaceSRGB};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::{
    MARKDOWN_LINE_FONT_SLOTS, MarkdownComposedLine, MarkdownComposedLinePart,
    MarkdownComposedLines, MarkdownLayoutTextAuthority, MarkdownLineComposeError,
    MarkdownLineFontRegistration, MarkdownPresentationMode, MarkdownPresentationStyles,
    MarkdownTableCellDisplaySource,
};

use super::{
    paged_line_fonts::{NativeMarkdownLineFont, NativeMarkdownLineFontError},
    paged_line_geometry::NativeMarkdownLineGeometryError,
    paged_text_view::{NativeMarkdownPagedCharacterRect, NativeMarkdownPagedSceneKey},
};

mod caret;
mod table_cell;
pub use table_cell::NativeMarkdownComposedTableCell;
mod line_alignment;
pub(in crate::native_surface) use line_alignment::NativeMarkdownLineAlignmentMeasure;
pub use line_alignment::NativeMarkdownMeasuredLine;
mod geometry;
mod paint;
mod visual;
pub use caret::NativeMarkdownComposedCaretGeometry;
pub use geometry::NativeMarkdownComposedViewportHit;
pub(in crate::native_surface) use paint::{
    NativeMarkdownComposedViewportDecorationCursor, NativeMarkdownComposedViewportDecorationStep,
};
pub use paint::{
    NativeMarkdownComposedViewportPaintBudget, NativeMarkdownComposedViewportPaintCause,
    NativeMarkdownComposedViewportPaintCompletion, NativeMarkdownComposedViewportPaintError,
    NativeMarkdownComposedViewportPaintOutcome, NativeMarkdownComposedViewportPaintReceipt,
    NativeMarkdownComposedViewportPaintSession, NativeMarkdownComposedViewportPaintStep,
    NativeMarkdownComposedViewportPaintWork,
};
pub use visual::NativeMarkdownComposedVisualTarget;

/// Bound metadata for one prepared window; larger windows require producer batches rather than truncation presented as complete.
pub const NATIVE_MARKDOWN_COMPOSED_VIEWPORT_LINES: usize = 256;
pub const NATIVE_MARKDOWN_COMPOSED_VIEWPORT_PARTS: usize = 1_024;
/// Bound synchronous paint only; incremental sessions follow step budgets without truncating visible glyphs to this total.
pub const NATIVE_MARKDOWN_COMPOSED_VIEWPORT_GLYPHS: usize = 4_096;
const MAX_POINT: f64 = (1_u64 << 46) as f64;

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownComposedViewportError {
    #[error("原可见行窗口尚未准备此位置或超出本次固定配额")]
    Pending,
    #[error("原可见行窗口权限已经撤销")]
    Revoked,
    #[error("原行输出正在被使用，不能重入读取")]
    Busy,
    #[error("原窗口的实际几何无效或溢出")]
    InvalidGeometry,
    #[error("原窗口行不连续或不属于同一Source")]
    InvalidCoverage,
    #[error("原范围无效或切入尚不支持的文字边界")]
    InvalidRange,
    #[error("原palette没有这个part实际使用的字体")]
    FontMissing,
    #[error("无法创建原绘图的命名 sRGB 颜色空间")]
    ColorSpaceUnavailable,
    #[error("原行目录读取失败：{0}")]
    Composed(#[from] MarkdownLineComposeError),
    #[error("原表格显示来源失效：{0:?}")]
    Table(omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError),
    #[error("原字体注册或度量失效：{0}")]
    Font(#[from] NativeMarkdownLineFontError),
    #[error("原行文字几何失效：{0}")]
    Geometry(#[from] NativeMarkdownLineGeometryError),
}
type Result<T> = std::result::Result<T, NativeMarkdownComposedViewportError>;

struct Part {
    handle: MarkdownComposedLinePart,
    font: usize,
}

impl Part {
    /// Preparation/painting share this exact nonempty code-background predicate, without inferring area from glyph ink or adjacent styles.
    fn has_inline_code_background(&self) -> bool {
        let facts = self.handle.facts();
        !facts
            .presentation
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            && facts
                .presentation
                .contains(MarkdownPresentationStyles::INLINE_CODE)
            && facts.x_advance_26_6 != 0
    }
}

/// Each actual font use retains its original weak output registration; equal names or another live catalog cannot renew that authority.
struct FontUse {
    index: usize,
    registration: MarkdownLineFontRegistration,
}

struct Line {
    handle: MarkdownComposedLine,
    parts: Vec<Part>,
    // Share sealed-part lifetime; false proves the per-part background pass would emit no pixels.
    // Retain no text, glyphs, or extra resources, and bypass neither line-block backgrounds nor later Source/font health checks.
    has_inline_code_background: bool,
    frame: NSRect,
    baseline: CGPoint,
    ascent: f64,
    descent: f64,
    paragraph: super::preview_paragraph::PreviewParagraph,
    geometry: super::preview_paragraph::PreviewLineGeometry,
}

/// Strong claims cover the same sealed reader and bounded line/palette metadata, without text, glyph, or grapheme lists.
pub struct NativeMarkdownComposedViewport {
    output: Rc<RefCell<MarkdownComposedLines>>,
    key: NativeMarkdownPagedSceneKey,
    domain: LayoutDomain,
    palette: Box<[Rc<NativeMarkdownLineFont>]>,
    fonts: Box<[FontUse]>,
    lines: Vec<Line>,
    glyphs: usize,
    // Palette components are sRGB; the target bitmap space does not determine CGContext color-setter input space.
    // The window owns one named color space and returns it with its reader/font window, without per-part/glyph CGColor allocation.
    paint_space: CFRetained<CGColorSpace>,
}

/// Body follows original SceneKey; read-only Cell owns its actual display Source without manufacturing Body Input or a local document.
enum LayoutDomain {
    Body,
    TableCell {
        display: Rc<MarkdownTableCellDisplaySource>,
        alignment: omni_markdown_core::MarkdownTableAlignment,
        /// The complete measured content lane: horizontal clip limits pixels without changing soft-line alignment origin.
        width: f64,
        clip: NSRect,
        identity: Rc<()>,
    },
}

impl LayoutDomain {
    /// Active Cell takes alignment from the same session/Source TargetDomain; read-only Cell uses original raw authority.
    /// Ordinary Body retains paragraph geometry; full-table or scrolled-clip width cannot replace active Cell line width.
    fn line_origin_x(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        first_x: f64,
        inset: f64,
        extent: Option<&line_alignment::Extent>,
    ) -> Result<f64> {
        let (alignment, width) = match self {
            Self::Body => {
                let Some(alignment) = key.request.target.cell_alignment() else {
                    return Ok(first_x + inset);
                };
                // The same Native conversion supplies actual layout origin, without fallback Source/Preview constants.
                let layout_left = key.request.virtual_viewport.layout_origin_x
                    - key.request.virtual_viewport.document_origin.x;
                let padding = first_x - layout_left;
                (
                    alignment,
                    key.request.virtual_viewport.layout_width - padding * 2.0,
                )
            }
            Self::TableCell {
                alignment, width, ..
            } => (*alignment, *width),
        };
        Ok(first_x + line_alignment::alignment_origin_x(alignment, width, inset, extent)?)
    }

    /// Even under one outer Scene, Composer consumes its actual captured Body or display Source.
    fn source(&self, key: &NativeMarkdownPagedSceneKey) -> MarkdownLayoutTextAuthority {
        match self {
            Self::Body => key.source.clone(),
            Self::TableCell { display, .. } => display.authority(),
        }
    }

    /// Display Cell coordinates end at actual transformed length; canonical Input length grants no local-range authority.
    fn utf16_len(&self, key: &NativeMarkdownPagedSceneKey) -> usize {
        match self {
            Self::Body => key.request.input.utf16_len,
            Self::TableCell { display, .. } => display.source().utf16_len(),
        }
    }

    /// Clip is the same actual Cell-frame/Body-viewport intersection, not whole-table offscreen backing.
    fn clip(&self, key: &NativeMarkdownPagedSceneKey) -> NSRect {
        match self {
            Self::Body => key.request.viewport,
            Self::TableCell { clip, .. } => *clip,
        }
    }

    /// Return document borrows before Native/current reentry; a frozen display snapshot cannot extend old publication editing authority.
    fn require(&self, key: &NativeMarkdownPagedSceneKey) -> Result<()> {
        if let Self::TableCell { display, .. } = self {
            let owner = key
                .request
                .document
                .upgrade()
                .ok_or(NativeMarkdownComposedViewportError::Revoked)?;
            let document = owner
                .try_borrow()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
            display
                .require_for(&document)
                .map_err(NativeMarkdownComposedViewportError::Table)?;
        }
        Ok(())
    }
}

impl NativeMarkdownComposedViewport {
    /// Glyphs/code backgrounds/decorations share a part baseline; selection/caret retain actual line slots instead of following script glyphs outside them.
    fn part_baseline(&self, line: &Line, part: &Part) -> CGPoint {
        let mut baseline = line.baseline;
        baseline.y += super::text_attributes::native_markdown_baseline_offset(
            part.handle.facts().presentation,
            self.key.presentation_mode == omni_markdown_core::MarkdownPresentationMode::Source,
        )
        .unwrap_or(0.0);
        baseline
    }

    /// Prepare bounded consecutive Lines in the existing coalesced phase; first_top must come from the virtual-scroll producer's actual anchor.
    ///
    /// Use actual Body y-up coordinates; Source preserves Core Text arithmetic, while Preview/producer share same-font line-slot/paragraph results.
    ///
    /// # Errors
    /// Reject the whole candidate for line/part budget overflow, unsealed/invalid output, foreign handles, missing fonts, or invalid geometry.
    pub fn prepare(
        output: Rc<RefCell<MarkdownComposedLines>>,
        key: NativeMarkdownPagedSceneKey,
        palette: Vec<Rc<NativeMarkdownLineFont>>,
        handles: impl IntoIterator<Item = impl Into<NativeMarkdownMeasuredLine>>,
        first_top: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        Self::prepare_domain(
            output,
            key,
            LayoutDomain::Body,
            palette,
            handles,
            first_top,
            current,
        )
    }

    /// Only this module's concrete Cell consumer constructs local layout; display-local lines are never Body Input coordinates.
    fn prepare_domain(
        output: Rc<RefCell<MarkdownComposedLines>>,
        key: NativeMarkdownPagedSceneKey,
        domain: LayoutDomain,
        palette: Vec<Rc<NativeMarkdownLineFont>>,
        handles: impl IntoIterator<Item = impl Into<NativeMarkdownMeasuredLine>>,
        first_top: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        require_output(&output, &key, &domain, current)?;
        let source = domain.source(&key);
        let extent = domain.utf16_len(&key);
        let clip = domain.clip(&key);
        // Palette contains actual current Native fonts; empty/oversized palettes cannot reconstruct an old part's font by name.
        if palette.is_empty() || palette.len() > MARKDOWN_LINE_FONT_SLOTS {
            return Err(NativeMarkdownComposedViewportError::FontMissing);
        }
        // Original Scene/virtual-scroll geometry must be finite; do not clamp NaN/huge coordinates into seemingly valid viewports.
        if !point(first_top)
            || !rect(key.request.viewport)
            || !rect(key.request.frame_in_parent)
            || !key.request.bounds_width.is_finite()
            || key.request.bounds_width <= 0.0
            || key.request.bounds_width >= MAX_POINT
            || !point(key.request.virtual_viewport.document_origin)
        {
            return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
        }
        let palette = palette.into_boxed_slice(); // Do not retain excess Vec capacity supplied by the caller.
        let mut lines: Vec<Line> = Vec::new();
        let mut top = first_top.y;
        let mut parts = 0usize;
        let mut glyphs = 0usize;
        let mut fonts: Vec<FontUse> = Vec::new();
        // Accept this bounded window only; insufficient budget returns the whole candidate so the producer can prepare another batch.
        for measured in handles {
            let measured: NativeMarkdownMeasuredLine = measured.into();
            let (handle, alignment_extent) = measured.into_parts();
            if lines.len() == NATIVE_MARKDOWN_COMPOSED_VIEWPORT_LINES
                || handle.facts().parts
                    > NATIVE_MARKDOWN_COMPOSED_VIEWPORT_PARTS.saturating_sub(parts)
            {
                return Err(NativeMarkdownComposedViewportError::Pending);
            }
            if handle.facts().source_utf16.end > extent {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            }
            // Consecutive handles need adjacent actual source ranges and line ordinals; cached gaps cannot form a window.
            if let Some(previous) = lines.last()
                && (previous.handle.facts().source_utf16.end != handle.facts().source_utf16.start
                    || previous.handle.facts().ordinal.checked_add(1)
                        != Some(handle.facts().ordinal))
            {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            }
            let table_row =
                handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow;
            if table_row
                && (key.presentation_mode != MarkdownPresentationMode::InlinePreview
                    || !matches!(domain, LayoutDomain::Body))
            {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            } // Actual table-block lines belong only to Body flow, never literal Source rendering or another Cell's local layout.
            let mut prepared = Vec::with_capacity(handle.facts().parts);
            let mut has_inline_code_background = false;
            let glyphs_before = glyphs;
            let mut metrics =
                super::preview_paragraph::PreviewLineMetrics::new(key.presentation_mode);
            // Each part contains a weak handle/font index only; this Vec stores no glyphs, graphemes, or Source text.
            for ordinal in 0..handle.facts().parts {
                // Borrow the reader for pure Core reads only; font/Native/reentrant current calls run after that borrow ends.
                let part = {
                    let mut reader = output
                        .try_borrow_mut()
                        .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
                    reader.require(&source)?;
                    let part = reader
                        .part(&handle, ordinal)?
                        .ok_or(NativeMarkdownComposedViewportError::InvalidCoverage)?;
                    // Validate output/registration mapping before sealing; reuse afterward does not repeat empty glyph batches for every part.
                    reader.read_batch(&part, 0..0, &mut [])?;
                    part
                };
                require_output(&output, &key, &domain, current)?;
                // Original registration identifies the same font payload; another Native capture with equal name/size cannot impersonate it.
                let authority = part.font_registration().authority()?;
                let font = palette
                    .iter()
                    .position(|font| font.native().core_font().permits(&authority))
                    .ok_or(NativeMarkdownComposedViewportError::FontMissing)?;
                palette[font].require(part.font_registration())?;
                if !fonts.iter().any(|used| used.index == font) {
                    fonts.push(FontUse {
                        index: font,
                        registration: part.font_registration().clone(),
                    });
                } // This output catalog has one registration per live Core payload; do not retain duplicate weak registrations per part.
                if table_row {
                    if part.facts().kind
                        != omni_markdown_core::MarkdownComposedLinePartKind::TableRow
                        || part.facts().glyphs != 0
                    {
                        return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
                    }
                } else {
                    if !palette[font]
                        .native()
                        .accepts_shaping_size(part.facts().points)
                    {
                        // Diagnostics retain actual point-size mismatch during preparation, before later pixel errors could hide its font cause.
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        eprintln!(
                            "native_line_font_size_mismatch phase=viewport part_points={} native_points={} core_points={:?} kind={:?}",
                            part.facts().points,
                            palette[font].native().point_size(),
                            palette[font].native().core_font().shaping_point_size(),
                            part.facts().kind,
                        );
                        return Err(NativeMarkdownLineGeometryError::FontSizeMismatch.into());
                    } // Validate size with its sealed part; an actual TableRow's zero point size is outside the text-shaping contract.
                    let font_metrics = palette[font].metrics()?;
                    metrics
                        .add(
                            part.facts(),
                            font_metrics,
                            (key.presentation_mode
                                == omni_markdown_core::MarkdownPresentationMode::InlinePreview)
                                .then(|| palette[font].preview_metrics()),
                        )
                        .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?;
                }
                glyphs = glyphs
                    .checked_add(part.facts().glyphs)
                    .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?;
                let part = Part { handle: part, font };
                // Use already verified original part facts rather than rescan ordinary text for every later tile.
                // Negative advances may have background area; zero advances/hidden markers follow the same actual paint predicate.
                has_inline_code_background |= part.has_inline_code_background();
                prepared.push(part);
            }
            if glyphs.checked_sub(glyphs_before) != Some(handle.facts().glyphs) {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            }
            // Fonts on one line share an actual baseline; split leading above/below and accumulate subsequent tops from measured geometry.
            let geometry = if table_row {
                let height = handle.facts().y_advance_26_6 as f64 / 64.0;
                if !height.is_finite() || height <= 0.0 || height >= MAX_POINT {
                    return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
                }
                // This is completed block height for all actual row Cells, not a registered font's nominal 17 pt line slot.
                super::preview_paragraph::PreviewLineGeometry {
                    advance: height,
                    baseline_from_top: 0.0,
                    text_height: height,
                    before: 0.0,
                    after: 0.0,
                }
            } else {
                metrics
                    .geometry(handle.facts())
                    .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?
            };
            let height = geometry.advance;
            let baseline = CGPoint {
                x: domain.line_origin_x(
                    &key,
                    first_top.x,
                    metrics.paragraph.inset,
                    alignment_extent.as_ref(),
                )?,
                y: if key.presentation_mode == omni_markdown_core::MarkdownPresentationMode::Source
                {
                    top - metrics.ascent - metrics.leading / 2.0 // Literal Source preserves the original floating-point evaluation order.
                } else {
                    top - geometry.baseline_from_top
                },
            };
            let frame = NSRect::new(
                NSPoint::new(clip.origin.x, top - height),
                NSSize::new(clip.size.width, height),
            );
            if !point(baseline) || !rect(frame) {
                return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
            } // Empty font geometry/nonfinite height cannot manufacture a placeholder line frame.
            parts += prepared.len();
            top -= height;
            let (ascent, descent) = if table_row {
                (0.0, 0.0) // A block line has no text caret; Scene separately consumes actual Cell geometry.
            } else if key.presentation_mode
                == omni_markdown_core::MarkdownPresentationMode::InlinePreview
            {
                let ascent = geometry.baseline_from_top - geometry.before;
                (ascent, geometry.text_height - ascent)
            } else {
                (metrics.ascent, metrics.descent) // Source retains Core Text selection/hit height; Preview follows the same measured line slot.
            };
            lines.push(Line {
                handle,
                parts: prepared,
                has_inline_code_background,
                frame,
                baseline,
                ascent,
                descent,
                paragraph: metrics.paragraph,
                geometry,
            });
        }
        if lines.is_empty() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // A window without actual lines is not Ready; even empty documents require the real control-part/font producer.
        // SAFETY: Core Graphics' public named constant has static lifetime; capture color space only, without text reads/copies.
        let paint_space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        require_output(&output, &key, &domain, current)?; // After Native returns preserve actual Source error priority.
        let paint_space =
            paint_space.ok_or(NativeMarkdownComposedViewportError::ColorSpaceUnavailable)?;
        let candidate = Self {
            output,
            key,
            domain,
            palette,
            fonts: fonts.into_boxed_slice(),
            lines,
            glyphs,
            paint_space,
        };
        candidate.require(current)?; // Recheck original Source/Scene nonce before publication; partial metadata is not success.
        Ok(candidate)
    }

    #[must_use]
    pub fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }

    /// Return prepared-window range only; a sealed prefix/final visible line cannot stand in for document EOF.
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.lines[0].handle.facts().source_utf16.start
            ..self.lines[self.lines.len() - 1]
                .handle
                .facts()
                .source_utf16
                .end
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Return measured local line frames for the original producer's verifiable virtual-scroll anchors.
    #[must_use]
    pub fn line_frame(&self, ordinal_in_window: usize) -> Option<NSRect> {
        self.lines.get(ordinal_in_window).map(|line| line.frame)
    }

    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    /// Observe at most 256 installed original lines without advancing layout, reading text, or recomputing Native layout.
    pub(in crate::native_surface) fn probe_line_geometry(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Vec<super::preview_paragraph::PreviewLineObservation>> {
        self.require(current)?;
        let values = self
            .lines
            .iter()
            .map(|line| {
                let facts = line.handle.facts();
                super::preview_paragraph::PreviewLineObservation {
                    source: facts.source_utf16.clone(),
                    paragraph: facts.paragraph_utf16.clone(),
                    frame: line.frame,
                    baseline: NSPoint::new(line.baseline.x, line.baseline.y),
                    text_height: line.geometry.text_height,
                    before: line.geometry.before,
                    after: line.geometry.after,
                }
            })
            .collect();
        self.require(current)?;
        Ok(values)
    }

    /// Sealing already verified each part's size/output mapping; reuse validates actual shared resources without rescanning the whole image in glyph batches.
    ///
    /// Deduplicate weak registrations by actual font; Source/reader/publication/font revocation still applies around callbacks.
    /// New drawing validates glyphs through original part reads; zero-glyph pixel reuse cannot bypass resource failure.
    pub fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()> {
        self.require(current)?;
        for used in &self.fonts {
            let validation = (|| -> Result<()> {
                let font = &self.palette[used.index];
                font.require(&used.registration)?;
                font.metrics()?;
                Ok(())
            })();
            self.require(current)?; // Native/current may reenter; original Source failure precedes the newly obtained font error.
            validation?;
        }
        self.require(current)
    }

    /// Individual window use validates the same Source/reader/publication; full font validation stays at quantum boundaries.
    pub(in crate::native_surface) fn require(&self, current: &dyn Fn() -> bool) -> Result<()> {
        require_output(&self.output, &self.key, &self.domain, current)
    }

    /// All glyph/GCB/geometry reads follow actual layout Source; outer SceneKey still identifies the canonical Scene.
    fn source(&self) -> MarkdownLayoutTextAuthority {
        self.domain.source(&self.key)
    }

    /// Validate ranges in the actual layout domain; outer Body length cannot validate display-Cell local endpoints.
    fn utf16_len(&self) -> usize {
        self.domain.utf16_len(&self.key)
    }

    /// Painting, hit-testing, and selection share the same actual viewport intersection.
    fn clip(&self) -> NSRect {
        self.domain.clip(&self.key)
    }

    fn visible(&self, frame: NSRect) -> bool {
        intersects(frame, self.clip())
    }
}

/// Original Source failure precedes boolean revocation; release reader borrows around current so reentry cannot hide EIO.
fn require_output(
    output: &RefCell<MarkdownComposedLines>,
    key: &NativeMarkdownPagedSceneKey,
    domain: &LayoutDomain,
    current: &dyn Fn() -> bool,
) -> Result<()> {
    let before = current();
    {
        let reader = output
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        reader.require(&domain.source(key))?;
    }
    domain.require(key)?;
    let after = current(); // The first check may reenter; validate original Source again rather than restore authority through a new capture.
    {
        let reader = output
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        reader.require(&domain.source(key))?;
    }
    domain.require(key)?;
    if !before || !after || !key.nonce.same(&key.nonce) {
        return Err(NativeMarkdownComposedViewportError::Revoked);
    }
    Ok(())
}

fn point(value: CGPoint) -> bool {
    value.x.is_finite()
        && value.y.is_finite()
        && value.x.abs() < MAX_POINT
        && value.y.abs() < MAX_POINT
}
fn rect(value: NSRect) -> bool {
    point(value.origin)
        && value.size.width.is_finite()
        && value.size.height.is_finite()
        && value.size.width > 0.0
        && value.size.height > 0.0
        && point(NSPoint::new(
            value.origin.x + value.size.width,
            value.origin.y + value.size.height,
        ))
}
fn intersects(first: NSRect, second: NSRect) -> bool {
    first.origin.x < second.origin.x + second.size.width
        && second.origin.x < first.origin.x + first.size.width
        && first.origin.y < second.origin.y + second.size.height
        && second.origin.y < first.origin.y + first.size.height
}
