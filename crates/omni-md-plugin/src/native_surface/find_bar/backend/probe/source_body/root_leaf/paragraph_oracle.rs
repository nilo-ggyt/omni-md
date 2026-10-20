//! 短合成样本的独立标准 TextKit 2 几何；不调用分页段落策略、不读取用户文稿。

use super::*;
use crate::native_surface::{
    list_markers::MarkdownListGlyphRole,
    text_attributes::{
        MarkdownFontSelectionCache, MarkdownTextAttributePalette, MarkdownTextAttributeVariant,
        build_markdown_text_attributes_with_row_height, native_markdown_document_fixed_font,
        native_markdown_document_font,
    },
};
use objc2::{AnyThread, rc::Weak, runtime::Bool};
use objc2_app_kit::{
    NSFont, NSLayoutManager, NSTextContainer, NSTextContentStorage, NSTextLayoutFragment,
    NSTextLayoutFragmentEnumerationOptions, NSTextLayoutFragmentState, NSTextLayoutManager,
    NSTextLayoutManagerSegmentOptions, NSTextLayoutManagerSegmentType, NSTextRange, NSTextStorage,
    NSTextStorageObserving,
};
use objc2_core_foundation::CFRange;
use objc2_core_text::{CTFont, CTRun, CTTypesetter, kCTFontAttributeName};
use objc2_foundation::{NSMutableAttributedString, NSString};
use omni_markdown_core::MarkdownViewportPresentation;
use std::{ops::Range, ptr::NonNull};

#[path = "paragraph_oracle/table_fonts.rs"]
mod table_fonts;
#[path = "paragraph_oracle/table_positions.rs"]
mod table_positions;

/// 只有源范围和真实几何离开原生池；不会把字体、属性正文或 TextKit 图留给分页候选。
pub(super) struct OriginalLine {
    pub source: Range<usize>,
    pub paragraph: Range<usize>,
    pub frame: NSRect,
    pub baseline: NSPoint,
    pub paragraph_frame: NSRect,
    pub before: f64,
    pub after: f64,
    pub graphemes: Vec<OriginalGrapheme>,
    glyphs: Vec<OriginalGlyph>,
    // 只有短表需要这些原 Native 纯值；浮点自洽与26.6转换完成后在原池内归还。
    table_glyphs: Vec<table_positions::Glyph>,
}

/// 固定短表的实际 Native 组合字符选区；只留公开来源与 Copy 矩形，不保留系统排版图。
pub(super) struct OriginalGrapheme {
    pub source: Range<usize>,
    /// 原 Selection 框保留系统的容器裁切/软行尾延展，不能冒充完整字形推进。
    pub frame: NSRect,
    /// 同一原 GCB 的 Standard typographic 框；仅用于先证原字形浮点图自洽。
    typographic_frame: NSRect,
    /// 逐实际glyph量化后的定位参考；raw frame仍保留原Native排版政策，不混淆两种合同。
    pub positioned_frame: Option<NSRect>,
}

/// 候选关闭后保留的实际封存纯值；来源范围只指定待比对象，不参与参考字体或软行的生成。
pub(super) struct PartAdvance {
    pub source: Range<usize>,
    pub advance_26_6: i64,
}

/// 原 TextKit 行关闭后只消费已采纯值；不再创建第二个会另算 CJK 行末策略的 CTLine。
/// parts 仅指定待比的原来源边界，既不提供字体、advance，也不决定 Native glyph 的划分。
pub(super) fn part_advances(
    source: &str,
    lines: &[OriginalLine],
    parts: &[PartAdvance],
) -> Result<Vec<i64>, String> {
    if source.len() > 2048 || parts.len() > 256 {
        return Err("独立字形推进只允许固定≤2KiB短例与≤256个实际 part".into());
    }
    let length = source.encode_utf16().count();
    let mut totals = vec![0_i64; parts.len()];
    for (index, part) in parts.iter().enumerate() {
        if part.source.is_empty() || part.source.len() > 256 || part.source.end > length {
            return Err("实际 part 不能完整落在独立固定短源的256单元范围内".into());
        }
        let (mut offset, mut start, mut end) = (0, false, false);
        for scalar in source.chars() {
            start |= offset == part.source.start;
            end |= offset == part.source.end;
            if part.source.contains(&offset) && scalar.is_control() {
                return Err("实际 Glyph part 含控制，不能略过后冒充完整字形推进".into());
            }
            offset += scalar.len_utf16();
        }
        end |= offset == part.source.end;
        if !start || !end {
            return Err("实际 part 的端点切开了固定源的完整 Unicode scalar".into());
        }
        let mut covered = [false; 256];
        for glyph in lines.iter().flat_map(|line| &line.glyphs).filter(|glyph| {
            part.source.start < glyph.source.end && glyph.source.start < part.source.end
        }) {
            // 唯一真实 glyph 的 cluster 跨界时直接拒绝；不能将连字按字符比例分摊给候选 part。
            if glyph.source.start < part.source.start || glyph.source.end > part.source.end {
                return Err(format!(
                    "原实际 glyph 跨 part 边界，不能按字符比例分摊：glyph_source={:?} part={:?}",
                    glyph.source, part.source
                ));
            }
            let from = glyph.source.start - part.source.start;
            let to = glyph.source.end - part.source.start;
            if covered[from..to].iter().any(|covered| *covered) {
                return Err("原实际 glyph 来源重复，不能重复累计推进".into());
            }
            covered[from..to].fill(true);
            totals[index] = totals[index]
                .checked_add(glyph.advance_26_6)
                .ok_or("原完整 part 整数推进溢出")?;
        }
        if covered[..part.source.len()].iter().any(|covered| !covered) {
            return Err("原 TextKit glyph 没有精确覆盖待比 part".into());
        }
    }
    Ok(totals)
}

/// 只有原 Native 的 glyph 来源和实际 TextKit 边缘都取得后才留下这个纯值。
/// 不保属性串、字体或 line fragment，候选与旧 Native 图仍按原顺序完全关闭。
struct OriginalGlyph {
    source: Range<usize>,
    advance_26_6: i64,
}

/// CTTypesetter 仅证明同 Font 的真实 glyph/cluster 边界；推进一律读取已排好的 TextKit 行。
/// 这区分了两种实际 Native 行末 CJK 策略，不把独立 CTLine 的七点差值拿来修正原行。
fn line_glyphs(
    line: &objc2_app_kit::NSTextLineFragment,
    element_start: usize,
    parts: &[PartAdvance],
) -> Result<Vec<OriginalGlyph>, String> {
    let local = line.characterRange();
    let end = local
        .location
        .checked_add(local.length)
        .ok_or("旧行范围溢出")?;
    let canonical = element_start + local.location..element_start + end;
    if !parts
        .iter()
        .any(|part| part.source.start < canonical.end && canonical.start < part.source.end)
    {
        return Ok(Vec::new()); // 无实际 glyph 消费者的普通几何例不创建额外的 CTLine 或 metadata。
    }
    let attributed = line.attributedString();
    if end > attributed.length() || local.length > 256 || local.length == 0 {
        return Err("旧 TextKit 行不在原属性串内或超过256个固定来源单元".into());
    }
    // SAFETY: getter 返回的完整源属性串由真实 TextKit 行保活；没有 substring 或候选属性。
    let typesetter =
        unsafe { CTTypesetter::with_attributed_string_and_options(attributed.as_ref(), None) }
            .ok_or("真实 TextKit 属性没有取得有限边界参考")?;
    // SAFETY: range 是同一原属性串中当前真实行的来源，CTLine 不负责提供推进期望。
    let native = unsafe { typesetter.line(CFRange::new(local.location as _, local.length as _)) };
    // SAFETY: 不可变 line 保活全部 runs，以下只收完整 glyph 的来源和实际字体身份。
    let (range, count, runs) = unsafe {
        (
            native.string_range(),
            native.glyph_count(),
            native.glyph_runs(),
        )
    };
    let count = usize::try_from(count).map_err(|_| "Native glyph 边界数量无效")?;
    if range != CFRange::new(local.location as _, local.length as _) || !(1..=256).contains(&count)
    {
        return Err("Native glyph 边界不覆盖同一真实 TextKit 行或超过256字形".into());
    }
    let mut origins = Vec::with_capacity(count);
    let mut glyphs = [0_u16; 256];
    let mut indices = [0_isize; 256];
    for ordinal in 0..runs.len() {
        let ordinal = isize::try_from(ordinal).map_err(|_| "原 run 下标无效")?;
        // SAFETY: ordinal 来自活数组，CTRun 指针的借用不离开当前真实 TextKit 枚举回调。
        let raw = unsafe { runs.value_at_index(ordinal) };
        let run = NonNull::new(raw.cast_mut().cast::<CTRun>()).ok_or("原 run 为空")?;
        // SAFETY: 原 run 强归 native/数组所有，只读真实字形域与属性，不查候选 Font。
        let (used, attributes, matrix) = unsafe {
            (
                run.as_ref().glyph_count(),
                run.as_ref().attributes(),
                run.as_ref().text_matrix(),
            )
        };
        let used = usize::try_from(used).map_err(|_| "原 run 字形数量无效")?;
        if used > 256
            || origins.len() + used > count
            || (matrix.a, matrix.b, matrix.c, matrix.d, matrix.tx, matrix.ty)
                != (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
        {
            return Err("原 glyph 边界域或矩阵不属于水平短行".into());
        }
        if used == 0 {
            continue; // 真空 run 不读取栈数组，也不为它合成来源边界。
        }
        // SAFETY: CoreText 字体键的公开值类型是 CTFont，字典贯穿下方所有借用。
        let actual = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
        let actual = NonNull::new(actual.cast_mut().cast::<CTFont>()).ok_or("原 run 缺 Font")?;
        // SAFETY: 数量已核≤256，两张固定数组完整保存本 run 的原 glyph 与 cluster。
        unsafe {
            run.as_ref().glyphs(
                CFRange::new(0, used as _),
                NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            );
            run.as_ref().string_indices(
                CFRange::new(0, used as _),
                NonNull::new(indices.as_mut_ptr()).unwrap(),
            );
        }
        // SAFETY: 实际 run 字体贯穿本回调，glyph_count 只读取同一个 Native glyph 域。
        let domain = usize::try_from(unsafe { actual.as_ref().glyph_count() })
            .map_err(|_| "原 run Font 的 glyph 域无效")?;
        for glyph in 0..used {
            let from = usize::try_from(indices[glyph]).map_err(|_| "原 glyph cluster 无效")?;
            if from < local.location || from >= end || usize::from(glyphs[glyph]) >= domain {
                return Err("原 glyph 不在同一真实行来源或字体字形域内".into());
            }
            // SAFETY: from 是当前真实属性串内的原 cluster，返回对象由本栈持有到身份核验结束。
            let expected = unsafe {
                attributed.attribute_atIndex_effectiveRange(
                    NSFontAttributeName,
                    from,
                    std::ptr::null_mut(),
                )
            }
            .ok_or("原 TextKit 字形来源没有 Font 属性")?;
            let expected = expected
                .downcast_ref::<NSFont>()
                .ok_or("原 Font 属性类型不是 NSFont")?;
            let expected: &CTFont = expected.as_ref();
            // SAFETY: 两边来自同一个真实 TextKit 属性来源；相同名称或字号不足以代替 CFEqual。
            if unsafe { actual.as_ref() } != expected {
                return Err("独立 glyph 边界参考更换了原 TextKit Font，不能据它切分".into());
            }
            origins.push(from);
        }
    }
    if origins.len() != count {
        return Err("原 glyph 边界参考没有交付完整真实字形域".into());
    }
    origins.sort_unstable();
    // 同 cluster 多 glyph 不能借字符边缘分别恢复它们的推进；不把 partial 参考当完整 oracle。
    if origins.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("原 cluster 含多个 glyph，公开字符边缘不足以逐 glyph 证明推进".into());
    }
    let bounds = line.typographicBounds();
    let glyph_origin = line.glyphOrigin();
    let first_index = isize::try_from(local.location).map_err(|_| "旧行首索引无效")?;
    let first = line.locationForCharacterAtIndex(first_index);
    if !first.x.is_finite()
        || !first.y.is_finite()
        || !bounds.size.width.is_finite()
        || (first.x - glyph_origin.x).abs() > 1.0 / 64.0
    {
        return Err(format!(
            "原 TextKit 字符边缘未与真实 glyphOrigin 自洽：range={local:?} first={first:?} glyph_origin={glyph_origin:?} width={}",
            bounds.size.width
        ));
    }
    let terminal = first.x + bounds.size.width;
    let mut result = Vec::new();
    for (ordinal, &from) in origins.iter().enumerate() {
        let to = origins.get(ordinal + 1).copied().unwrap_or(end);
        let source = element_start + from..element_start + to;
        if !parts
            .iter()
            .any(|part| part.source.start < source.end && source.start < part.source.end)
        {
            continue;
        }
        let index = isize::try_from(from).map_err(|_| "原 glyph 字符索引无效")?;
        let point = line.locationForCharacterAtIndex(index);
        if let Some(previous) = ordinal.checked_sub(1).and_then(|index| origins.get(index)) {
            let previous = line.locationForCharacterAtIndex(
                isize::try_from(*previous).map_err(|_| "原前一 glyph 索引无效")?,
            );
            // 即使待比 part 从这里才开始，也须排除实际 Native 同一 glyph 的内部位置。
            // 只检查下一边缘会漏掉“part 起点已切进连字、其末端却是合法边界”的情况。
            if !previous.x.is_finite() || previous.x >= point.x {
                return Err(format!(
                    "原 TextKit glyph 起点与前驱同原点或逆序，不能授予 part 边界：source={source:?} previous={previous:?} point={point:?}"
                ));
            }
        }
        let next = if to == end {
            terminal // 原 typographicBounds 是真实行的末端；不用另一 CTLine 的行末定位代替。
        } else {
            line.locationForCharacterAtIndex(
                isize::try_from(to).map_err(|_| "原下一 glyph 索引无效")?,
            )
            .x
        };
        if !point.x.is_finite() || !next.is_finite() || next <= point.x {
            return Err(format!(
                "原 TextKit glyph 边界同原点、逆序或无效，不能插值分摊：source={source:?} from={point:?} next_x={next}"
            ));
        }
        let advance = ((next - point.x) * 64.0).round();
        if !advance.is_finite() || advance < f64::from(i32::MIN) || advance > f64::from(i32::MAX) {
            return Err("原 TextKit glyph 推进不能表示为水平26.6".into());
        }
        result.push(OriginalGlyph {
            source,
            advance_26_6: advance as i64,
        });
    }
    Ok(result)
}

/// 统一擦除的弱票仍指向刚创建的真实对象；它不保留任何系统排版 owner。
fn native_weak(value: &AnyObject) -> Weak<AnyObject> {
    Weak::from(value)
}

/// 系统 default-line API 与旧 TextKit 2 的独立短串同时给出真实标量，不用一次字号的失败反推 ceil 规则。
pub(super) fn font_defaults() -> Result<(), String> {
    // 行标量对照沿实际文稿 palette，不把任意 UI 字体的特殊默认策略算成产品字体合同。
    let base = native_markdown_document_fixed_font(14.0).map_err(|cause| cause.to_string())?;
    let palette = MarkdownTextAttributePalette::new(base).map_err(|cause| cause.to_string())?;
    for (name, font) in [
        ("mono14", palette.fixed_font.clone()),
        (
            "mono13.5",
            native_markdown_document_fixed_font(13.5).map_err(|cause| cause.to_string())?,
        ),
        (
            "document15",
            native_markdown_document_font(15.0).map_err(|cause| cause.to_string())?,
        ),
        ("heading28", palette.heading_fonts[0].clone()),
        ("heading24", palette.heading_fonts[1].clone()),
        ("hidden0.1", palette.hidden_font.clone()),
    ] {
        let (height, baseline, weak) = autoreleasepool(|_| {
            let manager = NSLayoutManager::new();
            (
                manager.defaultLineHeightForFont(&font),
                manager.defaultBaselineOffsetForFont(&font),
                native_weak(&manager),
            ) // 只查询系统字体行标量，没有 storage、container、正文或第二个产品布局图。
        });
        if weak.load().is_some() {
            return Err(format!("{name}: 系统字体行标量查询结束后 manager 尚未释放"));
        }
        let lines = measure("alpha beta gamma delta\n", 72.0, &font, &[])?;
        for (ordinal, line) in lines.iter().enumerate() {
            eprintln!(
                "native_paragraph_font_defaults font={name} points={} ascent={} descent={} leading={} default_height={height} default_baseline={baseline} ordinal={ordinal} source={:?} frame={:?} baseline={:?}",
                font.pointSize(),
                font.ascender(),
                -font.descender(),
                font.leading(),
                line.source,
                line.frame,
                line.baseline
            );
        } // 每个字体都沿固定72pt真实换行，细字号与隐藏字形不共享一个由当前候选推来的期望。
    }
    Ok(())
}

/// 原完整 Parser 只接固定短例；字体和属性仍使用未修改的旧 Native 分支。
fn attributed(
    source: &str,
    font: &Retained<NSFont>,
) -> Result<Retained<NSMutableAttributedString>, String> {
    let mut presentation = MarkdownViewportPresentation::default();
    let _ = presentation.set_mode(MarkdownPresentationMode::InlinePreview);
    presentation
        .rebuild(1, 0..source.encode_utf16().count(), source)
        .map_err(|cause| cause.to_string())?;
    let palette =
        MarkdownTextAttributePalette::new(font.clone()).map_err(|cause| cause.to_string())?;
    let string = NSString::from_str(source);
    let result =
        NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &string);
    let mut index = 0;
    for scalar in source.chars() {
        let styles = presentation
            .query(index)
            .ok_or("旧完整短样本缺少真实属性覆盖")?
            .styles;
        let resolved =
            MarkdownFontSelectionCache::resolve_uncached(styles, scalar, &palette, false)
                .map_err(|cause| cause.to_string())?;
        if matches!(scalar, '中' | '😀') {
            // 固定短例的原属性选字事实只含Font元数据；与候选相互独立，不输出正文或借它改变预期。
            eprintln!(
                "native_paragraph_original_font source={index} scalar={:x} face={} points={} ascent={} descent={} leading={}",
                scalar as u32,
                resolved.font.fontName(),
                resolved.font.pointSize(),
                resolved.font.ascender(),
                -resolved.font.descender(),
                resolved.font.leading(),
            );
        }
        let attributes = build_markdown_text_attributes_with_row_height(
            styles,
            &resolved.font,
            &palette,
            MarkdownTextAttributeVariant {
                source_mode: false,
                table_row_height: None,
                list_role: MarkdownListGlyphRole::Literal,
            },
        );
        // SAFETY: 只给固定短串中这一个完整 scalar 设置原属性，不跨代理项或越过实际 Native 长度。
        unsafe { result.addAttributes_range(&attributes, NSRange::new(index, scalar.len_utf16())) };
        index += scalar.len_utf16();
    }
    Ok(result)
}

/// 真实标准布局只接≤2KiB短 oracle；所有关系在同一池内对称拆开后，才返回纯值记录。
pub(super) fn measure(
    source: &str,
    width: f64,
    font: &Retained<NSFont>,
    parts: &[PartAdvance],
) -> Result<Vec<OriginalLine>, String> {
    measure_text(source, width, font, parts, None, &[])
}

/// 原正式表格属性设置真正的 NSParagraphStyle 对齐；尾空白是否参与由 Native 原排版决定。
/// 与普通段落共用下面同一套真实装配/拆卸，不复制 oracle 图或在期望侧重新计算 offset。
/// 短表对齐消费每个真实组合字符框，包括行尾空白；普通段落入口不附带这些查询。
pub(super) fn measure_table_cell_graphemes(
    source: &str,
    width: f64,
    font: &Retained<NSFont>,
    alignment: omni_markdown_core::MarkdownTableAlignment,
    grapheme_boundaries: &[usize],
) -> Result<Vec<OriginalLine>, String> {
    // 分组来自固定短例的独立 UAX 端点，不由待测 Core 的输出或 Foundation 本机分组政策生成。
    // Native 只消费同一原范围的实际排版几何；下方同时证明这些范围完整拼成原 source。
    measure_text(
        source,
        width,
        font,
        &[],
        Some(alignment),
        grapheme_boundaries,
    )
}

fn measure_text(
    source: &str,
    width: f64,
    font: &Retained<NSFont>,
    parts: &[PartAdvance],
    alignment: Option<omni_markdown_core::MarkdownTableAlignment>,
    grapheme_boundaries: &[usize],
) -> Result<Vec<OriginalLine>, String> {
    if source.len() > 2048 || parts.len() > 256 {
        return Err("独立标准排版只允许固定短样本和有限part，禁止送入长段落或用户正文".into());
    }
    let (values, native) = autoreleasepool(|_| -> Result<_, String> {
        let text = attributed(source, font)?;
        if alignment.is_some() {
            // 只有固定短表使用完整 EGC 的同候选覆盖合同；普通段落仍观察原逐 scalar TextKit 政策。
            // 此处先核声明范围再选字，后续 Native 几何不能借本机旧 Foundation 分组改写 UAX 边界。
            table_fonts::apply(source, font, &text, grapheme_boundaries)?;
        }
        let wrapped = alignment.map(|alignment| {
            crate::native_surface::text_attributes::native_markdown_wrapped_table_cell_text(
                &text, alignment,
            )
        });
        let storage = NSTextStorage::new();
        storage.setAttributedString(wrapped.as_deref().unwrap_or(&text));
        let content = NSTextContentStorage::new();
        let manager = NSTextLayoutManager::new();
        let container =
            NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(width, f64::MAX));
        container.setLineFragmentPadding(0.0); // 两侧用同一实际正文列宽；不让系统额外 5pt padding 改写 wrap 对照。
        content.addTextLayoutManager(&manager);
        content.setPrimaryTextLayoutManager(Some(&manager));
        manager.setTextContainer(Some(&container));
        let observing = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*content);
        observing.setTextStorage(Some(&storage));
        let native = [
            native_weak(&storage),
            native_weak(&content),
            native_weak(&manager),
            native_weak(&container),
        ];
        let values = {
            let range = content.documentRange();
            let origin = range.location();
            manager.ensureLayoutForRange(&range); // 有意只完整排这个固定短例；产品长文路径从不调用此 oracle。
            let result: RefCell<Result<Vec<OriginalLine>, String>> = RefCell::new(Ok(Vec::new()));
            let block = block2::RcBlock::new(|pointer: NonNull<NSTextLayoutFragment>| {
                let mut result = result.borrow_mut();
                let Ok(lines) = &mut *result else {
                    return Bool::NO;
                };
                // SAFETY: 标准同步枚举在本回调借出活 fragment；不 retain 到后继任务。
                let fragment = unsafe { pointer.as_ref() };
                let actual = (|| -> Result<(), String> {
                    if fragment.state() != NSTextLayoutFragmentState::LayoutAvailable {
                        return Err("旧短例没有真实 LayoutAvailable fragment".into());
                    }
                    let element = fragment.textElement().ok_or("旧短例缺少实际文本元素")?;
                    let element_range = element.elementRange().ok_or("旧短例元素缺少真实源范围")?;
                    let start = usize::try_from(
                        content.offsetFromLocation_toLocation(&origin, &element_range.location()),
                    )
                    .map_err(|_| "旧短例元素起点不是 canonical offset")?;
                    let end = usize::try_from(
                        content
                            .offsetFromLocation_toLocation(&origin, &element_range.endLocation()),
                    )
                    .map_err(|_| "旧短例元素终点不是 canonical offset")?;
                    let frame = fragment.layoutFragmentFrame();
                    let native_lines = fragment.textLineFragments();
                    for (ordinal, line) in native_lines.iter().enumerate() {
                        if lines.len() == 64 {
                            return Err("固定短例的实际视觉行超过本次独立 Native 对照范围".into());
                        }
                        let local = line.characterRange();
                        let bounds = line.typographicBounds();
                        let glyph = line.glyphOrigin();
                        let source_start = start
                            .checked_add(local.location)
                            .ok_or("旧短例源位置溢出")?;
                        let source_end = source_start
                            .checked_add(local.length)
                            .ok_or("旧短例源末端溢出")?;
                        if source_end > end {
                            return Err("旧标准行不在其实际元素范围内".into());
                        }
                        let glyphs = line_glyphs(&line, start, parts)?;
                        let table_glyphs = if alignment.is_some() {
                            table_positions::capture(&line, start, &text)?
                        } else {
                            Vec::new() // 普通段落与原CJK part观察不领取额外的短表glyph参考。
                        };
                        lines.push(OriginalLine {
                            glyphs,
                            table_glyphs,
                            graphemes: Vec::new(),
                            source: source_start..source_end,
                            paragraph: if source_start == source_end
                                && source_end == source.encode_utf16().count()
                                && source.ends_with(['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}'])
                            {
                                source_end..source_end // 系统额外EOF视觉行仍挂前一element；canonical新空段自身不再含旧字符。
                            } else {
                                start..end
                            },
                            frame: NSRect::new(
                                NSPoint::new(
                                    frame.origin.x + bounds.origin.x,
                                    frame.origin.y + bounds.origin.y,
                                ),
                                bounds.size,
                            ),
                            baseline: NSPoint::new(
                                frame.origin.x + bounds.origin.x + glyph.x,
                                frame.origin.y + bounds.origin.y + glyph.y,
                            ),
                            paragraph_frame: frame,
                            before: if ordinal == 0 { bounds.origin.y } else { 0.0 },
                            after: if ordinal + 1 == native_lines.len() {
                                frame.size.height - bounds.origin.y - bounds.size.height
                            } else {
                                0.0
                            }, // top/bottomMargin属于TextBlock；paragraph段距由真实首末line框与完整fragment框的差取得。
                        });
                    }
                    Ok(())
                })();
                if let Err(cause) = actual {
                    *result = Err(cause);
                    Bool::NO
                } else {
                    Bool::YES
                }
            });
            manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                None,
                NSTextLayoutFragmentEnumerationOptions::EnsuresLayout
                    | NSTextLayoutFragmentEnumerationOptions::EnsuresExtraLineFragment,
                &block,
            );
            drop(block);
            let mut values = result.into_inner();
            if let Some(alignment) = alignment
                && let Ok(lines) = &mut values
            {
                let positioned =
                    table_graphemes(source, &content, &manager, lines, grapheme_boundaries)
                        .and_then(|()| {
                            // 对照使用原容器的实际宽度；不拿产品 content/clip 或候选 glyph 反推。
                            let actual_width = container.size().width;
                            for line in lines {
                                table_positions::position(source, line, actual_width, alignment)?;
                            }
                            Ok(())
                        });
                if let Err(cause) = positioned {
                    // Native 查询/定位证明失败仍先拆开原图；不能用量化绕过浮点原图错误。
                    values = Err(cause);
                }
            }
            values
        };
        // 成功和失败都先归还标准图；不让一次 oracle 失败留住后继分页的资源或 Native 管理器。
        manager.setTextContainer(None);
        observing.setTextStorage(None);
        content.setPrimaryTextLayoutManager(None);
        content.removeTextLayoutManager(&manager);
        Ok((values, native))
    })?;
    if native.iter().any(|weak| weak.load().is_some()) {
        return Err("独立标准短例的 storage/content/manager/container 尚未真实释放".into());
    }
    values
}

/// 同一个已完整排好的短表图交付真实选区边缘；RTL 不能把逻辑首字符误当视觉左边缘。
/// 只对≤2KiB参考字面运行，整簇端点由独立固定字面声明，几何由原 TextKit manager 返回。
fn table_graphemes(
    source: &str,
    content: &NSTextContentStorage,
    manager: &NSTextLayoutManager,
    lines: &mut [OriginalLine],
    grapheme_boundaries: &[usize],
) -> Result<(), String> {
    /// 原 callback 的来源和框必须一起保留；相同矩形不能抹去另一个真实 text range。
    #[derive(Debug)]
    struct NativeSegment {
        source: Range<usize>,
        frame: NSRect,
    }

    let native = NSString::from_str(source);
    let length = native.length();
    // table_fonts::apply 在原图创建前已经证明固定 UAX 端点沿 scalar 完整覆盖此短 source。
    // 此处只把同一范围交给 Native 几何，不重新调用 Foundation 组合字符分组。
    let document = content.documentRange();
    let origin = document.location();
    for pair in grapheme_boundaries.windows(2) {
        let (offset, end) = (pair[0], pair[1]);
        let location = content
            .locationFromLocation_withOffset(&origin, offset as isize)
            .ok_or("原短表组合字符缺少真实起点")?;
        let terminal = content
            .locationFromLocation_withOffset(&origin, end as isize)
            .ok_or("原短表组合字符缺少真实末端")?;
        let range = NSTextRange::initWithLocation_endLocation(
            NSTextRange::alloc(),
            &location,
            Some(&terminal),
        )
        .ok_or("原短表组合字符无法组成真实选区")?;
        // 两次查询消费同一活图、同一 GCB、同一原 range；只差公开的原生 segment 语义。
        // Selection 的尾缘绘制政策与 Standard typographic 宽度分别原样保存，不相互覆盖。
        let segment = |kind| -> Result<NSRect, String> {
            let frames: RefCell<Result<Vec<NativeSegment>, String>> = RefCell::new(Ok(Vec::new()));
            {
                let block = block2::StackBlock::new(
                    |range: *mut NSTextRange,
                     frame: NSRect,
                     _baseline: f64,
                     _container: NonNull<NSTextContainer>| {
                        // 不再声明 RangeNotRequired：先读框架实际范围的两个全源位置，结束所有
                        // Native getter 后才借记录槽，不能把回调重入跨在 RefCell 可变借用里。
                        let observed = (|| -> Result<NativeSegment, String> {
                            let range =
                                NonNull::new(range).ok_or("原短表回调没有实际 text range")?;
                            // SAFETY: 同步枚举只在本 callback 内借出此 NSTextRange，不保留对象。
                            let range = unsafe { range.as_ref() };
                            let start = usize::try_from(
                                content.offsetFromLocation_toLocation(&origin, &range.location()),
                            )
                            .map_err(|_| "原短表回调范围起点不是 canonical 位置")?;
                            let terminal = usize::try_from(
                                content
                                    .offsetFromLocation_toLocation(&origin, &range.endLocation()),
                            )
                            .map_err(|_| "原短表回调范围末端不是 canonical 位置")?;
                            if start > terminal || terminal > length {
                                return Err("原短表回调范围不在同一真实文档内".into());
                            }
                            Ok(NativeSegment {
                                source: start..terminal,
                                frame,
                            })
                        })();
                        let mut result = frames.borrow_mut();
                        let Ok(frames) = &mut *result else {
                            return Bool::NO; // 先前真实失败仍属本次查询，不由后继回调改成成功。
                        };
                        match observed {
                            Ok(observed) if frames.len() < 4 => frames.push(observed),
                            Ok(_) => {
                                *result = Err("原短表真实回调片段超出有限观察范围".into());
                                return Bool::NO; // 不能截去第5片后把前4个重复框冒充完整结果。
                            }
                            Err(cause) => {
                                *result = Err(cause);
                                return Bool::NO;
                            }
                        }
                        Bool::YES
                    },
                );
                manager.enumerateTextSegmentsInRange_type_options_usingBlock(
                    &range,
                    kind,
                    NSTextLayoutManagerSegmentOptions::empty(),
                    &block,
                );
            } // callback 的有限 frames 借用结束后，才取出纯值结果。
            let frames = frames.into_inner()?;
            let Some(first) = frames.first() else {
                return Err(format!(
                    "原短表组合字符没有实际原生框：source={offset}..{end} kind={kind:?} frames={frames:?}"
                ));
            };
            // 同一个 GCB 可收到框架合法重复通知，但必须连实际 range 都与原查询完全相同。
            // 这里不做矩形 dedup、不合并异片，也不从多个不同结果中选择第一项通过验收。
            if first.source != (offset..end)
                || frames.iter().any(|observed| {
                    observed.source != first.source || observed.frame != first.frame
                })
            {
                return Err(format!(
                    "原短表组合字符没有唯一同源几何结论：source={offset}..{end} kind={kind:?} frames={frames:?}"
                ));
            }
            if frames.len() > 1 {
                eprintln!(
                    "native_table_original_segment_repeat source={offset}..{end} kind={kind:?} observations={frames:?}"
                ); // 保留每次实际回调的纯值事实；共享几何结论不等于从诊断中删掉重复观察。
            }
            let frame = first.frame;
            if ![
                frame.origin.x,
                frame.origin.y,
                frame.size.width,
                frame.size.height,
            ]
            .into_iter()
            .all(f64::is_finite)
                || frame.size.width < 0.0
                || frame.size.height <= 0.0
            {
                return Err("原短表组合字符几何不是实际有限原生框".into());
            }
            Ok(frame)
        };
        let frame = segment(NSTextLayoutManagerSegmentType::Selection)?;
        let typographic_frame = segment(NSTextLayoutManagerSegmentType::Standard)?;
        let line = lines
            .iter_mut()
            .find(|line| line.source.start <= offset && end <= line.source.end)
            .ok_or("原短表组合字符跨了实际软行边界")?;
        line.graphemes.push(OriginalGrapheme {
            source: offset..end,
            frame,
            typographic_frame,
            positioned_frame: None, // 原浮点观察尚未完成逐glyph自洽与26.6定位，不能提前签发。
        });
    }
    Ok(())
}
