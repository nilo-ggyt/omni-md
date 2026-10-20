//! 固定短表的独立 Native 字形定位；先核原浮点排版，再逐字形转为公开 26.6 合同。

use super::*;
use objc2_core_text::CTRunStatus;
use objc2_foundation::{NSCharacterSet, NSNotFound, NSStringCompareOptions};
use omni_markdown_core::MarkdownTableAlignment;

/// 只保同一真实 run 的纯值；没有 Font、CTLine、Source 或候选字形的强引用。
pub(super) struct Glyph {
    cluster: usize,
    position: NSPoint,
    // 同一次原 run 的 base advance 与相对 origin，不从画出的 glyph x 顺序反推。
    advance: NSSize,
    origin: NSPoint,
    run_origin: f64,
    run: usize,
}

/// 原 TextKit 行限定输入，CoreText 只交付同一属性来源的实际 glyph、位置与推进。
/// 这些位置必须在下面重建原 TextKit GCB 框；另一条排版策略不能借量化获得通过。
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
    // SAFETY: 原属性串与行范围仍由活 TextKit 图持有；不读取候选字体或重新分配正文来源。
    let setter = unsafe { CTTypesetter::with_attributed_string_and_options(text.as_ref(), None) }
        .ok_or("短表原属性没有真实 typesetter")?;
    // SAFETY: local 完整落在同一实际属性串内；CTLine 在本同步栈内归还。
    let native = unsafe { setter.line(CFRange::new(local.location as _, local.length as _)) };
    // SAFETY: 不可变 CTLine 保有下方全部 run，记录只以 Copy 值离开本函数。
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
        // SAFETY: ordinal 来自活 run 数组，Font 与 run 的所有借用均不离开这个循环。
        let raw = unsafe { runs.value_at_index(ordinal as _) };
        let run = NonNull::new(raw.cast_mut().cast::<CTRun>()).ok_or("短表实际 run 为空")?;
        // SAFETY: 原 run 的公开元数据只读；非水平矩阵不能冒充当前短表的几何合同。
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
        // SAFETY: 固定数组均覆盖 used 项；所有来源和坐标在数组交付后逐项核验。
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
        // 首 origin 也可能非零；相减才是实际 run 起点，随后逐 glyph 重建原 Native position。
        // 此处的 base prefix 不含 mark 相对定位，不能拿 run 间 gap 当作 glyph offset。
        let run_origin = NSPoint::new(positions[0].x - origins[0].x, positions[0].y - origins[0].y);
        let mut pen = NSSize::ZERO;
        // SAFETY: kCTFontAttributeName 的公开值为原 run 实际 CTFont；包括系统真正选取的 fallback。
        let font = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
        let font = NonNull::new(font.cast_mut().cast::<CTFont>()).ok_or("短表实际 run 缺 Font")?;
        // SAFETY: run 保活 Font；只核真实 glyph 编号域，不按名字或候选字体推断。
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
            // 候选整簇试次之后，最终原 TextKit 行也必须保留同一实际 Font；不接受静默 cascade。
            // 身份、轴和字号来自原属性与真实 run，绝不读取产品已经选出的字体来生成参考。
            // SAFETY: 此字体仍由本轮原 run 字典持活；只借入同步属性核对，不跨原生资源释放边界。
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
    // 只按实际 run 起点排序，run 内保持 Native 原 glyph 消费序；mark 的 ink x 可以越过 base。
    // 同点保持原 run 顺序，不按 source 或画出的位置把 RTL/同 cluster 多 glyph 改成 LTR。
    result.sort_by(|left, right| left.run_origin.total_cmp(&right.run_origin));
    // CTLine 的零点可位于视觉行内部：RTL 的逻辑尾空白真实排在负 x，不能强令首 run 为零。
    // 仍逐 run 严格核连续；这个起点来自原 Native 事实，不吸收后继 run 的任何 gap。
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

/// 两个末端代表不同的真实合同：原 Native 字符尾策略只证明旧浮点图，完整 EGC 决定最终定位。
struct AlignmentEnds {
    native_alignment_utf16: usize,
    whole_egc_utf16: usize,
}

/// 原 Native 的非空白搜索不重新分组，固定 EGC 的整簇空白判断也不借待测 Core trim 结果。
/// Prepend+首空格会在 Native 尾策略里排除该空格，在最终完整 EGC 中则必须共同保留。
fn alignment_ends(source: &str, line: &OriginalLine) -> AlignmentEnds {
    let native = NSString::from_str(source);
    let nonwhite = NSCharacterSet::whitespaceAndNewlineCharacterSet().invertedSet();
    // 该公开 API 只查询原真实软行范围内最后一个非空白字符；范围已由原图证明属于短 source。
    // 此来源仅用于核实际 Native baseline，绝不把这个标量端点签成产品可编辑 GCB 边界。
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
        // NSRange.location 是 NSUInteger，而绑定的 NSNotFound 是正的 NSIntegerMax。
        // macOS 二者位宽相同；这里只转换系统哨兵，不转换或截断实际来源下标。
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

/// 公开定位是逐 glyph 最近值/半值离零；不是给浮点累计坐标 round 一次，也不按字符数修偏。
fn tick(value: f64) -> f64 {
    (value * 64.0).round() / 64.0
}

/// 仅由原 Native 实际 glyph 得出同一完整 GCB 的浮点与量化包络。
/// 每次重走固定≤256项纯值，避免为这几个短串引入目录、第二排版器或可变缓存。
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
        // 只认原 Native 确实存在的 basepen=0 字形边缘。RTL 尾空白位于它之前，须减去此前
        // 各 glyph 已分别量化的 prefix；不能将整个负 run_origin round 一次或选择最近边缘。
        if glyph.position.x - glyph.origin.x == 0.0 && zero_prefix.is_none() {
            zero_prefix = Some(positioned);
        }
        // 相对定位直接取原 origin；不再拿全线 pen 逆推而吞掉 run 间错位。
        // base advance 与相对 origin 分别量化，随后继续同一个 Native glyph 消费序。
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

/// 先证浮点原图与 alignment pivot，再输出独立26.6字形参考；原 raw frame 始终保留。
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
    // 原 Native 对齐范围只为证明其浮点图；Prepend 的旧尾空白政策不能反过来切产品完整簇。
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
        ); // 明示两个合同的真实差别，不将旧 Native 尾策略伪称为最终完整 EGC 的逐位位置。
    }
    for grapheme in &mut line.graphemes {
        let bounds = bounds(&glyphs, &grapheme.source)?;
        // Standard 是同一真实 GCB 的 typographic 框，Selection 另有软行尾延展/容器裁切政策。
        // 先核未改写的 Standard，不能拿已裁到 content.right 的 Selection 宽度当完整 advance。
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
        // 当前查询只含一个完整 GCB，并已证明落在单一原软行内；None 没有请求 head/tail
        // 扩展，不能仅因 GCB.end 恰等于软行.end 就猜它的选择还继续到下一行。
        // 原 Selection 可等于 Standard 的真实容器裁切；raw 仍保留，不用候选几何拟合。
        let intersection = native_left.max(0.0)..native_right.min(width);
        let outside = intersection.end <= intersection.start;
        // 完全离屏的 Native 空选择保留原 Standard 起点，只将宽度置零；它不是落在W的caret。
        // 最终 q 字形仍保持完整范围，visibility 继续由它与产品 clip 的真实交集决定。
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
        // 只有前置原图证明完成才签量化框；Center 的半差保留浮点，不能把 baseline 再 round。
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
