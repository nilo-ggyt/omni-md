//! Grapheme carets in the original sealed part read only its grapheme/cluster directories, without
//! strings or Native layout.

use std::cell::RefCell;

use objc2_core_foundation::CGPoint;
use omni_markdown_core::{
    MarkdownComposedLinePart, MarkdownComposedLinePartKind, MarkdownComposedLines,
    MarkdownGlyphClusterAnswer, MarkdownLineComposeError, MarkdownShapedGlyph,
    MarkdownShapingDirection,
};

use super::{
    paged_font_geometry::{
        NativeMarkdownPagedFontGeometryError, NativeMarkdownPagedFontMetricValues,
        capture_paged_ligature_carets,
    },
    paged_line_fonts::{NativeMarkdownLineFont, NativeMarkdownLineFontError},
};

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownLineGeometryError {
    #[error("原行插入点请求已失权")]
    Revoked,
    #[error("原行输出仍被占用，不能重入读取")]
    Busy,
    #[error("位置不属于这个原part或不是完整字素边界")]
    InvalidPosition,
    #[error("原字形与字素目录不连续")]
    InconsistentClusters,
    #[error("原排版方向尚未由水平正文几何建模")]
    UnsupportedDirection,
    #[error("原part字号与实际Native字体不一致")]
    FontSizeMismatch,
    #[error("原插入点坐标非有限或溢出")]
    InvalidCoordinate,
    #[error("原行目录读取失败：{0}")]
    Composed(#[from] MarkdownLineComposeError),
    #[error("原行字体不一致：{0}")]
    Font(#[from] NativeMarkdownLineFontError),
    #[error("原字体插入点读取失败：{0}")]
    FontGeometry(#[from] NativeMarkdownPagedFontGeometryError),
}
type Result<T> = std::result::Result<T, NativeMarkdownLineGeometryError>;

/// Caret and actual font metrics share one baseline; outer line layout still owns the combined
/// rectangle for mixed fonts.
#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownComposedCaret {
    pub source_utf16: usize,
    pub baseline: CGPoint,
    pub font_metrics: NativeMarkdownPagedFontMetricValues,
}

const MAX_EXACT: u64 = (1_u64 << 53) - 1;
const MAX_POINT: f64 = (1_u64 << 46) as f64;

/// Combine bounded 26.6 and point coordinates without losing exact integer range or accepting nonfinite
/// output.
fn axis(origin: f64, fixed: i64, within: f64) -> Result<f64> {
    if !origin.is_finite()
        || fixed.unsigned_abs() > MAX_EXACT
        || !within.is_finite()
        || origin.abs() >= MAX_POINT
    {
        return Err(NativeMarkdownLineGeometryError::InvalidCoordinate);
    }
    let value = origin + fixed as f64 / 64.0 + within;
    if !value.is_finite() || value.abs() >= MAX_POINT {
        return Err(NativeMarkdownLineGeometryError::InvalidCoordinate);
    }
    Ok(value)
}

/// Separate original font/output health from reentrant current checks; callers must end live RefCell
/// borrows before entry.
fn require(
    output: &RefCell<MarkdownComposedLines>,
    part: &MarkdownComposedLinePart,
    font: &NativeMarkdownLineFont,
    current: &dyn Fn() -> bool,
) -> Result<()> {
    if !current() {
        return Err(NativeMarkdownLineGeometryError::Revoked);
    }
    font.require(part.font_registration())?;
    if !font.native().accepts_shaping_size(part.facts().points) {
        // Record sealed/bound sizes only at actual rejection, without reading text or replacing the
        // font for inconsistent output.
        #[cfg(feature = "native-markdown-runtime-probe")]
        eprintln!(
            "native_line_font_size_mismatch phase=geometry part_points={} native_points={} core_points={:?} kind={:?}",
            part.facts().points,
            font.native().point_size(),
            font.native().core_font().shaping_point_size(),
            part.facts().kind,
        );
        return Err(NativeMarkdownLineGeometryError::FontSizeMismatch);
    }
    {
        let mut lines = output
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownLineGeometryError::Busy)?;
        lines.require(part.source_authority())?;
        lines.read_batch(part, 0..0, &mut [])?;
    }
    if !current() {
        return Err(NativeMarkdownLineGeometryError::Revoked);
    }
    // current may discover actual Source failure; recheck original health without a same-name font or
    // successor Scene renewing authority.
    let lines = output
        .try_borrow()
        .map_err(|_| NativeMarkdownLineGeometryError::Busy)?;
    lines.require(part.source_authority())?;
    Ok(())
}

/// Geometry edges belong to a certified part's paint envelope only, granting no grapheme, selection, or
/// input-caret authority.
#[derive(Clone, Copy)]
pub(in crate::native_surface) enum ComposedPartSourceEdge {
    Start,
    End,
}

/// Bidi may split shaping items inside a grapheme. Painting needs their actual edges; public carets
/// still require the original grapheme directory.
/// Return coordinates without a Source-bearing caret so input consumers cannot misuse an internal
/// scalar seam.
pub(in crate::native_surface) fn composed_part_extent_edge(
    output: &RefCell<MarkdownComposedLines>,
    part: &MarkdownComposedLinePart,
    font: &NativeMarkdownLineFont,
    edge: ComposedPartSourceEdge,
    line_baseline: CGPoint,
    current: &dyn Fn() -> bool,
) -> Result<CGPoint> {
    require(output, part, font, current)?;
    let facts = part.facts();
    let backward = match facts.direction {
        MarkdownShapingDirection::LeftToRight => false,
        MarkdownShapingDirection::RightToLeft => true,
        MarkdownShapingDirection::TopToBottom | MarkdownShapingDirection::BottomToTop => {
            return Err(NativeMarkdownLineGeometryError::UnsupportedDirection);
        }
    };
    // Choose only the original part's actual start/end side; a bare scalar index cannot authorize an
    // edge.
    let advance = matches!(edge, ComposedPartSourceEdge::End) != backward;
    let x = facts
        .origin_x_26_6
        .checked_add(if advance { facts.x_advance_26_6 } else { 0 })
        .ok_or(NativeMarkdownLineGeometryError::InvalidCoordinate)?;
    let y = facts
        .origin_y_26_6
        .checked_add(if advance { facts.y_advance_26_6 } else { 0 })
        .ok_or(NativeMarkdownLineGeometryError::InvalidCoordinate)?;
    let baseline = CGPoint {
        x: axis(line_baseline.x, x, 0.0)?,
        y: axis(line_baseline.y, y, 0.0)?,
    };
    require(output, part, font, current)?;
    Ok(baseline)
}

/// Compute one certified grapheme-boundary caret within the original part without synchronous grapheme
/// scans, paragraph reads, or shaping.
///
/// Use actual font carets when complete GDEF matches the interior grapheme count. Otherwise distribute
/// interior carets by the original
/// cluster's actual total advance and grapheme ordinal, never by UTF-16 units, bytes, or viewport
/// edges splitting a combining sequence. This grants neither safe line-break authority nor unsafe
/// glyph-slice reuse.
///
/// # Errors
/// Reject authority/output/font/grapheme/cluster mismatch or coordinate overflow, without substituting
/// an approximate caret elsewhere.
pub fn composed_part_caret(
    output: &RefCell<MarkdownComposedLines>,
    part: &MarkdownComposedLinePart,
    font: &NativeMarkdownLineFont,
    source_utf16: usize,
    line_baseline: CGPoint,
    current: &dyn Fn() -> bool,
) -> Result<NativeMarkdownComposedCaret> {
    require(output, part, font, current)?;
    let facts = part.facts();
    let backward = match facts.direction {
        MarkdownShapingDirection::LeftToRight => false,
        MarkdownShapingDirection::RightToLeft => true,
        MarkdownShapingDirection::TopToBottom | MarkdownShapingDirection::BottomToTop => {
            return Err(NativeMarkdownLineGeometryError::UnsupportedDirection);
        }
    };
    if source_utf16 < facts.source_utf16.start || source_utf16 > facts.source_utf16.end {
        return Err(NativeMarkdownLineGeometryError::InvalidPosition);
    }
    let metrics = font.metrics()?;
    // Endpoints and interior positions both pass the original grapheme directory; low surrogates or
    // combining interiors cannot bypass it.
    let mut lines = output
        .try_borrow_mut()
        .map_err(|_| NativeMarkdownLineGeometryError::Busy)?;
    let ordinal = lines
        .grapheme_ordinal(source_utf16)?
        .ok_or(NativeMarkdownLineGeometryError::InvalidPosition)?;
    let (relative_x, relative_y, glyph, internal_rank, graphemes) =
        if source_utf16 == facts.source_utf16.start || source_utf16 == facts.source_utf16.end {
            let end_side = (source_utf16 == facts.source_utf16.end) != backward;
            let (x, y) = if end_side {
                (facts.x_advance_26_6, facts.y_advance_26_6)
            } else {
                (0, 0)
            };
            (x, y, None, 0, 0) // Interpret actual part endpoints in their real visual direction, without guessing wrap side from neighboring clusters.
        } else {
            if facts.kind != MarkdownComposedLinePartKind::Glyphs {
                return Err(NativeMarkdownLineGeometryError::InvalidPosition);
            } // Control parts permit actual grapheme endpoints only; nonexistent glyph clusters cannot provide interior carets.
            let cluster = match lines.cluster(part, source_utf16)? {
                MarkdownGlyphClusterAnswer::Known(Some(value)) => value,
                _ => return Err(NativeMarkdownLineGeometryError::InconsistentClusters),
            };
            let first = match lines.cluster(part, facts.source_utf16.start)? {
                MarkdownGlyphClusterAnswer::Known(Some(value)) => value,
                _ => return Err(NativeMarkdownLineGeometryError::InconsistentClusters),
            };
            let low = lines
                .grapheme_ordinal(cluster.source_utf16.start)?
                .ok_or(NativeMarkdownLineGeometryError::InconsistentClusters)?;
            let high = lines
                .grapheme_ordinal(cluster.source_utf16.end)?
                .ok_or(NativeMarkdownLineGeometryError::InconsistentClusters)?;
            let graphemes = high
                .checked_sub(low)
                .filter(|count| *count != 0 && (*count as u128) <= u128::from(MAX_EXACT))
                .ok_or(NativeMarkdownLineGeometryError::InconsistentClusters)?;
            let rank = ordinal
                .checked_sub(low)
                .filter(|rank| *rank < graphemes)
                .ok_or(NativeMarkdownLineGeometryError::InconsistentClusters)?;
            let prefix = |value: i64, base: i64, advance: i64, total: i64| -> Result<i64> {
                let logical = value
                    .checked_sub(base)
                    .ok_or(NativeMarkdownLineGeometryError::InvalidCoordinate)?;
                if backward {
                    total
                        .checked_sub(logical)
                        .and_then(|value| value.checked_sub(advance))
                        .ok_or(NativeMarkdownLineGeometryError::InvalidCoordinate)
                } else {
                    Ok(logical)
                }
            };
            let x = prefix(
                cluster.prefix_x_26_6,
                first.prefix_x_26_6,
                cluster.x_advance_26_6,
                facts.x_advance_26_6,
            )?;
            let y = prefix(
                cluster.prefix_y_26_6,
                first.prefix_y_26_6,
                cluster.y_advance_26_6,
                facts.y_advance_26_6,
            )?;
            let glyph = if cluster.glyphs.len() == 1 && rank != 0 {
                let mut glyphs = [MarkdownShapedGlyph {
                    glyph_id: 0,
                    source_utf16_cluster: 0,
                    flags: 0,
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                }];
                lines.read_batch(part, cluster.glyphs.clone(), &mut glyphs)?;
                Some(glyphs[0])
            } else {
                None
            }; // A multi-glyph cluster has no single GDEF origin; its first glyph cannot certify carets for the whole cluster.
            // These are fixed-count original-reader queries, without iterating all graphemes or glyphs
            // inside a cluster.
            let fraction = if backward {
                (graphemes - rank) as f64 / graphemes as f64
            } else {
                rank as f64 / graphemes as f64
            };
            (
                x,
                y,
                Some((
                    glyph,
                    cluster.x_advance_26_6,
                    cluster.y_advance_26_6,
                    fraction,
                )),
                rank,
                graphemes,
            )
        };
    drop(lines); // End this reader lease before Native font queries, CFRelease, or reentrant current checks.
    require(output, part, font, current)?;
    let (mut within_x, within_y) = if let Some((_, x, y, fraction)) = glyph {
        if x.unsigned_abs() > MAX_EXACT || y.unsigned_abs() > MAX_EXACT {
            return Err(NativeMarkdownLineGeometryError::InvalidCoordinate);
        }
        (x as f64 * fraction / 64.0, y as f64 * fraction / 64.0)
    } else {
        (0.0, 0.0)
    };
    if let Some((Some(glyph), _, _, _)) = glyph {
        let font_current = || require(output, part, font, current).is_ok();
        let captured = capture_paged_ligature_carets(font.native(), glyph.glyph_id, &font_current);
        require(output, part, font, current)?; // Preserve actual Source/font failure before a boolean callback's ordinary revocation.
        let carets = captured?;
        let positions = carets.positions_for(font.native(), glyph.glyph_id)?;
        if positions.len().checked_add(1) == Some(graphemes) && internal_rank != 0 {
            let index = if backward {
                graphemes - internal_rank - 1
            } else {
                internal_rank - 1
            };
            within_x = positions[index] + f64::from(glyph.x_offset) / 64.0;
        } // Absent/incomplete GDEF keeps actual grapheme-ordinal interpolation, without code-unit averaging or altered glyph advance.
    }
    let origin_x = facts
        .origin_x_26_6
        .checked_add(relative_x)
        .ok_or(NativeMarkdownLineGeometryError::InvalidCoordinate)?;
    let origin_y = facts
        .origin_y_26_6
        .checked_add(relative_y)
        .ok_or(NativeMarkdownLineGeometryError::InvalidCoordinate)?;
    let baseline = CGPoint {
        x: axis(line_baseline.x, origin_x, within_x)?,
        y: axis(line_baseline.y, origin_y, within_y)?,
    };
    require(output, part, font, current)?;
    Ok(NativeMarkdownComposedCaret {
        source_utf16,
        baseline,
        font_metrics: metrics,
    })
}
