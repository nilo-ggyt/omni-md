//! Owns bounded native font/attribute caches and derived paragraph styling.
//! No cache retains canonical document bytes or any session beyond its surface.

use super::*;

/// Fixed colors and fonts for bounded TextKit attribute dictionaries.
///
/// Resources are created once per editor. A bounded LRU shares semantic dictionaries
/// without constructing a complete attributed document or churning fonts on scroll.
pub(super) struct MarkdownTextAttributePalette {
    pub(super) normal_font: Retained<NSFont>,
    pub(super) strong_font: Retained<NSFont>,
    pub(super) source_strong_font: Retained<NSFont>,
    pub(super) fixed_font: Retained<NSFont>,
    pub(super) table_header_font: Retained<NSFont>,
    pub(super) hidden_font: Retained<NSFont>,
    pub(super) heading_fonts: Vec<Retained<NSFont>>,
    pub(super) foreground: Retained<NSColor>,
    pub(super) muted_foreground: Retained<NSColor>,
    pub(super) accent_foreground: Retained<NSColor>,
    pub(super) marker_foreground: Retained<NSColor>,
    pub(super) hidden_foreground: Retained<NSColor>,
    pub(super) code_background: Retained<NSColor>,
    pub(super) quote_background: Retained<NSColor>,
    pub(super) structure_border: Retained<NSColor>,
    pub(super) table_header_background: Retained<NSColor>,
    pub(super) table_row_background: Retained<NSColor>,
    pub(super) table_active_background: Retained<NSColor>,
}

impl MarkdownTextAttributePalette {
    /// Acquires a fixed native palette owned only by this active editor.
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
            strong_font: NSFont::boldSystemFontOfSize(15.0),
            source_strong_font: NSFont::monospacedSystemFontOfSize_weight(14.0, semibold_weight),
            normal_font,
            fixed_font,
            table_header_font,
            hidden_font: NSFont::systemFontOfSize(0.1),
            heading_fonts,
            foreground: NSColor::colorWithSRGBRed_green_blue_alpha(0.86, 0.88, 0.92, 1.0),
            muted_foreground: NSColor::colorWithSRGBRed_green_blue_alpha(0.58, 0.66, 0.78, 1.0),
            accent_foreground: NSColor::colorWithSRGBRed_green_blue_alpha(0.40, 0.68, 1.0, 1.0),
            marker_foreground: NSColor::colorWithSRGBRed_green_blue_alpha(0.48, 0.56, 0.68, 1.0),
            hidden_foreground: NSColor::colorWithSRGBRed_green_blue_alpha(0.075, 0.084, 0.11, 1.0),
            code_background: NSColor::colorWithSRGBRed_green_blue_alpha(0.13, 0.15, 0.20, 1.0),
            quote_background: NSColor::colorWithSRGBRed_green_blue_alpha(0.10, 0.12, 0.16, 1.0),
            structure_border: NSColor::colorWithSRGBRed_green_blue_alpha(0.28, 0.34, 0.44, 1.0),
            table_header_background: NSColor::colorWithSRGBRed_green_blue_alpha(
                0.105, 0.12, 0.16, 1.0,
            ),
            table_row_background: NSColor::colorWithSRGBRed_green_blue_alpha(
                0.075, 0.084, 0.11, 1.0,
            ),
            table_active_background: NSColor::colorWithSRGBRed_green_blue_alpha(
                0.13, 0.20, 0.31, 1.0,
            ),
        }
    }
}

/// Detect a heading role in the fixed semantic bit domain without allocating a style collection.
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
                // Code and formula spans require stable monospaced glyph widths.
                (styles.contains(MarkdownPresentationStyles::INLINE_CODE)
                    || styles.contains(MarkdownPresentationStyles::FENCED_CODE)
                    || styles.contains(MarkdownPresentationStyles::FORMULA))
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

    if !hidden_marker && styles.contains(MarkdownPresentationStyles::EMPHASIS) {
        // Add italic traits to the selected semantic base while preserving its point size.
        let descriptor = base_font.fontDescriptor();
        let italic_descriptor = descriptor.fontDescriptorWithSymbolicTraits(
            descriptor.symbolicTraits() | NSFontDescriptorSymbolicTraits::TraitItalic,
        );
        NSFont::fontWithDescriptor_size(&italic_descriptor, base_font.pointSize())
            .unwrap_or_else(|| base_font.clone())
    } else {
        // Nonitalic and hidden spans reuse the existing retained face without a new descriptor.
        base_font.clone()
    }
}

/// One style-to-face cache entry, including immutable scalar coverage.
pub(super) struct MarkdownStyleFontEntry {
    pub(super) source_mode: bool,
    pub(super) styles: MarkdownPresentationStyles,
    pub(super) font: Retained<NSFont>,
    pub(super) coverage: Retained<NSCharacterSet>,
}

/// One bounded Unicode fallback entry tied to its semantic base face.
pub(super) struct MarkdownFallbackFontEntry {
    pub(super) base_font: Retained<NSFont>,
    pub(super) font: Retained<NSFont>,
    pub(super) coverage: Retained<NSCharacterSet>,
}

/// Selected face and coverage used to bound an adjacent compatible UTF-16 run.
pub(super) struct MarkdownResolvedFont {
    pub(super) font: Retained<NSFont>,
    pub(super) base_coverage: Retained<NSCharacterSet>,
    pub(super) fallback_coverage: Option<Retained<NSCharacterSet>>,
}

/// Editor-owned LRU caches; their fixed capacities do not scale with document length.
#[derive(Default)]
pub(super) struct MarkdownFontSelectionCache {
    pub(super) style_fonts: VecDeque<MarkdownStyleFontEntry>,
    pub(super) fallback_fonts: VecDeque<MarkdownFallbackFontEntry>,
}

impl MarkdownFontSelectionCache {
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
pub(super) fn native_markdown_same_font(left: &NSFont, right: &NSFont) -> bool {
    std::ptr::eq(left, right)
}

/// Treats ASCII as a base-font fast path and checks full scalar coverage otherwise.
pub(super) fn native_markdown_character_set_covers(
    coverage: &NSCharacterSet,
    scalar: char,
) -> bool {
    scalar.is_ascii() || coverage.longCharacterIsMember(scalar as u32)
}

/// Complete immutable dictionary for one style/face combination.
pub(super) struct MarkdownTextAttributeCacheEntry {
    pub(super) source_mode: bool,
    pub(super) styles: MarkdownPresentationStyles,
    pub(super) font: Retained<NSFont>,
    pub(super) attributes: Retained<MarkdownTextAttributes>,
}

/// Editor-local fixed-capacity LRU of derived attribute dictionaries.
pub(super) struct MarkdownTextAttributeCache {
    pub(super) entries: VecDeque<MarkdownTextAttributeCacheEntry>,
}

impl Default for MarkdownTextAttributeCache {
    /// Reserves only the fixed cache capacity when the active editor is constructed.
    fn default() -> Self {
        // Capacity follows a constant policy rather than the source document size.
        Self {
            entries: VecDeque::with_capacity(MAX_NATIVE_MARKDOWN_ATTRIBUTE_CACHE_ENTRIES),
        }
    }
}

impl MarkdownTextAttributeCache {
    /// Reuses or creates one immutable dictionary while enforcing the fixed LRU bound.
    pub(super) fn get_or_insert(
        &mut self,
        styles: MarkdownPresentationStyles,
        font: &Retained<NSFont>,
        palette: &MarkdownTextAttributePalette,
        base_attributes: &MarkdownTextAttributes,
        source_mode: bool,
    ) -> Retained<MarkdownTextAttributes> {
        if styles.is_empty() && native_markdown_same_font(font, &palette.normal_font) {
            // Ordinary text uses the shared base dictionary without admitting a redundant cache entry.
            return base_attributes.into();
        }
        // A matching semantic style and exact face can reuse the same immutable dictionary.
        if let Some(index) = self.entries.iter().position(|entry| {
            // Font identity distinguishes Unicode fallback dictionaries with otherwise equal style bits.
            entry.source_mode == source_mode
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
        let attributes = build_markdown_text_attributes(styles, font, palette, source_mode);
        if self.entries.len() == MAX_NATIVE_MARKDOWN_ATTRIBUTE_CACHE_ENTRIES {
            // Remove the oldest dictionary before inserting a replacement at the bounded capacity.
            self.entries.pop_front();
        }
        self.entries.push_back(MarkdownTextAttributeCacheEntry {
            source_mode,
            styles,
            font: font.clone(),
            attributes: attributes.clone(),
        });
        attributes
    }
}

/// Creates one complete bounded dictionary of font/color and applicable semantic deltas.
pub(super) fn build_markdown_text_attributes(
    styles: MarkdownPresentationStyles,
    font: &Retained<NSFont>,
    palette: &MarkdownTextAttributePalette,
    source_mode: bool,
) -> Retained<MarkdownTextAttributes> {
    let hidden_marker = !source_mode
        && (styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            || styles.contains(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER));
    let source_color = native_markdown_source_color(styles);
    let foreground = if source_mode {
        // Source syntax colors never conceal or shrink canonical Markdown delimiters.
        &source_color
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
    let baseline = NSNumber::new_f64(4.0);
    let paragraph_style = if source_mode {
        // Literal source uses uniform native line geometry, with no preview table/quote placeholders.
        None
    } else {
        // Inline preview derives its bounded block spacing and backgrounds from the semantic role.
        build_markdown_paragraph_style(styles, palette)
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
    if ((!source_mode && styles.contains(MarkdownPresentationStyles::UNDERLINE))
        || (source_mode && styles.contains(MarkdownPresentationStyles::LINK)))
        && !hidden_marker
    {
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

    if !source_mode && styles.contains(MarkdownPresentationStyles::SUPERSCRIPT) && !hidden_marker {
        // Superscript offsets the baseline while keeping bytes and selection in the same model.
        let baseline_object: &AnyObject = &baseline;
        keys.push(baseline_key);
        values.push(baseline_object);
    }

    // `base_attributes` is intentionally not merged by copying a dictionary. The small
    // style dictionary contains the complete normal font/color pair plus only the
    // semantic deltas, keeping each cache entry independent and bounded.
    MarkdownTextAttributes::from_slices(&keys, &values)
}

/// Derives block geometry and returns no object for ordinary inline-only runs.
pub(super) fn build_markdown_paragraph_style(
    styles: MarkdownPresentationStyles,
    palette: &MarkdownTextAttributePalette,
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
        // Overlay cells need a fixed-height row slot independent of their concealed source glyph widths.
        paragraph.setMinimumLineHeight(NATIVE_MARKDOWN_TABLE_ROW_HEIGHT);
        paragraph.setMaximumLineHeight(NATIVE_MARKDOWN_TABLE_ROW_HEIGHT);
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
        let block = markdown_rule_text_block(&palette.structure_border);
        paragraph.setTextBlocks(&NSArray::from_retained_slice(&[block]));
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
        let block = markdown_box_text_block(
            Some(&palette.code_background),
            // Table boundaries share the subdued structural border color.
            &palette.structure_border,
            0.0,
            6.0,
        );
        paragraph.setTextBlocks(&NSArray::from_retained_slice(&[block]));
        return Some(paragraph);
    }

    if styles.contains(MarkdownPresentationStyles::BLOCK_QUOTE) {
        // Quotes receive their block border/background without storing another attributed document.
        paragraph.setLineHeightMultiple(1.2);
        paragraph.setParagraphSpacing(2.0);
        let block = markdown_box_text_block(
            Some(&palette.quote_background),
            // Table boundaries share the subdued structural border color.
            &palette.structure_border,
            1.0,
            6.0,
        );
        paragraph.setTextBlocks(&NSArray::from_retained_slice(&[block]));
        return Some(paragraph);
    }

    // Only earlier table/heading semantics retain this object; plain runs return no paragraph style.
    has_semantic_layout.then_some(paragraph)
}

/// Constructs one full-width native block for code or quote decoration.
pub(super) fn markdown_box_text_block(
    background: Option<&NSColor>,
    border: &NSColor,
    border_width: f64,
    padding: f64,
) -> Retained<NSTextBlock> {
    let block = NSTextBlock::new();
    block.setContentWidth_type(100.0, NSTextBlockValueType::PercentageValueType);
    block.setWidth_type_forLayer(
        border_width,
        NSTextBlockValueType::AbsoluteValueType,
        NSTextBlockLayer::Border,
    );
    block.setBorderColor(Some(border));
    block.setWidth_type_forLayer(
        padding,
        NSTextBlockValueType::AbsoluteValueType,
        NSTextBlockLayer::Padding,
    );
    block.setBackgroundColor(background);
    block
}

/// Constructs a single native bottom border for a Markdown rule placeholder.
pub(super) fn markdown_rule_text_block(color: &NSColor) -> Retained<NSTextBlock> {
    let block = NSTextBlock::new();
    block.setContentWidth_type(100.0, NSTextBlockValueType::PercentageValueType);
    block.setWidth_type_forLayer_edge(
        1.0,
        NSTextBlockValueType::AbsoluteValueType,
        NSTextBlockLayer::Border,
        NSRectEdge::MinY,
    );
    block.setBorderColor_forEdge(Some(color), NSRectEdge::MinY);
    block
}
