//! 原封存part的字素插入点；只读原GCB/cluster目录，不构造字符串或触发Native排版。

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

/// 插入点与真实Font度量共享同一baseline；外层行布局仍负责混合字体的整体行框。
#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownComposedCaret {
    pub source_utf16: usize,
    pub baseline: CGPoint,
    pub font_metrics: NativeMarkdownPagedFontMetricValues,
}

const MAX_EXACT: u64 = (1_u64 << 53) - 1;
const MAX_POINT: f64 = (1_u64 << 46) as f64;

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

/// 原Font/输出健康与可重入current分开；调用者不得把活RefCell借用带进这个函数。
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
        // 只在真实拒绝点记录原封存字号与绑定值；不读取正文或替不一致输出更换字体。
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
    // current可能发现实际Source故障；最后再核原健康，不用同名字体或后继scene恢复权限。
    let lines = output
        .try_borrow()
        .map_err(|_| NativeMarkdownLineGeometryError::Busy)?;
    lines.require(part.source_authority())?;
    Ok(())
}

/// 仅用于已经认证 part 的绘制包络；这个几何边缘不授予字素、选择或输入光标权限。
#[derive(Clone, Copy)]
pub(in crate::native_surface) enum ComposedPartSourceEdge {
    Start,
    End,
}

/// UBA 可在完整字素内部拆分整形项；绘制需要其真实边缘，公开 caret 仍必须经过原 GCB 目录。
/// 返回纯坐标而不是带源位置的 Caret，避免内部 scalar seam 被输入消费者误用。
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
    // 此端点只能从原 part 的实际起/终侧选择，调用方不能传裸 scalar 下标补签权限。
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

/// 只计算原part内一个已认证GCB边界的实际插入点，不同步扫描字素、读取段落或shape。
///
/// 完整GDEF与真实内部GCB数量一致时使用字体给出的caret。没有可匹配GDEF时，按原
/// cluster的真实总advance及GCB ordinal分配内部插入点；绝不按UTF16单元、bytes或窗口
/// 端点拆开组合字。此策略不授予安全断行权，也不会重用不安全的字形切片。
///
/// # Errors
/// 原权限、输出、字体、GCB或cluster不符以及坐标溢出均拒绝，不返回近似的别处光标。
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
    // 端点与内部位置都要经过原GCB目录；一个低代理项或组合字内部位置不能借端点捷径通过。
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
            (x, y, None, 0, 0) // 真part端点沿实际视觉方向解释；不查相邻part的cluster来猜换行侧。
        } else {
            if facts.kind != MarkdownComposedLinePartKind::Glyphs {
                return Err(NativeMarkdownLineGeometryError::InvalidPosition);
            } // 控制part只允许真实GCB两端；不从不存在的glyph cluster推导段内光标。
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
            }; // 多glyph cluster没有单个GDEF坐标原点；不能任取首glyph伪称整个cluster的插入点。
            // 到此均为原Reader的固定次数查询；不循环读取cluster内全部GCB或glyph。
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
    drop(lines); // 任何Native字体查询、CFRelease或current都不能保留这个Reader租约。
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
        require(output, part, font, current)?; // 先保留原Source/Font的真实失败，不能只报告bool回调失权。
        let carets = captured?;
        let positions = carets.positions_for(font.native(), glyph.glyph_id)?;
        if positions.len().checked_add(1) == Some(graphemes) && internal_rank != 0 {
            let index = if backward {
                graphemes - internal_rank - 1
            } else {
                internal_rank - 1
            };
            within_x = positions[index] + f64::from(glyph.x_offset) / 64.0;
        } // 不完整或无GDEF保留真实GCB ordinal插值，不按code unit平均，也不篡改原字形advance。
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
