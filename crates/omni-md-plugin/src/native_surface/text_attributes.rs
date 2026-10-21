//! Owns bounded native font/attribute caches and derived paragraph styling.
//! No cache retains canonical document bytes or any session beyond its surface.

use super::paged_fonts::NativeMarkdownPagedFont;
use super::paged_line_fonts::NativeMarkdownLineFontError;
use super::*;
use objc2_app_kit::{NSFontManager, NSFontTraitMask};
use objc2_core_text::CTFont;

/// Create a document instance from the actual system face's CGFont, preserving
/// its point size and outlines while removing the UI usage role. Creation does
/// not substitute a user-preferred family; a failed candidate is returned.
///
/// # Errors
/// Reject an instance whose name tables, axes, matrix, size, or semantic features
/// cannot be preserved; another face cannot supply missing authority.
pub fn native_markdown_document_font(
    points: f64,
) -> Result<Retained<NSFont>, NativeMarkdownLineFontError> {
    into_document_font(NSFont::systemFontOfSize(points))
}

/// Keep the original monospaced system role and size for Source/code. Chinese
/// cascading starts from the converted actual document instance.
///
/// # Errors
/// Reject lost monospaced semantics or source identity instead of reconstructing
/// the font by name or switching its family.
pub fn native_markdown_document_fixed_font(
    points: f64,
) -> Result<Retained<NSFont>, NativeMarkdownLineFontError> {
    // SAFETY: This public system weight is an input to the existing monospaced
    // factory; the call does not modify the user's font preferences.
    into_document_font(NSFont::monospacedSystemFontOfSize_weight(points, unsafe {
        NSFontWeightRegular
    }))
}

/// Transfer the factory's local font claim through the public NSFont/CTFont
/// bridge. The Native font owner validates real geometry/source identity; this
/// factory retains no second font graph after returning its document claim.
fn into_document_font(
    font: Retained<NSFont>,
) -> Result<Retained<NSFont>, NativeMarkdownLineFontError> {
    let native: &CTFont = font.as_ref();
    let document = NativeMarkdownPagedFont::document_instance(native)?;
    let retained: Retained<CTFont> = document.into();
    // SAFETY: Apple defines the CTFont/NSFont toll-free bridge. Preserve the
    // existing +1 claim and change only its Rust view type.
    Ok(unsafe { Retained::cast_unchecked(retained) })
}

/// Derive size/italic from this actual document instance without rematching a
/// private PostScript name. A failed style conversion rejects the candidate;
/// ordinary typography cannot silently replace the requested semantic face.
fn derive_document_font(
    base: &NSFont,
    points: f64,
    italic: bool,
) -> Result<Retained<NSFont>, NativeMarkdownLineFontError> {
    // Paged, MainThreadOnly storage, and Native references resolve style on the
    // original main thread. A wrong-thread caller is a programming error, not a
    // reason to report missing italic and return an ordinary face.
    let mtm = MainThreadMarker::new().expect("原生样式字体只能在主线程转换");
    let styled = if italic {
        // Ask the existing AppKit font manager to derive the actual trait from
        // this face; the final identity/feature check below remains mandatory.
        let manager = NSFontManager::sharedFontManager(mtm);
        manager.convertFont_toHaveTrait(base, NSFontTraitMask::ItalicFontMask)
    } else {
        // A size-only derivation starts from the same retained base face.
        base.into()
    };
    let resized = if styled.pointSize() == points {
        // An unchanged size already has the correct native optical-axis choice.
        styled
    } else {
        let native: &CTFont = styled.as_ref();
        // SAFETY: Retain the immutable original CTFont. A null matrix preserves
        // its transform and None adds no replacement descriptor. Core Text
        // derives optical size for the requested points without hand-setting
        // old axes, kerning, or feature overrides.
        let resized = unsafe { native.copy_with_attributes(points, std::ptr::null(), None) };
        let retained: Retained<CTFont> = resized.into();
        // SAFETY: Transfer the same +1 through the public CTFont/NSFont bridge;
        // no font is reconstructed by name.
        unsafe { Retained::cast_unchecked(retained) }
    };
    // The derived claim is published only after actual size, traits, source,
    // axes, and features satisfy the font owner's existing validation contract.
    NativeMarkdownPagedFont::require_derived_style(
        base.as_ref(),
        resized.as_ref(),
        points,
        italic,
    )?;
    Ok(resized)
}

/// TextKit reference attributes and actual Paged paint share the existing
/// preview colors, preventing independent typography policies from drifting.
pub(super) const NATIVE_MARKDOWN_LINK_COLOR: [f64; 4] = [0.40, 0.68, 1.0, 1.0];
pub(super) const NATIVE_MARKDOWN_CODE_BACKGROUND: [f64; 4] = [0.13, 0.15, 0.20, 1.0];
pub(super) const NATIVE_MARKDOWN_MARKER_COLOR: [f64; 4] = [0.48, 0.56, 0.68, 1.0];
/// Quotes change ink without replacing the actual face; attributes and Paged
/// paint use the same established quote color.
pub(super) const NATIVE_MARKDOWN_QUOTE_COLOR: [f64; 4] = [0.58, 0.66, 0.78, 1.0];
/// CPU table paint preserves the existing NSColor policy and interface colors;
/// mounting a paged view does not introduce another theme.
pub(super) const NATIVE_MARKDOWN_TABLE_BORDER: [f64; 4] = [0.28, 0.34, 0.44, 1.0];
pub(super) const NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND: [f64; 4] = [0.105, 0.12, 0.16, 1.0];
pub(super) const NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND: [f64; 4] = [0.075, 0.084, 0.11, 1.0];

/// Finite fonts/colors belong to one active editor. Paged uses the common
/// policy; TextKit additionally acquires list glyphs at its assembly boundary.
/// Palette destruction returns these claims; scrolling creates no new owner.
pub(super) struct MarkdownTextAttributePalette {
    // Paged needs no NSGlyphInfo claims. The TextKit constructor acquires all
    // five list glyphs together; unsupported symbols retain the existing literal
    // None policy rather than authorizing concealed syntax without a glyph.
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
    /// Acquire fixed common font/color resources at the original Native assembly
    /// boundary, before any document/RefCell borrow. Paged creates no TextKit-only
    /// glyph claims, and later text getters need not initialize these resources.
    pub(super) fn new(normal_font: Retained<NSFont>) -> Result<Self, NativeMarkdownLineFontError> {
        // The mounting owner supplies the same actual body base across modes.
        // Each weight/size is selected by its original AppKit factory before
        // document conversion; a 15pt optical axis cannot authorize a 28pt
        // heading, and semibold table headers must not become bold.
        let fixed_font = native_markdown_document_fixed_font(14.0)?;
        let strong_font = into_document_font(NSFont::boldSystemFontOfSize(15.0))?;
        // SAFETY: Preserve the original public 14pt semibold request. Conversion
        // removes UI usage and checks the resulting actual axes individually.
        let table_header_font =
            into_document_font(NSFont::systemFontOfSize_weight(14.0, unsafe {
                NSFontWeightSemibold
            }))?;
        let heading_fonts = [28.0, 24.0, 20.0, 18.0, 16.0, 15.0]
            .into_iter()
            .map(|points| into_document_font(NSFont::boldSystemFontOfSize(points)))
            .collect::<Result<Vec<_>, _>>()?;
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        // SAFETY: Source emphasis keeps the original public monospaced 14pt
        // semibold factory; it does not inherit Preview's different bold axes.
        let source_strong_font =
            into_document_font(NSFont::monospacedSystemFontOfSize_weight(14.0, unsafe {
                NSFontWeightSemibold
            }))?;
        // Concealed syntax keeps its actual 0.1pt document face. Its source bytes
        // remain intact, without depending on a special tiny UI-font policy.
        let hidden_font = native_markdown_document_font(0.1)?;
        Ok(Self {
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            list_glyphs: None,
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            list_hidden_foreground: NSColor::clearColor(),
            strong_font,
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            source_strong_font,
            normal_font,
            fixed_font,
            table_header_font,
            hidden_font,
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
        })
    }

    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    /// Acquire TextKit's full resource set at storage assembly, before borrowing
    /// attribute/document slots. The five list glyphs succeed together or remain
    /// literal together, owned by this editor's original palette until release.
    pub(super) fn new_for_textkit(
        normal_font: Retained<NSFont>,
    ) -> Result<Self, NativeMarkdownLineFontError> {
        let mut palette = Self::new(normal_font)?;
        // This is the real TextKit construction path; attribute getters, first
        // scroll, and lazy queries never initialize missing glyph resources.
        palette.list_glyphs = MarkdownListMarkerGlyphPalette::new();
        Ok(palette)
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
) -> Result<Retained<NSFont>, NativeMarkdownLineFontError> {
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
        // Add actual italic while preserving Source's size, weight, and mono
        // role; neither private-name rematching nor a plain-face fallback applies.
        derive_document_font(font, font.pointSize(), true)
    } else {
        // Ordinary source reuses its fixed retained face.
        Ok(font.clone())
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
) -> Result<Retained<NSFont>, NativeMarkdownLineFontError> {
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
                // Only code semantics choose monospace. Ordinary dollar text
                // retains its established body/emphasis typography.
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
        // Italic and subscript may coexist. Derive both from the same original
        // face; the requested size and trait must both actually succeed.
        let point_size = if subscript_content {
            base_font.pointSize() * 0.75
        } else {
            base_font.pointSize() // Italic keeps its size; literal Source does not use this Preview path.
        };
        derive_document_font(
            base_font,
            point_size,
            styles.contains(MarkdownPresentationStyles::EMPHASIS),
        )
    } else {
        // Nonitalic and hidden spans reuse the existing retained face without a new descriptor.
        Ok(base_font.clone())
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
    /// Reentry while the cache owner is on an outer Native stack uses the same
    /// font policy without allocating or publishing another LRU owner.
    pub(super) fn resolve_uncached(
        styles: MarkdownPresentationStyles,
        scalar: char,
        palette: &MarkdownTextAttributePalette,
        source_mode: bool,
    ) -> Result<MarkdownResolvedFont, NativeMarkdownLineFontError> {
        let base_font = if source_mode {
            // Reentry cannot change literal Source's monospaced role to Preview.
            native_markdown_source_font(styles, palette)
        } else {
            // Preview retains its existing heading/hidden/table/emphasis priority.
            native_markdown_base_font(styles, palette)
        }?;
        let base_coverage = base_font.coveredCharacterSet();
        if (!source_mode && styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER))
            || native_markdown_character_set_covers(&base_coverage, scalar)
        {
            // Keep the actual base when it covers the complete scalar. A plain
            // body dictionary cannot stand in for a requested semantic face.
            return Ok(MarkdownResolvedFont {
                font: base_font,
                base_coverage,
                fallback_coverage: None,
            });
        }
        // Core Text resolves an uncovered complete scalar under the original
        // policy. Temporary claims remain local to this call's stack.
        let font = native_markdown_fallback_font(&base_font, scalar);
        let coverage = font.coveredCharacterSet();
        Ok(MarkdownResolvedFont {
            font,
            base_coverage,
            fallback_coverage: Some(coverage),
        })
    }

    /// Resolves one scalar and promotes reused fallback faces within the bounded LRU.
    pub(super) fn resolve(
        &mut self,
        styles: MarkdownPresentationStyles,
        scalar: char,
        palette: &MarkdownTextAttributePalette,
        source_mode: bool,
    ) -> Result<MarkdownResolvedFont, NativeMarkdownLineFontError> {
        // Resolve semantic typography before testing coverage for this scalar.
        let (base_font, base_coverage) = self.style_font(styles, palette, source_mode)?;
        if (!source_mode && styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER))
            || native_markdown_character_set_covers(&base_coverage, scalar)
        {
            // Hidden syntax and supported scalars need no fallback font or fallback-run cache.
            return Ok(MarkdownResolvedFont {
                font: base_font,
                base_coverage,
                fallback_coverage: None,
            });
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
            return Ok(resolved);
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
        Ok(MarkdownResolvedFont {
            font: fallback_font,
            base_coverage,
            fallback_coverage: Some(fallback_coverage),
        })
    }

    /// Resolves a semantic font once per style combination with bounded LRU promotion.
    pub(super) fn style_font(
        &mut self,
        styles: MarkdownPresentationStyles,
        palette: &MarkdownTextAttributePalette,
        source_mode: bool,
    ) -> Result<(Retained<NSFont>, Retained<NSCharacterSet>), NativeMarkdownLineFontError> {
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
            return Ok(result);
        }

        // An unseen semantic combination acquires one face and its immutable coverage set.
        let font = if source_mode {
            // Literal source keeps monospaced line geometry while exposing semantic bold/italic syntax.
            native_markdown_source_font(styles, palette)
        } else {
            // Inline preview keeps heading sizes and concealed marker typography.
            native_markdown_base_font(styles, palette)
        }?;
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
        Ok((font, coverage))
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

/// Dictionary identity includes glyph role and row height; equal style bits alone
/// cannot authorize different presentation geometry.
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
        // Invalidated/reentrant owners reserve no second LRU. Capacity grows
        // only for real queries against the active owner.
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

/// Row height belongs to this attribute query. The complete dictionary owns
/// Native attribute values, never a document or projection.
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
        // Conceal only syntax whose task glyph was successfully substituted;
        // transparency avoids residual tiny strokes across light/dark themes.
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
    // Attributes and Paged paint share the visible-content offset. Revealed '~'
    // markers and literal Source cannot inherit a subscript baseline.
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
        // A substitution covers its complete original unit. Source, active
        // literal syntax, and concealed tails never inherit stale glyph info.
        // SAFETY: The immutable AppKit key has process lifetime; this editor's
        // palette owns the correctly typed glyph claim throughout construction.
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
    // This reduces its layout width without replacing the canonical NSString or
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
        // Shift visible Preview content only. Source and revealed '~' markers
        // keep their normal baseline, text, selection, and strike semantics.
        let baseline_object: &AnyObject = baseline;
        keys.push(baseline_key);
        values.push(baseline_object);
    }

    // `base_attributes` is intentionally not merged by copying a dictionary. The small
    // style dictionary contains the complete normal font/color pair plus only the
    // semantic deltas, keeping each cache entry independent and bounded.
    MarkdownTextAttributes::from_slices(&keys, &values)
}

/// Express the existing subscript baseline in points. Plain text, concealed
/// syntax, and literal Source receive no content offset.
pub(super) fn native_markdown_baseline_offset(
    styles: MarkdownPresentationStyles,
    source_mode: bool,
) -> Option<f64> {
    if source_mode || styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER) {
        // Source remains literal and concealed delimiters must not contribute
        // visible shifted ink or an extra Native attribute.
        return None;
    }
    if styles.contains(MarkdownPresentationStyles::SUBSCRIPT)
        && !styles.contains(MarkdownPresentationStyles::MARKER)
    {
        Some(-3.0) // Apply the existing visible-subscript offset only to content.
    } else {
        None // Plain text/revealed markers retain their baseline without a redundant zero attribute.
    }
}

/// Links remain recognizable through underlining in both modes; explicit
/// underline semantics render only in Preview.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_should_underline(
    styles: MarkdownPresentationStyles,
    source_mode: bool,
    hidden_marker: bool,
) -> bool {
    // Concealed syntax leaves no underline. Source keeps literal <u> text rather
    // than pretending its HTML-like syntax has already rendered.
    !hidden_marker
        && (styles.contains(MarkdownPresentationStyles::LINK)
            || (!source_mode && styles.contains(MarkdownPresentationStyles::UNDERLINE)))
}

/// Add wrapping/alignment to one complete bounded visible cell. Measurement and
/// drawing consume this same result so their line geometry cannot diverge.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_wrapped_table_cell_text(
    source: &NSAttributedString,
    alignment: MarkdownTableAlignment,
) -> Retained<NSAttributedString> {
    let paragraph = NSMutableParagraphStyle::new();
    // Actual visible cell content always wraps; the concealed Source
    // placeholder's clipping policy must not leak into user-visible text.
    paragraph.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping);
    paragraph.setAlignment(match alignment {
        // Leading alignment uses the complete content rectangle and equal padding.
        MarkdownTableAlignment::Left => NSTextAlignment::Left,
        // Center each visual line; translating a tight measured box is not
        // equivalent to wrapping and centering within the actual full width.
        MarkdownTableAlignment::Center => NSTextAlignment::Center,
        // Trailing alignment uses that same full width to decide line breaks.
        MarkdownTableAlignment::Right => NSTextAlignment::Right,
    });
    let result = NSMutableAttributedString::initWithAttributedString(
        NSMutableAttributedString::alloc(),
        source,
    );
    // SAFETY: AppKit exports an immutable process-lifetime key; the dictionary
    // retains this locally created, correctly typed paragraph style.
    let attributes = unsafe {
        MarkdownTextAttributes::from_slices(&[NSParagraphStyleAttributeName], &[&*paragraph])
    };
    // SAFETY: The range covers exactly the new result's UTF-16 length, without
    // borrowing original Source or crossing the Native string's bounds.
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
        // The complete overlay paints visible cells. The concealed original row
        // reserves one slot measured by that same layout batch.
        let height = table_row_height.unwrap_or(NATIVE_MARKDOWN_TABLE_ROW_HEIGHT);
        paragraph.setMinimumLineHeight(height);
        paragraph.setMaximumLineHeight(height);
        // Clip only the invisible Source placeholder so a giant raw row cannot
        // create extra soft-wrap slots; actual visible cells wrap independently.
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
        // Standard fragments reserve the row slot and finite prepared geometry
        // paints the thin rule; textBlocks would change TextKit's layout mode.
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
        // Actual indentation/spacing participates in wrapping and selection;
        // background painting consumes prepared rectangles without doing layout.
        apply_preview_block_insets(&paragraph, 6.0);
        return Some(paragraph);
    }

    if styles.contains(MarkdownPresentationStyles::BLOCK_QUOTE) {
        // Quotes receive their block border/background without storing another attributed document.
        paragraph.setLineHeightMultiple(1.2);
        paragraph.setParagraphSpacing(2.0);
        // Keep the existing 2pt paragraph spacing; the 7pt inset includes the
        // quote border and padding under the same geometry policy.
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
        // Both modes preserve link recognition; concealed destination syntax
        // must not leave an underline after the visible label ends.
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
        // Source preserves literal <u> semantics. Plain text and concealed
        // delimiters cannot accidentally acquire link/underline presentation.
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
