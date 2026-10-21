//! Independent Native glyph positions for fixed short tables verify original floating layout before
//! per-glyph conversion to public 26.6 coordinates.

use super::*;
use objc2_core_text::CTRunStatus;
use objc2_foundation::{NSCharacterSet, NSNotFound, NSStringCompareOptions};
use omni_markdown_core::MarkdownTableAlignment;

/// Retain pure facts from the same actual run, without font, CTLine, Source, or candidate-glyph
/// ownership.
pub(super) struct Glyph {
    cluster: usize,
    position: NSPoint,
    // Use original run base advances and relative origins, without inferring them from painted glyph-X
    // order.
    advance: NSSize,
    origin: NSPoint,
    run_origin: f64,
    run: usize,
}

/// The actual TextKit line bounds input; Core Text supplies actual glyphs, positions, and advances from
/// that same attributed Source.
/// These positions must reconstruct original TextKit grapheme boxes below; quantization cannot excuse a
/// different floating layout.
pub(super) fn capture(
    line: &objc2_app_kit::NSTextLineFragment,
    element_start: usize,
    expected: &objc2_foundation::NSAttributedString,
) -> Result<Vec<Glyph>, String> {
    let local = line.characterRange();
    if local.length == 0 {
        return Ok(Vec::new());
    }
    let end = local
        .location
        .checked_add(local.length)
        .ok_or("短表原行末端溢出")?;
    let text = line.attributedString();
    if end > text.length() || local.length > 256 {
        return Err("短表字形参考超出原有限行范围".into());
    }
    // SAFETY: The live TextKit graph retains original attributes/range; read no candidate font and
    // allocate no document Source.
    let setter = unsafe { CTTypesetter::with_attributed_string_and_options(text.as_ref(), None) }
        .ok_or("短表原属性没有真实 typesetter")?;
    // SAFETY: Local range fits the same actual attributed string; CTLine returns on this synchronous
    // stack.
    let native = unsafe { setter.line(CFRange::new(local.location as _, local.length as _)) };
    // SAFETY: The immutable CTLine retains every run below; only Copy facts leave this function.
    let (range, count, runs) = unsafe {
        (
            native.string_range(),
            native.glyph_count(),
            native.glyph_runs(),
        )
    };
    if range != CFRange::new(local.location as _, local.length as _) || !(1..=256).contains(&count)
    {
        return Err("短表真实 glyph 域未覆盖同一原行".into());
    }
    let mut result = Vec::with_capacity(count as usize);
    let mut ids = [0_u16; 256];
    let mut indices = [0_isize; 256];
    let mut positions = [NSPoint::ZERO; 256];
    let mut advances = [NSSize::ZERO; 256];
    let mut origins = [NSPoint::ZERO; 256];
    let mut covered = [false; 256];
    for ordinal in 0..runs.len() {
        // SAFETY: Ordinal belongs to the live run array; all font/run borrows end within this
        // iteration.
        let raw = unsafe { runs.value_at_index(ordinal as _) };
        let run = NonNull::new(raw.cast_mut().cast::<CTRun>()).ok_or("短表实际 run 为空")?;
        // SAFETY: Read original public run metadata only; a nonhorizontal matrix cannot satisfy this
        // short-table geometry contract.
        let (used, matrix, status, attributes, run_range) = unsafe {
            (
                run.as_ref().glyph_count(),
                run.as_ref().text_matrix(),
                run.as_ref().status(),
                run.as_ref().attributes(),
                run.as_ref().string_range(),
            )
        };
        let used = usize::try_from(used).map_err(|_| "短表 run 字形域无效")?;
        if used > 256
            || result.len() + used > count as usize
            || (matrix.a, matrix.b, matrix.c, matrix.d, matrix.tx, matrix.ty)
                != (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
        {
            return Err("短表实际 run 不属于本次有限水平字形域".into());
        }
        if used == 0 {
            continue;
        }
        let run_start = usize::try_from(run_range.location).map_err(|_| "短表 run 来源起点无效")?;
        let run_length = usize::try_from(run_range.length).map_err(|_| "短表 run 来源长度无效")?;
        let run_end = run_start
            .checked_add(run_length)
            .ok_or("短表 run 来源末端溢出")?;
        if run_start < local.location || run_end > end || run_length == 0 {
            return Err("短表 run 未落在同一原行完整来源内".into());
        }
        let covered = &mut covered[run_start - local.location..run_end - local.location];
        if covered.iter().any(|covered| *covered) {
            return Err("短表实际 run 重复覆盖原行来源".into());
        }
        covered.fill(true);
        // SAFETY: Every fixed array holds used entries; validate all Source coordinates and positions
        // after delivery.
        unsafe {
            run.as_ref().glyphs(
                CFRange::new(0, used as _),
                NonNull::new(ids.as_mut_ptr()).unwrap(),
            );
            run.as_ref().string_indices(
                CFRange::new(0, used as _),
                NonNull::new(indices.as_mut_ptr()).unwrap(),
            );
            run.as_ref().positions(
                CFRange::new(0, used as _),
                NonNull::new(positions.as_mut_ptr()).unwrap(),
            );
            run.as_ref().base_advances_and_origins(
                CFRange::new(0, used as _),
                advances.as_mut_ptr(),
                origins.as_mut_ptr(),
            );
        }
        // The first relative origin may be nonzero; subtract it for actual run start, then reconstruct
        // each original Native glyph position.
        // This base prefix excludes relative mark positioning; gaps between runs cannot masquerade as
        // glyph offsets.
        let run_origin = NSPoint::new(positions[0].x - origins[0].x, positions[0].y - origins[0].y);
        let mut pen = NSSize::ZERO;
        // SAFETY: The public font-key value is the original run actual CTFont, including any actual
        // system fallback.
        let font = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
        let font = NonNull::new(font.cast_mut().cast::<CTFont>()).ok_or("短表实际 run 缺 Font")?;
        // SAFETY: The run retains Font; check actual glyph-ID domain without inference from names or
        // candidate fonts.
        let domain = usize::try_from(unsafe { font.as_ref().glyph_count() })
            .map_err(|_| "短表实际 Font 的 glyph 域无效")?;
        for index in 0..used {
            let source = usize::try_from(indices[index]).map_err(|_| "短表实际 cluster 无效")?;
            let position = positions[index];
            let advance = advances[index];
            let origin = origins[index];
            if source < run_start
                || source >= run_end
                || ids[index] == 0
                || usize::from(ids[index]) >= domain
                || ![
                    position.x,
                    position.y,
                    advance.width,
                    advance.height,
                    origin.x,
                    origin.y,
                    run_origin.x,
                    run_origin.y,
                ]
                .into_iter()
                .all(f64::is_finite)
                || advance.width < 0.0
                || advance.height != 0.0
                || run_origin.y != 0.0
                || (position.x - run_origin.x - pen.width - origin.x).abs() > 1.0 / 64.0
                || (position.y - run_origin.y - pen.height - origin.y).abs() > 1.0 / 64.0
            {
                return Err("短表实际 glyph、水平位置或来源无效".into());
            }
            // After the whole-grapheme trial, the final original TextKit line must still use that
            // actual font without silent cascade.
            // Identity, axes, and size come from original attributes/actual runs, never the product
            // selected font as reference input.
            // SAFETY: This original run dictionary retains the font; synchronous attribute comparison
            // crosses no Native release boundary.
            let actual_font = unsafe { font.as_ref() };
            table_fonts::require_run_font(expected, element_start + source, actual_font)?;
            if index != 0 && !status.contains(CTRunStatus::NonMonotonic) {
                let reversed = indices[index] < indices[index - 1];
                let advanced = indices[index] > indices[index - 1];
                if (status.contains(CTRunStatus::RightToLeft) && advanced)
                    || (!status.contains(CTRunStatus::RightToLeft) && reversed)
                {
                    return Err("短表原 run 的真实方向与 cluster 顺序不一致".into());
                }
            }
            result.push(Glyph {
                cluster: element_start + source,
                position,
                advance,
                origin,
                run_origin: run_origin.x,
                run: ordinal,
            });
            pen.width += advance.width;
            pen.height += advance.height;
        }
    }
    if result.len() != count as usize || covered[..local.length].iter().any(|covered| !covered) {
        return Err("短表最终 Native run 未完整覆盖原行来源和字形域".into());
    }
    // Sort actual run starts only, preserving Native glyph order within each run because mark ink may
    // extend before its base.
    // Keep original order for tied starts; Source/ink sorting must not convert RTL or multiglyph
    // clusters into LTR.
    result.sort_by(|left, right| left.run_origin.total_cmp(&right.run_origin));
    // CTLine zero may lie inside the visual line: actual RTL trailing whitespace can have negative X,
    // so the first run need not start at zero.
    // Still verify actual per-run continuity; the original Native start cannot absorb gaps from later
    // runs.
    let (mut run, mut pen) = (None, result.first().map_or(0.0, |glyph| glyph.run_origin));
    for glyph in &result {
        if run != Some(glyph.run) {
            if (glyph.run_origin - pen).abs() > 1.0 / 64.0 {
                return Err(format!(
                    "短表 Native 视觉 run 的真实 base pen 不连续：line={local:?} previous_run={run:?} run={} cluster={} actual_origin={} expected_basepen={pen} position={:?} base={:?} origin={:?}",
                    glyph.run,
                    glyph.cluster,
                    glyph.run_origin,
                    glyph.position,
                    glyph.advance,
                    glyph.origin
                ));
            }
            run = Some(glyph.run);
        }
        pen += glyph.advance.width;
    }
    Ok(result)
}

/// Keep two distinct ends: Native character-tail policy verifies original floating layout, while whole
/// graphemes determine final positioning.
struct AlignmentEnds {
    native_alignment_utf16: usize,
    whole_egc_utf16: usize,
}

/// Native nonwhitespace search does not regroup text; whole-grapheme whitespace checks do not borrow
/// the Core trim result under test.
/// Native tail policy can exclude the space after Prepend, but the final complete grapheme must retain
/// both together.
fn alignment_ends(source: &str, line: &OriginalLine) -> AlignmentEnds {
    let native = NSString::from_str(source);
    let nonwhite = NSCharacterSet::whitespaceAndNewlineCharacterSet().invertedSet();
    // The public API finds the final nonwhitespace character inside the actual soft-line range already
    // certified by the original graph.
    // Use this scalar endpoint for Native-baseline verification only, without certifying a
    // product-editable grapheme boundary.
    let last_nonwhite = native.rangeOfCharacterFromSet_options_range(
        &nonwhite,
        NSStringCompareOptions::BackwardsSearch,
        NSRange::new(line.source.start, line.source.len()),
    );
    let native_alignment_utf16 = if last_nonwhite.location == NSNotFound as usize {
        line.source.start
    } else {
        last_nonwhite.location + last_nonwhite.length
    };
    let mut end = line.source.end;
    for grapheme in line.graphemes.iter().rev() {
        let text =
            native.substringWithRange(NSRange::new(grapheme.source.start, grapheme.source.len()));
        // NSRange.location uses NSUInteger while the binding NSNotFound is positive NSIntegerMax.
        // Their widths match on macOS; convert the system sentinel only, without converting/truncating
        // actual Source indices.
        if text.rangeOfCharacterFromSet(&nonwhite).location != NSNotFound as usize {
            break;
        }
        end = grapheme.source.start;
    }
    AlignmentEnds {
        native_alignment_utf16,
        whole_egc_utf16: end,
    }
}

/// Public positioning rounds each glyph to nearest with ties away from zero, rather than rounding an
/// accumulated coordinate or adjusting by character count.
fn tick(value: f64) -> f64 {
    (value * 64.0).round() / 64.0
}

/// Derive floating and quantized envelopes of one complete grapheme from original actual Native glyphs
/// only.
/// Revisit at most 256 fixed pure records rather than adding directories, another layout engine, or
/// mutable caches for short fixtures.
struct GlyphBounds {
    native_left: f64,
    native_right: f64,
    positioned_left: f64,
    positioned_right: f64,
}

fn bounds(glyphs: &[Glyph], source: &Range<usize>) -> Result<GlyphBounds, String> {
    let mut positioned = 0.0;
    let mut zero_prefix = None;
    let (mut left, mut right, mut qleft, mut qright) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for glyph in glyphs {
        // Require an actual original Native base-pen-zero glyph edge. RTL trailing whitespace lies
        // before it, so subtract earlier
        // individually quantized glyph prefixes instead of rounding the whole negative run origin or
        // choosing a nearest edge.
        if glyph.position.x - glyph.origin.x == 0.0 && zero_prefix.is_none() {
            zero_prefix = Some(positioned);
        }
        // Use original relative origin directly without reverse-deriving it from a whole-line pen that
        // could hide inter-run misalignment.
        // Quantize base advance and relative origin separately while retaining the same Native glyph
        // consumption order.
        let advance = tick(glyph.advance.width);
        let origin = positioned + tick(glyph.origin.x);
        if source.contains(&glyph.cluster) {
            left = left.min(glyph.position.x);
            right = right.max(glyph.position.x + glyph.advance.width);
            qleft = qleft.min(origin);
            qright = qright.max(origin + advance);
        }
        positioned += advance;
    }
    if ![left, right, qleft, qright].into_iter().all(f64::is_finite) {
        return Err(format!(
            "短表原 glyph 没有覆盖完整 Native GCB：source={source:?}"
        ));
    }
    let zero_prefix = zero_prefix
        .ok_or_else(|| format!("短表原 glyph 没有真实零 basepen 边缘：source={source:?}"))?;
    Ok(GlyphBounds {
        native_left: left,
        native_right: right,
        positioned_left: qleft - zero_prefix,
        positioned_right: qright - zero_prefix,
    })
}

/// Verify original floating geometry/alignment pivot before producing independent 26.6 reference
/// positions, preserving raw frames.
pub(super) fn position(
    source: &str,
    line: &mut OriginalLine,
    width: f64,
    alignment: MarkdownTableAlignment,
) -> Result<(), String> {
    if line.graphemes.is_empty() {
        return Ok(());
    }
    let glyphs = std::mem::take(&mut line.table_glyphs);
    let ends = alignment_ends(source, line);
    // The Native alignment extent verifies floating layout only; its Prepend tail policy cannot split
    // product whole graphemes.
    let native_alignment_extent = if ends.native_alignment_utf16 == line.source.start {
        0.0..0.0
    } else {
        let actual = bounds(&glyphs, &(line.source.start..ends.native_alignment_utf16))?;
        actual.native_left..actual.native_right
    };
    let mut whole_egc_extent = f64::INFINITY..f64::NEG_INFINITY;
    for grapheme in &line.graphemes {
        let bounds = bounds(&glyphs, &grapheme.source)?;
        if grapheme.source.start < ends.whole_egc_utf16 {
            whole_egc_extent.start = whole_egc_extent.start.min(bounds.positioned_left);
            whole_egc_extent.end = whole_egc_extent.end.max(bounds.positioned_right);
        }
    }
    if ends.whole_egc_utf16 == line.source.start {
        whole_egc_extent = 0.0..0.0;
    }
    let factor = match alignment {
        MarkdownTableAlignment::Left => 0.0,
        MarkdownTableAlignment::Center => 0.5,
        MarkdownTableAlignment::Right => 1.0,
    };
    let origin = if factor == 0.0 {
        0.0
    } else {
        (width - (native_alignment_extent.end - native_alignment_extent.start)) * factor
            - native_alignment_extent.start
    };
    let qorigin = if factor == 0.0 {
        0.0
    } else {
        (width - (whole_egc_extent.end - whole_egc_extent.start)) * factor - whole_egc_extent.start
    };
    if !origin.is_finite() || !qorigin.is_finite() || (origin - line.baseline.x).abs() > 1.0 / 64.0
    {
        return Err(format!(
            "短表 Native 原浮点对齐 pivot 不自洽：alignment={alignment:?} source={:?} baseline={:?} reconstructed={origin} native_alignment_end={} native_alignment_extent={native_alignment_extent:?} whole_egc_end={} whole_egc_extent={whole_egc_extent:?} width={width}",
            line.source, line.baseline, ends.native_alignment_utf16, ends.whole_egc_utf16
        ));
    }
    if ends.native_alignment_utf16 != ends.whole_egc_utf16 {
        eprintln!(
            "native_table_original_alignment_policy source={:?} native_alignment_end={} native_alignment_extent={native_alignment_extent:?} native_origin={origin} whole_egc_end={} whole_egc_extent={whole_egc_extent:?} positioned_origin={qorigin}",
            line.source, ends.native_alignment_utf16, ends.whole_egc_utf16
        ); // Report the actual policy difference without treating old Native tail rules as final complete-grapheme positions.
    }
    for grapheme in &mut line.graphemes {
        let bounds = bounds(&glyphs, &grapheme.source)?;
        // Standard supplies the same actual grapheme typographic box; Selection separately applies
        // line-end extension/container clipping.
        // Verify untouched Standard first; a Selection clipped to content.right cannot stand in for
        // complete advance.
        let native_left = origin + bounds.native_left;
        let native_right = origin + bounds.native_right;
        if (native_left - grapheme.typographic_frame.origin.x).abs() > 1.0 / 64.0
            || (native_right - native_left - grapheme.typographic_frame.size.width).abs()
                > 1.0 / 64.0
        {
            return Err(format!(
                "短表 Native 原浮点 glyph/GCB 框不自洽：source={:?} selection={:?} standard={:?} reconstructed_x={native_left} reconstructed_width={}",
                grapheme.source,
                grapheme.frame,
                grapheme.typographic_frame,
                native_right - native_left
            ));
        }
        // This query contains one complete grapheme certified within one actual soft line; no head/tail
        // extension, so equal grapheme/soft-line end does not imply selection continues into another
        // line.
        // Actual Selection may equal Standard after container clipping; preserve raw observations
        // without fitting to candidate geometry.
        let intersection = native_left.max(0.0)..native_right.min(width);
        let outside = intersection.end <= intersection.start;
        // An entirely offscreen Native empty selection keeps original Standard start with zero width,
        // rather than a caret clamped at container width.
        // Final quantized glyphs retain complete extents; actual intersection with the product clip
        // still determines visibility.
        let selected = if outside {
            native_left..native_left
        } else {
            intersection
        };
        if (selected.start - grapheme.frame.origin.x).abs() > 1.0 / 64.0
            || (selected.end - selected.start - grapheme.frame.size.width).abs() > 1.0 / 64.0
            || (outside
                && ((grapheme.frame.origin.y - grapheme.typographic_frame.origin.y).abs()
                    > 1.0 / 64.0
                    || (grapheme.frame.size.height - grapheme.typographic_frame.size.height).abs()
                        > 1.0 / 64.0))
        {
            return Err(format!(
                "短表 Native 原选择框不符真实容器政策：source={:?} selection={:?} standard={:?} expected={selected:?} width={width} outside={outside}",
                grapheme.source, grapheme.frame, grapheme.typographic_frame
            ));
        }
        // Issue a quantized box after original-graph checks only; retain the floating half-difference
        // for Center without rounding baseline again.
        grapheme.positioned_frame = Some(NSRect::new(
            NSPoint::new(qorigin + bounds.positioned_left, grapheme.frame.origin.y),
            NSSize::new(
                bounds.positioned_right - bounds.positioned_left,
                grapheme.frame.size.height,
            ),
        ));
    }
    Ok(())
}
