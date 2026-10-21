//! Independent standard TextKit 2 geometry for short synthetic fixtures, without paged paragraph policy
//! or user-document access.

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

/// Only Source ranges and actual geometry leave the Native pool, without fonts, attributed text, or
/// TextKit graphs retained for the paged candidate.
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
    // Short tables alone need these original Native facts, consumed for floating consistency/26.6
    // conversion within the original pool.
    table_glyphs: Vec<table_positions::Glyph>,
}

/// Actual Native composed-character selections retain public Source and Copy rectangles only, without
/// system layout graphs.
pub(super) struct OriginalGrapheme {
    pub source: Range<usize>,
    /// Selection boxes preserve system clipping/soft-line-end extension and cannot substitute for
    /// complete glyph advances.
    pub frame: NSRect,
    /// Standard typographic bounds for the same original grapheme first establish floating-glyph
    /// consistency only.
    typographic_frame: NSRect,
    /// Per-actual-glyph quantization supplies positioned reference bounds while raw frames preserve
    /// original Native policy as a distinct contract.
    pub positioned_frame: Option<NSRect>,
}

/// Retain actual sealed pure facts after candidate close; ranges select comparison subjects without
/// generating reference fonts/soft lines.
pub(super) struct PartAdvance {
    pub source: Range<usize>,
    pub advance_26_6: i64,
}

/// After TextKit line close, consume captured facts only, without another CTLine applying its different
/// CJK line-end policy.
/// Parts identify original comparison boundaries only, supplying neither fonts nor advances nor Native
/// glyph segmentation.
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
            // Reject an actual glyph cluster crossing the part boundary; never distribute ligature
            // advance by character proportions.
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

/// Retain this pure value only after original Native glyph Source and actual TextKit edges are both
/// known.
/// Retain no attributed string, font, or line fragment; candidate and original Native graphs still
/// close fully in their existing order.
struct OriginalGlyph {
    source: Range<usize>,
    advance_26_6: i64,
}

/// CTTypesetter certifies same-font actual glyph/cluster boundaries only; advances come from already
/// laid-out TextKit lines.
/// Keep the distinct actual Native CJK line-end policies separate instead of correcting original lines
/// with an independent CTLine difference.
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
        return Ok(Vec::new()); // Ordinary geometry cases without glyph consumers create no additional CTLine or metadata.
    }
    let attributed = line.attributedString();
    if end > attributed.length() || local.length > 256 || local.length == 0 {
        return Err("旧 TextKit 行不在原属性串内或超过256个固定来源单元".into());
    }
    // SAFETY: The actual TextKit line retains complete original attributes, without substrings or
    // candidate attributes.
    let typesetter =
        unsafe { CTTypesetter::with_attributed_string_and_options(attributed.as_ref(), None) }
            .ok_or("真实 TextKit 属性没有取得有限边界参考")?;
    // SAFETY: Range identifies the actual current line in the original attributed string; CTLine
    // supplies no expected advance.
    let native = unsafe { typesetter.line(CFRange::new(local.location as _, local.length as _)) };
    // SAFETY: The immutable line retains every run; collect complete glyph Source and actual font
    // identity only.
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
        // SAFETY: Ordinal belongs to the live array; the CTRun borrow stays within this actual
        // synchronous TextKit observation.
        let raw = unsafe { runs.value_at_index(ordinal) };
        let run = NonNull::new(raw.cast_mut().cast::<CTRun>()).ok_or("原 run 为空")?;
        // SAFETY: Native line/array retain the original run; read actual glyph domain/attributes
        // without querying candidate Font.
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
            continue; // An actual empty run reads no stack-array entries and creates no synthetic Source boundary.
        }
        // SAFETY: The public Core Text font-key value is CTFont; its dictionary spans every borrow
        // below.
        let actual = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
        let actual = NonNull::new(actual.cast_mut().cast::<CTFont>()).ok_or("原 run 缺 Font")?;
        // SAFETY: Count fits 256 entries and both fixed arrays fully hold this run original
        // glyphs/clusters.
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
        // SAFETY: The actual run font stays alive during observation; glyph_count reads that same
        // Native glyph domain only.
        let domain = usize::try_from(unsafe { actual.as_ref().glyph_count() })
            .map_err(|_| "原 run Font 的 glyph 域无效")?;
        for glyph in 0..used {
            let from = usize::try_from(indices[glyph]).map_err(|_| "原 glyph cluster 无效")?;
            if from < local.location || from >= end || usize::from(glyphs[glyph]) >= domain {
                return Err("原 glyph 不在同一真实行来源或字体字形域内".into());
            }
            // SAFETY: From is an original cluster in this actual attributed string; the returned object
            // stays retained through identity validation.
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
            // SAFETY: Both fonts come from the same actual TextKit attributed Source; equal names/sizes
            // cannot replace CFEqual.
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
    // Character edges cannot independently recover advances for multiple glyphs in one cluster; partial
    // evidence is not a complete reference.
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
            // Even when a part begins here, exclude positions lying inside the same actual Native
            // glyph.
            // Checking only the next edge misses a part beginning inside a ligature but ending at a
            // valid boundary.
            if !previous.x.is_finite() || previous.x >= point.x {
                return Err(format!(
                    "原 TextKit glyph 起点与前驱同原点或逆序，不能授予 part 边界：source={source:?} previous={previous:?} point={point:?}"
                ));
            }
        }
        let next = if to == end {
            terminal // Original typographicBounds supplies the actual line end without another CTLine end position.
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

/// Type-erased weak observations still identify the actual created objects without retaining any system
/// layout owner.
fn native_weak(value: &AnyObject) -> Weak<AnyObject> {
    Weak::from(value)
}

/// Read actual system default-line scalars and independent short TextKit 2 layouts, without inferring a
/// ceiling rule from one failed size.
pub(super) fn font_defaults() -> Result<(), String> {
    // Use the actual document palette for line-scalar comparison, without arbitrary UI-font defaults
    // becoming product contracts.
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
            ) // Query system font-line scalars only, without storage, container, text, or a second product layout graph.
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
        } // Every font wraps at the actual fixed 72-point width, without deriving shared small/hidden-font expectations from the candidate.
    }
    Ok(())
}

/// The complete reference parser accepts fixed short fixtures only; fonts/attributes use the unchanged
/// original Native path.
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
            // Log fixed-fixture original attribute font metadata independently of the candidate,
            // without document text or changed expectations.
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
        // SAFETY: Apply original attributes to one complete scalar in this short fixture without
        // splitting surrogates or exceeding Native length.
        unsafe { result.addAttributes_range(&attributes, NSRange::new(index, scalar.len_utf16())) };
        index += scalar.len_utf16();
    }
    Ok(result)
}

/// Actual standard layout accepts at most 2 KiB reference fixtures, detaching all relationships in the
/// same pool before returning pure records.
pub(super) fn measure(
    source: &str,
    width: f64,
    font: &Retained<NSFont>,
    parts: &[PartAdvance],
) -> Result<Vec<OriginalLine>, String> {
    measure_text(source, width, font, parts, None, &[])
}

/// Original table attributes install actual NSParagraphStyle alignment; original Native layout
/// determines trailing-whitespace participation.
/// Share the same actual assembly/teardown with ordinary paragraphs, without another reference graph or
/// expected-side offset recomputation.
/// Short-table alignment consumes actual grapheme boxes including trailing whitespace; ordinary
/// paragraph entry performs no such queries.
pub(super) fn measure_table_cell_graphemes(
    source: &str,
    width: f64,
    font: &Retained<NSFont>,
    alignment: omni_markdown_core::MarkdownTableAlignment,
    grapheme_boundaries: &[usize],
) -> Result<Vec<OriginalLine>, String> {
    // Independent fixture Unicode endpoints supply grouping, rather than Core output under test or this
    // machine Foundation grouping policy.
    // Native supplies actual geometry for the same original ranges, whose complete Source coverage is
    // checked below.
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
            // Only fixed short tables require same-candidate whole-grapheme coverage; ordinary
            // paragraphs retain observed scalar-based TextKit policy.
            // Validate declared ranges before font choice; later Native geometry cannot rewrite Unicode
            // boundaries using older Foundation grouping.
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
        container.setLineFragmentPadding(0.0); // Use the same actual content width on both paths, excluding system five-point padding from wrap comparison.
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
            manager.ensureLayoutForRange(&range); // Intentionally lay out this fixed short fixture completely; the production long-document path never calls this reference.
            let result: RefCell<Result<Vec<OriginalLine>, String>> = RefCell::new(Ok(Vec::new()));
            let block = block2::RcBlock::new(|pointer: NonNull<NSTextLayoutFragment>| {
                let mut result = result.borrow_mut();
                let Ok(lines) = &mut *result else {
                    return Bool::NO;
                };
                // SAFETY: Standard synchronous enumeration lends a live fragment during this callback
                // only, without retention into later work.
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
                            Vec::new() // Ordinary paragraphs/CJK-part observations request no additional table glyph reference.
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
                                source_end..source_end // The system extra EOF visual line may belong to the previous element; canonical trailing empty paragraph contains no old characters.
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
                            }, // TextBlock owns top/bottom margins; actual first/last line bounds relative to the full fragment determine paragraph spacing.
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
                            // The reference uses actual original container width without inferring it
                            // from product content/clip or candidate glyphs.
                            let actual_width = container.size().width;
                            for line in lines {
                                table_positions::position(source, line, actual_width, alignment)?;
                            }
                            Ok(())
                        });
                if let Err(cause) = positioned {
                    // Detach the original graph after Native query/position failure; quantization
                    // cannot bypass incorrect original floating geometry.
                    values = Err(cause);
                }
            }
            values
        };
        // Return the standard graph on both success and failure so a reference error cannot retain
        // resources/managers into later paged work.
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

/// The completely laid-out short table supplies actual selection edges; RTL logical-first characters
/// cannot stand in for visual-left edges.
/// Run on at most 2 KiB reference literals, using independently declared whole-grapheme endpoints and
/// actual TextKit manager geometry.
fn table_graphemes(
    source: &str,
    content: &NSTextContentStorage,
    manager: &NSTextLayoutManager,
    lines: &mut [OriginalLine],
    grapheme_boundaries: &[usize],
) -> Result<(), String> {
    /// Retain callback Source and frame together; equal rectangles cannot erase a different actual text
    /// range.
    #[derive(Debug)]
    struct NativeSegment {
        source: Range<usize>,
        frame: NSRect,
    }

    let native = NSString::from_str(source);
    let length = native.length();
    // table_fonts::apply already certified complete scalar-aligned Unicode endpoint coverage before
    // graph creation.
    // Pass the same range to Native geometry without regrouping composed characters through Foundation.
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
        // Both queries consume the same live graph, grapheme, and original range, differing only in
        // public Native segment semantics.
        // Preserve Selection tail-drawing policy and Standard typographic width independently rather
        // than overwriting either observation.
        let segment = |kind| -> Result<NSRect, String> {
            let frames: RefCell<Result<Vec<NativeSegment>, String>> = RefCell::new(Ok(Vec::new()));
            {
                let block = block2::StackBlock::new(
                    |range: *mut NSTextRange,
                     frame: NSRect,
                     _baseline: f64,
                     _container: NonNull<NSTextContainer>| {
                        // Do not request RangeNotRequired: read both full-Source positions from the
                        // actual framework range, ending every
                        // Native getter before borrowing the record slot so callback reentry crosses no
                        // RefCell mutable borrow.
                        let observed = (|| -> Result<NativeSegment, String> {
                            let range =
                                NonNull::new(range).ok_or("原短表回调没有实际 text range")?;
                            // SAFETY: Synchronous enumeration lends this NSTextRange inside the
                            // callback only, without retaining it.
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
                            return Bool::NO; // An earlier actual query failure cannot be overwritten by a later callback success.
                        };
                        match observed {
                            Ok(observed) if frames.len() < 4 => frames.push(observed),
                            Ok(_) => {
                                *result = Err("原短表真实回调片段超出有限观察范围".into());
                                return Bool::NO; // Do not drop a fifth segment and present four repeated boxes as a complete result.
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
            } // End callback bounded-frame borrows before extracting pure results.
            let frames = frames.into_inner()?;
            let Some(first) = frames.first() else {
                return Err(format!(
                    "原短表组合字符没有实际原生框：source={offset}..{end} kind={kind:?} frames={frames:?}"
                ));
            };
            // A grapheme may receive legitimate repeated framework callbacks, but actual range must
            // also match the original query exactly.
            // Neither deduplicate rectangles, combine distinct segments, nor choose the first of
            // inconsistent observations to pass.
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
                ); // Keep pure facts from every actual callback; one shared geometry conclusion does not erase repeated diagnostic observations.
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
            positioned_frame: None, // Original floating observations await per-glyph consistency/26.6 positioning, so issue no positioned frame yet.
        });
    }
    Ok(())
}
