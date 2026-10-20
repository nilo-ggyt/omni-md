//! Owns bounded native font/attribute caches and derived paragraph styling.
//! No cache retains canonical document bytes or any session beyond its surface.

use super::*;

/// 同一预览策略的链接/代码颜色供TextKit属性与Paged真实绘制共用，避免两条生产路径自行偏色。
pub(super) const NATIVE_MARKDOWN_LINK_COLOR: [f64; 4] = [0.40, 0.68, 1.0, 1.0];
pub(super) const NATIVE_MARKDOWN_CODE_BACKGROUND: [f64; 4] = [0.13, 0.15, 0.20, 1.0];
pub(super) const NATIVE_MARKDOWN_MARKER_COLOR: [f64; 4] = [0.48, 0.56, 0.68, 1.0];
/// 引用正文只改变墨迹，不替换真实字体；属性和分页绘图使用同一既有颜色。
pub(super) const NATIVE_MARKDOWN_QUOTE_COLOR: [f64; 4] = [0.58, 0.66, 0.78, 1.0];
/// 表格沿既有界面色值；CPU分页绘图与原NSColor策略共用，接入时不另造主题。
pub(super) const NATIVE_MARKDOWN_TABLE_BORDER: [f64; 4] = [0.28, 0.34, 0.44, 1.0];
pub(super) const NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND: [f64; 4] = [0.105, 0.12, 0.16, 1.0];
pub(super) const NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND: [f64; 4] = [0.075, 0.084, 0.11, 1.0];

/// 同一活动 editor 的有限字体与颜色；分页只取共同策略，TextKit 另在装配时取得列表字形。
/// 资源随原 palette 归还，不持正文，也不因滚动建立新的字体或属性资源拥有者。
pub(super) struct MarkdownTextAttributePalette {
    // 分页不消费 NSGlyphInfo，因此没有这组强边；TextKit 构造入口仍一次捕获完整五字形。
    // 字体不支持该组符号时保留原 None/字面语义，不把资源缺席冒充隐藏源码的权限。
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) list_glyphs: Option<MarkdownListMarkerGlyphPalette>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) list_hidden_foreground: Retained<NSColor>,
    pub(super) normal_font: Retained<NSFont>,
    pub(super) strong_font: Retained<NSFont>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) source_strong_font: Retained<NSFont>,
    pub(super) fixed_font: Retained<NSFont>,
    pub(super) table_header_font: Retained<NSFont>,
    pub(super) hidden_font: Retained<NSFont>,
    pub(super) heading_fonts: Vec<Retained<NSFont>>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) foreground: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) muted_foreground: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) accent_foreground: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) marker_foreground: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) hidden_foreground: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) code_background: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) structure_border: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) table_header_background: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) table_row_background: Retained<NSColor>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) table_active_background: Retained<NSColor>,
}

impl MarkdownTextAttributePalette {
    /// 构造分页与属性分支共用的固定字体、颜色；不为分页创建仅 TextKit 消费的 NSGlyphInfo。
    /// 调用方须在原 Native 装配边界取得本值，不能把字体创建推迟到正文或 RefCell 借用中。
    pub(super) fn new(normal_font: Retained<NSFont>) -> Self {
        // Build the finite font variants once; scrolling only reuses these retained objects.
        // SAFETY: AppKit exports immutable font-weight constants with process lifetime.
        let (regular_weight, semibold_weight) =
            unsafe { (NSFontWeightRegular, NSFontWeightSemibold) };
        let fixed_font = NSFont::monospacedSystemFontOfSize_weight(14.0, regular_weight);
        let table_header_font = NSFont::systemFontOfSize_weight(14.0, semibold_weight);
        let heading_fonts = [28.0, 24.0, 20.0, 18.0, 16.0, 15.0]
            .into_iter()
            .map(NSFont::boldSystemFontOfSize)
            .collect();
        // Retain the complete color/font palette under the editor so cache entries never borrow stack values.
        Self {
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            list_glyphs: None,
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            list_hidden_foreground: NSColor::clearColor(),
            strong_font: NSFont::boldSystemFontOfSize(15.0),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            source_strong_font: NSFont::monospacedSystemFontOfSize_weight(14.0, semibold_weight),
            normal_font,
            fixed_font,
            table_header_font,
            hidden_font: NSFont::systemFontOfSize(0.1),
            heading_fonts,
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            foreground: NSColor::colorWithSRGBRed_green_blue_alpha(0.86, 0.88, 0.92, 1.0),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            muted_foreground: NSColor::colorWithSRGBRed_green_blue_alpha(
                NATIVE_MARKDOWN_QUOTE_COLOR[0],
                NATIVE_MARKDOWN_QUOTE_COLOR[1],
                NATIVE_MARKDOWN_QUOTE_COLOR[2],
                NATIVE_MARKDOWN_QUOTE_COLOR[3],
            ),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            accent_foreground: NSColor::colorWithSRGBRed_green_blue_alpha(
                NATIVE_MARKDOWN_LINK_COLOR[0],
                NATIVE_MARKDOWN_LINK_COLOR[1],
                NATIVE_MARKDOWN_LINK_COLOR[2],
                NATIVE_MARKDOWN_LINK_COLOR[3],
            ),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            marker_foreground: NSColor::colorWithSRGBRed_green_blue_alpha(
                NATIVE_MARKDOWN_MARKER_COLOR[0],
                NATIVE_MARKDOWN_MARKER_COLOR[1],
                NATIVE_MARKDOWN_MARKER_COLOR[2],
                NATIVE_MARKDOWN_MARKER_COLOR[3],
            ),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            hidden_foreground: NSColor::colorWithSRGBRed_green_blue_alpha(0.075, 0.084, 0.11, 1.0),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            code_background: NSColor::colorWithSRGBRed_green_blue_alpha(
                NATIVE_MARKDOWN_CODE_BACKGROUND[0],
                NATIVE_MARKDOWN_CODE_BACKGROUND[1],
                NATIVE_MARKDOWN_CODE_BACKGROUND[2],
                NATIVE_MARKDOWN_CODE_BACKGROUND[3],
            ),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            structure_border: NSColor::colorWithSRGBRed_green_blue_alpha(
                NATIVE_MARKDOWN_TABLE_BORDER[0],
                NATIVE_MARKDOWN_TABLE_BORDER[1],
                NATIVE_MARKDOWN_TABLE_BORDER[2],
                NATIVE_MARKDOWN_TABLE_BORDER[3],
            ),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            table_header_background: NSColor::colorWithSRGBRed_green_blue_alpha(
                NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND[0],
                NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND[1],
                NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND[2],
                NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND[3],
            ),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            table_row_background: NSColor::colorWithSRGBRed_green_blue_alpha(
                NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND[0],
                NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND[1],
                NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND[2],
                NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND[3],
            ),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            table_active_background: NSColor::colorWithSRGBRed_green_blue_alpha(
                0.13, 0.20, 0.31, 1.0,
            ),
        }
    }

    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    /// TextKit 在原 storage 装配点取得完整资源；所有 Native 创建均先于属性/正文槽的借用。
    /// 五个字形仍整体成功或整体保持字面，由原 palette 独占到该 editor 实际释放。
    pub(super) fn new_for_textkit(normal_font: Retained<NSFont>) -> Self {
        let mut palette = Self::new(normal_font);
        // 这里已经处于真实 TextKit 创建流程；不在属性 getter、首次滚动或懒查询中补建字形。
        palette.list_glyphs = MarkdownListMarkerGlyphPalette::new();
        palette
    }
}

/// Detect a heading role in the fixed semantic bit domain without allocating a style collection.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn native_markdown_is_heading(styles: MarkdownPresentationStyles) -> bool {
    [
        MarkdownPresentationStyles::HEADING_1,
        MarkdownPresentationStyles::HEADING_2,
        MarkdownPresentationStyles::HEADING_3,
        MarkdownPresentationStyles::HEADING_4,
        MarkdownPresentationStyles::HEADING_5,
        MarkdownPresentationStyles::HEADING_6,
    ]
    .iter()
    .any(|heading| styles.contains(*heading))
}

/// Select literal source typography, preserving every marker's width and a uniform 14-point baseline.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn native_markdown_source_font(
    styles: MarkdownPresentationStyles,
    palette: &MarkdownTextAttributePalette,
) -> Retained<NSFont> {
    let font = if native_markdown_is_heading(styles)
        || styles.contains(MarkdownPresentationStyles::STRONG)
    {
        // Headings and strong source receive weight rather than preview's varying heading sizes.
        &palette.source_strong_font
    } else {
        // Code, markers and plain source share the same monospaced character geometry.
        &palette.fixed_font
    };
    if styles.contains(MarkdownPresentationStyles::EMPHASIS) {
        // Emphasis adds only an italic face while retaining the selected source point size.
        let descriptor = font.fontDescriptor();
        let descriptor = descriptor.fontDescriptorWithSymbolicTraits(
            descriptor.symbolicTraits() | NSFontDescriptorSymbolicTraits::TraitItalic,
        );
        NSFont::fontWithDescriptor_size(&descriptor, font.pointSize())
            .unwrap_or_else(|| font.clone())
    } else {
        // Ordinary source reuses its fixed retained face.
        font.clone()
    }
}

/// Match the preserved source highlighter's principal Markdown colors using finite semantic roles.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn native_markdown_source_color(styles: MarkdownPresentationStyles) -> Retained<NSColor> {
    let (red, green, blue) = if styles.contains(MarkdownPresentationStyles::MARKER) {
        // Delimiters/meta remain visible and subdued rather than preview-concealed.
        (0x7f, 0x89, 0x9a)
    } else if styles.contains(MarkdownPresentationStyles::LINK) {
        // Link labels/destinations use the same source accent and native underline.
        (0x8f, 0xb3, 0xff)
    } else if native_markdown_is_heading(styles) {
        // Heading content keeps high-contrast source weight without an accidental underline.
        (0xf1, 0xf4, 0xfa)
    } else if styles.contains(MarkdownPresentationStyles::STRONG) {
        // Strong source has a slightly brighter foreground than ordinary text.
        (0xf0, 0xf3, 0xf8)
    } else if styles.contains(MarkdownPresentationStyles::STRIKETHROUGH) {
        // Deleted/struck source remains legible and visually muted.
        (0x8e, 0x98, 0xa9)
    } else if styles.contains(MarkdownPresentationStyles::INLINE_CODE)
        || styles.contains(MarkdownPresentationStyles::FENCED_CODE)
    {
        // Literal code uses the preserved source literal color, with no preview background geometry.
        (0x8b, 0xd5, 0xa2)
    } else {
        // Unstyled and emphasis text retain the ordinary readable source foreground.
        (0xe5, 0xe9, 0xf1)
    };
    NSColor::colorWithSRGBRed_green_blue_alpha(
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0,
        1.0,
    )
}

/// Selects source/semantic typography before resolving Unicode coverage.
pub(super) fn native_markdown_base_font(
    styles: MarkdownPresentationStyles,
    palette: &MarkdownTextAttributePalette,
) -> Retained<NSFont> {
    let hidden_marker = styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        || styles.contains(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER);
    let heading_font = if styles.contains(MarkdownPresentationStyles::HEADING_1) {
        // Level-one headings use the largest fixed heading face.
        palette.heading_fonts.first()
    } else if styles.contains(MarkdownPresentationStyles::HEADING_2) {
        // Level-two headings use the second fixed heading face.
        palette.heading_fonts.get(1)
    } else if styles.contains(MarkdownPresentationStyles::HEADING_3) {
        // Level-three headings keep their own readable hierarchy step.
        palette.heading_fonts.get(2)
    } else if styles.contains(MarkdownPresentationStyles::HEADING_4) {
        // Level-four headings retain hierarchy without changing document bytes.
        palette.heading_fonts.get(3)
    } else if styles.contains(MarkdownPresentationStyles::HEADING_5) {
        // Level-five headings use the smaller fixed bold face.
        palette.heading_fonts.get(4)
    } else if styles.contains(MarkdownPresentationStyles::HEADING_6) {
        // Level-six headings retain the minimum heading emphasis.
        palette.heading_fonts.get(5)
    } else {
        // Ordinary spans inherit their inline semantic font below.
        None
    };
    let base_font = if hidden_marker {
        // Hidden syntax contributes negligible width while the piece tree preserves its bytes.
        &palette.hidden_font
    } else {
        // Visible spans choose heading, table, code, strong, then ordinary typography in that priority.
        heading_font
            .or_else(|| {
                // Table headers and strong table cells share the compact table font.
                (styles.contains(MarkdownPresentationStyles::TABLE_HEADER)
                    || (styles.contains(MarkdownPresentationStyles::TABLE)
                        && styles.contains(MarkdownPresentationStyles::STRONG)))
                .then_some(&palette.table_header_font)
            })
            .or_else(|| {
                // 只有代码语义使用等宽字体，普通美元文字保留其原有正文或强调字体。
                (styles.contains(MarkdownPresentationStyles::INLINE_CODE)
                    || styles.contains(MarkdownPresentationStyles::FENCED_CODE))
                .then_some(&palette.fixed_font)
            })
            .or_else(|| {
                // Remaining strong text uses the ordinary bold face.
                styles
                    .contains(MarkdownPresentationStyles::STRONG)
                    .then_some(&palette.strong_font)
            })
            .unwrap_or(&palette.normal_font)
    };

    let subscript_content = !hidden_marker
        && !styles.contains(MarkdownPresentationStyles::MARKER)
        && styles.contains(MarkdownPresentationStyles::SUBSCRIPT);
    if !hidden_marker
        && (styles.contains(MarkdownPresentationStyles::EMPHASIS) || subscript_content)
    {
        // 沿已选标题/粗体/表格face保留组合特征；只缩小实际下标内容，显露的语法符仍可正常编辑。
        let descriptor = base_font.fontDescriptor();
        let descriptor = if styles.contains(MarkdownPresentationStyles::EMPHASIS) {
            // 倾斜与下标可同时存在，不能为缩字号覆盖原EMPHASIS语义。
            descriptor.fontDescriptorWithSymbolicTraits(
                descriptor.symbolicTraits() | NSFontDescriptorSymbolicTraits::TraitItalic,
            )
        } else {
            descriptor // 普通下标沿原face，不引入独立字体集合或正文缓存。
        };
        let point_size = if subscript_content {
            base_font.pointSize() * 0.75
        } else {
            base_font.pointSize() // 旧斜体保留原字号；Source不经过本Preview选字路径。
        };
        NSFont::fontWithDescriptor_size(&descriptor, point_size)
            .unwrap_or_else(|| base_font.clone())
    } else {
        // Nonitalic and hidden spans reuse the existing retained face without a new descriptor.
        base_font.clone()
    }
}

/// One style-to-face cache entry, including immutable scalar coverage.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct MarkdownStyleFontEntry {
    pub(super) source_mode: bool,
    pub(super) styles: MarkdownPresentationStyles,
    pub(super) font: Retained<NSFont>,
    pub(super) coverage: Retained<NSCharacterSet>,
}

/// One bounded Unicode fallback entry tied to its semantic base face.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct MarkdownFallbackFontEntry {
    pub(super) base_font: Retained<NSFont>,
    pub(super) font: Retained<NSFont>,
    pub(super) coverage: Retained<NSCharacterSet>,
}

/// Selected face and coverage used to bound an adjacent compatible UTF-16 run.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct MarkdownResolvedFont {
    pub(super) font: Retained<NSFont>,
    pub(super) base_coverage: Retained<NSCharacterSet>,
    pub(super) fallback_coverage: Option<Retained<NSCharacterSet>>,
}

/// Editor-owned LRU caches; their fixed capacities do not scale with document length.
#[derive(Default)]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct MarkdownFontSelectionCache {
    pub(super) style_fonts: VecDeque<MarkdownStyleFontEntry>,
    pub(super) fallback_fonts: VecDeque<MarkdownFallbackFontEntry>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl super::cache_owner::CachePayload for MarkdownFontSelectionCache {
    fn empty() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl MarkdownFontSelectionCache {
    /// owner 正在外层 native 栈时按同一字体策略计算，但不分配或发布第二个 LRU。
    pub(super) fn resolve_uncached(
        styles: MarkdownPresentationStyles,
        scalar: char,
        palette: &MarkdownTextAttributePalette,
        source_mode: bool,
    ) -> MarkdownResolvedFont {
        let base_font = if source_mode {
            // Source 必须保留等宽语义，不能因重入改回 preview 字体。
            native_markdown_source_font(styles, palette)
        } else {
            // Preview 仍处理标题、隐藏标记、表格和 emphasis 的原有优先级。
            native_markdown_base_font(styles, palette)
        };
        let base_coverage = base_font.coveredCharacterSet();
        if (!source_mode && styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER))
            || native_markdown_character_set_covers(&base_coverage, scalar)
        {
            // 支持完整 scalar 时继续使用真实 base face，不伪造普通正文属性。
            return MarkdownResolvedFont {
                font: base_font,
                base_coverage,
                fallback_coverage: None,
            };
        }
        // 未覆盖 scalar 按原策略让 Core Text 选择完整字符的 fallback，临时值仅活在调用栈。
        let font = native_markdown_fallback_font(&base_font, scalar);
        let coverage = font.coveredCharacterSet();
        MarkdownResolvedFont {
            font,
            base_coverage,
            fallback_coverage: Some(coverage),
        }
    }

    /// Resolves one scalar and promotes reused fallback faces within the bounded LRU.
    pub(super) fn resolve(
        &mut self,
        styles: MarkdownPresentationStyles,
        scalar: char,
        palette: &MarkdownTextAttributePalette,
        source_mode: bool,
    ) -> MarkdownResolvedFont {
        // Resolve semantic typography before testing coverage for this scalar.
        let (base_font, base_coverage) = self.style_font(styles, palette, source_mode);
        if (!source_mode && styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER))
            || native_markdown_character_set_covers(&base_coverage, scalar)
        {
            // Hidden syntax and supported scalars need no fallback font or fallback-run cache.
            return MarkdownResolvedFont {
                font: base_font,
                base_coverage,
                fallback_coverage: None,
            };
        }

        // Reuse a fallback only when both semantic base identity and scalar coverage match.
        if let Some(index) = self.fallback_fonts.iter().position(|entry| {
            // A different base face may require a different weight/size even for the same character.
            native_markdown_same_font(&entry.base_font, &base_font)
                && native_markdown_character_set_covers(&entry.coverage, scalar)
        }) {
            // Promote this live entry to most-recently-used and return retained independent handles.
            let entry = self
                .fallback_fonts
                .remove(index)
                .expect("fallback font cache index was found immediately above");
            let resolved = MarkdownResolvedFont {
                font: entry.font.clone(),
                base_coverage,
                fallback_coverage: Some(entry.coverage.clone()),
            };
            self.fallback_fonts.push_back(entry);
            return resolved;
        }

        // Core Text resolves an uncovered scalar without materializing any surrounding document.
        let fallback_font = native_markdown_fallback_font(&base_font, scalar);
        let fallback_coverage = fallback_font.coveredCharacterSet();
        if self.fallback_fonts.len() == MAX_NATIVE_MARKDOWN_FALLBACK_FONT_CACHE_ENTRIES {
            // Evict the least recently used fallback before inserting a new native face.
            self.fallback_fonts.pop_front();
        }
        self.fallback_fonts.push_back(MarkdownFallbackFontEntry {
            base_font,
            font: fallback_font.clone(),
            coverage: fallback_coverage.clone(),
        });
        MarkdownResolvedFont {
            font: fallback_font,
            base_coverage,
            fallback_coverage: Some(fallback_coverage),
        }
    }

    /// Resolves a semantic font once per style combination with bounded LRU promotion.
    pub(super) fn style_font(
        &mut self,
        styles: MarkdownPresentationStyles,
        palette: &MarkdownTextAttributePalette,
        source_mode: bool,
    ) -> (Retained<NSFont>, Retained<NSCharacterSet>) {
        if let Some(index) = self
            .style_fonts
            .iter()
            .position(|entry| entry.styles == styles && entry.source_mode == source_mode)
        {
            // Matching style bits reuse the native face and coverage, then refresh their recency.
            let entry = self
                .style_fonts
                .remove(index)
                .expect("style font cache index was found immediately above");
            let result = (entry.font.clone(), entry.coverage.clone());
            self.style_fonts.push_back(entry);
            return result;
        }

        // An unseen semantic combination acquires one face and its immutable coverage set.
        let font = if source_mode {
            // Literal source keeps monospaced line geometry while exposing semantic bold/italic syntax.
            native_markdown_source_font(styles, palette)
        } else {
            // Inline preview keeps heading sizes and concealed marker typography.
            native_markdown_base_font(styles, palette)
        };
        let coverage = font.coveredCharacterSet();
        if self.style_fonts.len() == MAX_NATIVE_MARKDOWN_STYLE_FONT_CACHE_ENTRIES {
            // Enforce the fixed semantic-font bound before admitting another combination.
            self.style_fonts.pop_front();
        }
        self.style_fonts.push_back(MarkdownStyleFontEntry {
            source_mode,
            styles,
            font: font.clone(),
            coverage: coverage.clone(),
        });
        (font, coverage)
    }
}

/// Asks Core Text for the complete scalar, preserving its returned ownership claim.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_fallback_font(base_font: &NSFont, scalar: char) -> Retained<NSFont> {
    // A Unicode scalar needs at most four UTF-8 bytes and two UTF-16 units.
    let mut utf8 = [0; 4];
    let scalar_text = scalar.encode_utf8(&mut utf8);
    let string = CFString::from_str(scalar_text);
    let range = CFRange::new(0, scalar.len_utf16() as _);
    let core_text_font: &CTFont = base_font.as_ref();
    // SAFETY: `string` contains the complete queried Unicode scalar and `range`
    // is expressed in CFString UTF-16 units. Core Text returns a +1 CTFont.
    let fallback = unsafe { core_text_font.for_string(&string, range) };
    let fallback: Retained<CTFont> = fallback.into();
    // SAFETY: Apple documents CTFont and NSFont as toll-free bridged. The
    // conversion keeps the +1 ownership and changes only the Rust view type.
    unsafe { Retained::cast_unchecked(fallback) }
}

/// Compares cached native face identity without expensive semantic equality.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_same_font(left: &NSFont, right: &NSFont) -> bool {
    std::ptr::eq(left, right)
}

/// Treats ASCII as a base-font fast path and checks full scalar coverage otherwise.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_character_set_covers(
    coverage: &NSCharacterSet,
    scalar: char,
) -> bool {
    scalar.is_ascii() || coverage.longCharacterIsMember(scalar as u32)
}

/// 同一字典的呈现身份；字形与行高都不能被相同style bit掩盖。
#[derive(Clone, Copy, PartialEq)]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct MarkdownTextAttributeVariant {
    pub(super) source_mode: bool,
    pub(super) table_row_height: Option<f64>,
    pub(super) list_role: MarkdownListGlyphRole,
}

/// Complete immutable dictionary for one style/face combination.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct MarkdownTextAttributeCacheEntry {
    pub(super) variant: MarkdownTextAttributeVariant,
    pub(super) styles: MarkdownPresentationStyles,
    pub(super) font: Retained<NSFont>,
    pub(super) attributes: Retained<MarkdownTextAttributes>,
}

/// Editor-local fixed-capacity LRU of derived attribute dictionaries.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct MarkdownTextAttributeCache {
    pub(super) entries: VecDeque<MarkdownTextAttributeCacheEntry>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl super::cache_owner::CachePayload for MarkdownTextAttributeCache {
    fn empty() -> Self {
        // 失效和重入路径不预留另一个 128-entry LRU，容量只由真实活动查询按需增长。
        Self {
            entries: VecDeque::new(),
        }
    }
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl Default for MarkdownTextAttributeCache {
    /// Reserves only the fixed cache capacity when the active editor is constructed.
    fn default() -> Self {
        // Capacity follows a constant policy rather than the source document size.
        Self {
            entries: VecDeque::with_capacity(MAX_NATIVE_MARKDOWN_ATTRIBUTE_CACHE_ENTRIES),
        }
    }
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl MarkdownTextAttributeCache {
    /// Reuses or creates one immutable dictionary while enforcing the fixed LRU bound.
    pub(super) fn get_or_insert(
        &mut self,
        styles: MarkdownPresentationStyles,
        font: &Retained<NSFont>,
        palette: &MarkdownTextAttributePalette,
        base_attributes: &MarkdownTextAttributes,
        variant: MarkdownTextAttributeVariant,
    ) -> Retained<MarkdownTextAttributes> {
        if variant.list_role == MarkdownListGlyphRole::Literal
            && styles.is_empty()
            && native_markdown_same_font(font, &palette.normal_font)
        {
            // Ordinary text uses the shared base dictionary without admitting a redundant cache entry.
            return base_attributes.into();
        }
        // A matching semantic style and exact face can reuse the same immutable dictionary.
        if let Some(index) = self.entries.iter().position(|entry| {
            // Font identity distinguishes Unicode fallback dictionaries with otherwise equal style bits.
            entry.variant == variant
                && entry.styles == styles
                && native_markdown_same_font(&entry.font, font)
        }) {
            // Promote the hit before returning a separate retained dictionary handle to TextKit.
            let entry = self
                .entries
                .remove(index)
                .expect("attribute cache index was found immediately above");
            let result = entry.attributes.clone();
            self.entries.push_back(entry);
            return result;
        }

        // A miss creates a small semantic dictionary; no full-document attributed string is copied.
        let attributes =
            build_markdown_text_attributes_with_row_height(styles, font, palette, variant);
        if self.entries.len() == MAX_NATIVE_MARKDOWN_ATTRIBUTE_CACHE_ENTRIES {
            // Remove the oldest dictionary before inserting a replacement at the bounded capacity.
            self.entries.pop_front();
        }
        self.entries.push_back(MarkdownTextAttributeCacheEntry {
            variant,
            styles,
            font: font.clone(),
            attributes: attributes.clone(),
        });
        attributes
    }
}

/// 行高属于本次属性查询；构造出的字典不持有投影或正文。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn build_markdown_text_attributes_with_row_height(
    styles: MarkdownPresentationStyles,
    font: &Retained<NSFont>,
    palette: &MarkdownTextAttributePalette,
    variant: MarkdownTextAttributeVariant,
) -> Retained<MarkdownTextAttributes> {
    let MarkdownTextAttributeVariant {
        source_mode,
        table_row_height,
        list_role,
    } = variant;
    let hidden_marker = !source_mode
        && (list_role == MarkdownListGlyphRole::Hidden
            || styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            || styles.contains(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER));
    let source_color = native_markdown_source_color(styles);
    let foreground = if source_mode {
        // Source syntax colors never conceal or shrink canonical Markdown delimiters.
        &source_color
    } else if list_role == MarkdownListGlyphRole::Hidden {
        // 只收起已成功替换任务框的原语法；透明色不在深浅主题间留下微小尾部笔画。
        &palette.list_hidden_foreground
    } else if hidden_marker {
        // Hidden syntax blends with the editor background while retaining canonical source.
        &palette.hidden_foreground
    } else if styles.contains(MarkdownPresentationStyles::TABLE_BOUNDARY) {
        // Table boundaries share the subdued structural border color.
        &palette.structure_border
    } else if styles.contains(MarkdownPresentationStyles::MARKER) {
        // Revealed Markdown markers remain distinct from the content they delimit.
        &palette.marker_foreground
    } else if styles.contains(MarkdownPresentationStyles::LINK) {
        // Link text uses the interactive accent color.
        &palette.accent_foreground
    } else if styles.contains(MarkdownPresentationStyles::BLOCK_QUOTE) {
        // Quoted text is muted independently of its block background.
        &palette.muted_foreground
    } else {
        // Other visible content uses the ordinary high-contrast text color.
        &palette.foreground
    };

    // NSNumber values are kept alive until the dictionary has retained them. The
    // constants are exported by AppKit and are immutable for the process lifetime.
    let underline = NSNumber::new_isize(NSUnderlineStyle::Single.0);
    let strikethrough = NSNumber::new_isize(NSUnderlineStyle::Single.0);
    // 原属性和分页绘图消费同一可见内容偏移；不让显露的单~或Source借用下标基线。
    let baseline = native_markdown_baseline_offset(styles, source_mode).map(NSNumber::new_f64);
    let paragraph_style = if source_mode {
        // Literal source uses uniform native line geometry, with no preview table/quote placeholders.
        None
    } else {
        // Inline preview derives its bounded block spacing and backgrounds from the semantic role.
        build_markdown_paragraph_style(styles, palette, table_row_height)
    };
    let mut keys: Vec<&NSAttributedStringKey> = Vec::with_capacity(8);
    let mut values: Vec<&AnyObject> = Vec::with_capacity(8);
    let (
        font_key,
        foreground_key,
        background_key,
        paragraph_key,
        underline_key,
        strike_key,
        baseline_key,
        // SAFETY: these AppKit attribute keys are immutable process-lifetime exported NSStrings.
    ) = unsafe {
        (
            NSFontAttributeName,
            NSForegroundColorAttributeName,
            NSBackgroundColorAttributeName,
            NSParagraphStyleAttributeName,
            NSUnderlineStyleAttributeName,
            NSStrikethroughStyleAttributeName,
            NSBaselineOffsetAttributeName,
        )
    };
    keys.push(font_key);
    keys.push(foreground_key);
    let font_object: &AnyObject = font;
    let foreground_object: &AnyObject = foreground;
    values.push(font_object);
    values.push(foreground_object);

    if !source_mode
        && let Some(glyph) = palette
            .list_glyphs
            .as_ref()
            .and_then(|glyphs| glyphs.info(list_role))
    {
        // 属性只覆盖完整的一个原unit；Source/活动字面/隐藏尾部永不附带旧glyphInfo。
        // SAFETY: AppKit导出的不可变key进程期有效，glyph由本editor palette保持正确类型与所有权。
        keys.push(unsafe { NSGlyphInfoAttributeName });
        values.push(glyph);
    }

    if !source_mode
        && !hidden_marker
        && (styles.contains(MarkdownPresentationStyles::INLINE_CODE)
            || styles.contains(MarkdownPresentationStyles::FENCED_CODE))
    {
        // Only visible code receives a code background; hidden syntax must stay visually absent.
        let background_object: &AnyObject = &palette.code_background;
        keys.push(background_key);
        values.push(background_object);
    }

    if let Some(paragraph_style) = paragraph_style.as_ref() {
        // Install block geometry only when this style has a semantic paragraph contract.
        let paragraph_object: &AnyObject = paragraph_style;
        keys.push(paragraph_key);
        values.push(paragraph_object);
    }

    // Hidden syntax uses a 0.1-point font as well as the editor background color.
    // This removes its layout width without replacing the canonical NSString or
    // creating a second rendered document. The active structure gets a distinct
    // style combination and therefore immediately returns the normal source font.
    if native_markdown_should_underline(styles, source_mode, hidden_marker) {
        // Visible explicit underline uses the native single-underline attribute.
        let underline_object: &AnyObject = &underline;
        keys.push(underline_key);
        values.push(underline_object);
    }

    if styles.contains(MarkdownPresentationStyles::STRIKETHROUGH) && !hidden_marker {
        // Strike visible content without revealing otherwise hidden syntax markers.
        let strikethrough_object: &AnyObject = &strikethrough;
        keys.push(strike_key);
        values.push(strikethrough_object);
    }

    if let Some(baseline) = baseline.as_ref() {
        // 只移动Preview可见内容；Source与显露的单~保持普通基线，不修改正文/选择或使用删除线。
        let baseline_object: &AnyObject = baseline;
        keys.push(baseline_key);
        values.push(baseline_object);
    }

    // `base_attributes` is intentionally not merged by copying a dictionary. The small
    // style dictionary contains the complete normal font/color pair plus only the
    // semantic deltas, keeping each cache entry independent and bounded.
    MarkdownTextAttributes::from_slices(&keys, &values)
}

/// 基线以pt表达原下标属性；普通文字、隐藏语法和Source不产生内容偏移。
pub(super) fn native_markdown_baseline_offset(
    styles: MarkdownPresentationStyles,
    source_mode: bool,
) -> Option<f64> {
    if source_mode || styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER) {
        return None;
    }
    if styles.contains(MarkdownPresentationStyles::SUBSCRIPT)
        && !styles.contains(MarkdownPresentationStyles::MARKER)
    {
        Some(-3.0)
    } else {
        None // 普通文字和显露的下标marker保持原字体基线，不新增一个零值Native属性。
    }
}

/// 链接在两种模式都保持可辨识的下划线；显式 underline 仅在 preview解释。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_should_underline(
    styles: MarkdownPresentationStyles,
    source_mode: bool,
    hidden_marker: bool,
) -> bool {
    // 隐藏语法不能留下线条；source 的普通<u>仍按字面保留，不伪装已渲染HTML。
    !hidden_marker
        && (styles.contains(MarkdownPresentationStyles::LINK)
            || (!source_mode && styles.contains(MarkdownPresentationStyles::UNDERLINE)))
}

/// 为一个完整、有界的可见单元格附加换行/对齐；同一结果同时用于测量和绘制。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_wrapped_table_cell_text(
    source: &NSAttributedString,
    alignment: MarkdownTableAlignment,
) -> Retained<NSAttributedString> {
    let paragraph = NSMutableParagraphStyle::new();
    // 这里是用户可见内容，始终允许换行；不能沿用 source placeholder 的 clipping。
    paragraph.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping);
    paragraph.setAlignment(match alignment {
        // 普通列沿前缘对齐，完整content rect保留两侧一致padding。
        MarkdownTableAlignment::Left => NSTextAlignment::Left,
        // 居中逐视觉行计算，不能把一份紧缩测量框整体平移当成多行居中。
        MarkdownTableAlignment::Center => NSTextAlignment::Center,
        // 右列沿尾缘排版；仍由同一个完整宽度决定换行。
        MarkdownTableAlignment::Right => NSTextAlignment::Right,
    });
    let result = NSMutableAttributedString::initWithAttributedString(
        NSMutableAttributedString::alloc(),
        source,
    );
    // SAFETY: AppKit导出的属性key进程内不可变，字典持有本地创建的正确NSParagraphStyle类型。
    let attributes = unsafe {
        MarkdownTextAttributes::from_slices(&[NSParagraphStyleAttributeName], &[&*paragraph])
    };
    // SAFETY: range恰好覆盖新建结果的完整UTF-16长度，不借用原文或越过Native字符串边界。
    unsafe { result.addAttributes_range(&attributes, NSRange::new(0, result.length())) };
    result.into_super()
}

/// Derives block geometry and returns no object for ordinary inline-only runs.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn build_markdown_paragraph_style(
    styles: MarkdownPresentationStyles,
    _palette: &MarkdownTextAttributePalette,
    table_row_height: Option<f64>,
) -> Option<Retained<NSMutableParagraphStyle>> {
    let hidden_marker = styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER);
    if hidden_marker && styles.contains(MarkdownPresentationStyles::FENCED_CODE) {
        // Concealed fence markers must not acquire the visible code block's background or height.
        return None;
    }

    // Accumulate only relevant paragraph semantics; ordinary text needs no stored style object.
    let paragraph = NSMutableParagraphStyle::new();
    let mut has_semantic_layout = false;

    if styles.contains(MarkdownPresentationStyles::TABLE) {
        // Preserve the source table column alignment in its layout placeholder.
        paragraph.setAlignment(
            if styles.contains(MarkdownPresentationStyles::TABLE_ALIGN_CENTER) {
                // Center-aligned Markdown columns keep centered native geometry.
                NSTextAlignment::Center
            } else if styles.contains(MarkdownPresentationStyles::TABLE_ALIGN_RIGHT) {
                // Right-aligned Markdown columns keep right-aligned native geometry.
                NSTextAlignment::Right
            } else {
                // The Markdown default is left alignment when no explicit alignment bit is set.
                NSTextAlignment::Left
            },
        );
        has_semantic_layout = true;
    }

    if styles.contains(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER) {
        // 完整 overlay 负责可见格文字；隐藏的原始行只占同一批测量得出的单个行槽。
        let height = table_row_height.unwrap_or(NATIVE_MARKDOWN_TABLE_ROW_HEIGHT);
        paragraph.setMinimumLineHeight(height);
        paragraph.setMaximumLineHeight(height);
        // 这里只裁不可见 source placeholder，避免极长原始行产生多个占位软换行；可见格另行完整换行。
        paragraph.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByClipping);
        paragraph.setParagraphSpacing(0.0);
        paragraph.setAlignment(
            if styles.contains(MarkdownPresentationStyles::TABLE_ALIGN_CENTER) {
                // Center-aligned Markdown columns keep centered native geometry.
                NSTextAlignment::Center
            } else if styles.contains(MarkdownPresentationStyles::TABLE_ALIGN_RIGHT) {
                // Right-aligned Markdown columns keep right-aligned native geometry.
                NSTextAlignment::Right
            } else {
                // The Markdown default is left alignment when no explicit alignment bit is set.
                NSTextAlignment::Left
            },
        );
        return Some(paragraph);
    }

    if styles.contains(MarkdownPresentationStyles::HEADING_1)
        || styles.contains(MarkdownPresentationStyles::HEADING_2)
        || styles.contains(MarkdownPresentationStyles::HEADING_3)
        || styles.contains(MarkdownPresentationStyles::HEADING_4)
        || styles.contains(MarkdownPresentationStyles::HEADING_5)
        || styles.contains(MarkdownPresentationStyles::HEADING_6)
    {
        // Visible headings gain vertical hierarchy while preserving their canonical source spans.
        paragraph.setParagraphSpacingBefore(8.0);
        paragraph.setParagraphSpacing(6.0);
        paragraph.setLineHeightMultiple(1.08);
        has_semantic_layout = true;
    }

    if hidden_marker && styles.contains(MarkdownPresentationStyles::HORIZONTAL_RULE) {
        // A hidden rule marker reserves a thin native border block rather than visible Markdown dashes.
        paragraph.setMinimumLineHeight(12.0);
        paragraph.setMaximumLineHeight(12.0);
        paragraph.setParagraphSpacingBefore(6.0);
        paragraph.setParagraphSpacing(6.0);
        // 标准fragment保留行槽，有限背景hook画细线，不发布会触发兼容模式的textBlocks。
        return Some(paragraph);
    }

    if hidden_marker && styles.contains(MarkdownPresentationStyles::TABLE_DELIMITER) {
        // Delimiter rows consume only the dedicated separator height in preview.
        paragraph.setMinimumLineHeight(NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT);
        paragraph.setMaximumLineHeight(NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT);
        paragraph.setParagraphSpacing(0.0);
        return Some(paragraph);
    }

    if styles.contains(MarkdownPresentationStyles::FENCED_CODE) {
        // Visible fenced code receives stable line height, padding and its own background block.
        paragraph.setMinimumLineHeight(22.0);
        paragraph.setParagraphSpacing(0.0);
        // 真实indent与段距参与软换行/选区；背景只画已准备矩形，不承担排版。
        apply_preview_block_insets(&paragraph, 6.0);
        return Some(paragraph);
    }

    if styles.contains(MarkdownPresentationStyles::BLOCK_QUOTE) {
        // Quotes receive their block border/background without storing another attributed document.
        paragraph.setLineHeightMultiple(1.2);
        paragraph.setParagraphSpacing(2.0);
        // 原2点段后距保留，额外7点包含引用边框和padding。
        apply_preview_block_insets(&paragraph, 7.0);
        return Some(paragraph);
    }

    // Only earlier table/heading semantics retain this object; plain runs return no paragraph style.
    has_semantic_layout.then_some(paragraph)
}

#[cfg(test)]
mod underline_tests {
    use super::{MarkdownPresentationStyles as Styles, native_markdown_should_underline};

    #[test]
    fn preview_and_source_links_retain_a_visible_underline() {
        // 旧CSS链接的可辨识能力在两种模式都保留，隐藏destination不得残留线条。
        for source in [false, true] {
            assert!(native_markdown_should_underline(
                Styles::LINK,
                source,
                false
            ));
            assert!(!native_markdown_should_underline(
                Styles::LINK,
                source,
                true
            ));
        }
    }

    #[test]
    fn explicit_underline_is_rendered_only_in_preview_and_never_for_hidden_syntax() {
        // source保留字面<u>语义；普通字和隐藏标记不被误当成链接/下划线。
        assert!(native_markdown_should_underline(
            Styles::UNDERLINE,
            false,
            false
        ));
        assert!(!native_markdown_should_underline(
            Styles::UNDERLINE,
            true,
            false
        ));
        assert!(!native_markdown_should_underline(
            Styles::UNDERLINE,
            false,
            true
        ));
        assert!(!native_markdown_should_underline(
            Styles::default(),
            false,
            false
        ));
    }
}
