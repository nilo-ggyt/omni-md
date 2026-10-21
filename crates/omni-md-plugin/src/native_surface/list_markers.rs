//! Reference list preview replaces only one source unit's glyph; the piece tree, NSString, and every caret keep their original coordinates.

use super::*;

/// Includes glyph role in the existing attribute-LRU key, so identical MARKER bits cannot reuse a dictionary for active literal source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum MarkdownListGlyphRole {
    #[default]
    Literal,
    Dash,
    Plus,
    Star,
    Unchecked,
    Checked,
    Hidden,
}

/// The active reference editor alone owns this fixed palette; missing any glyph preserves the complete editable literal marker.
pub(super) struct MarkdownListMarkerGlyphPalette {
    pub(super) font: Retained<NSFont>,
    dash: Retained<NSGlyphInfo>,
    plus: Retained<NSGlyphInfo>,
    star: Retained<NSGlyphInfo>,
    unchecked: Retained<NSGlyphInfo>,
    checked: Retained<NSGlyphInfo>,
}

impl MarkdownListMarkerGlyphPalette {
    /// Acquires one fixed reference-preview palette. Any missing symbol drops the partial palette;
    /// callers then preserve literal markers rather than selecting another font or hiding a suffix.
    pub(super) fn new() -> Option<Self> {
        let font = NSFont::fontWithName_size(ns_string!("AppleSymbols"), 14.0)?;
        // All three symbols require coverage from the same actual font; a missing-glyph box grants no authority to hide source syntax.
        let mut symbols = [0x2022_u16, 0x2610, 0x2611];
        let mut glyphs = [0_u16; 3];
        let core_font: &CTFont = font.as_ref();
        // SAFETY: Both stack arrays have three entries; every symbol is one UTF-16 unit, and output is exclusively writable during this call.
        let covered = unsafe {
            core_font.glyphs_for_characters(
                NonNull::new(symbols.as_mut_ptr())?,
                NonNull::new(glyphs.as_mut_ptr())?,
                3,
            )
        };
        if !covered || glyphs.contains(&0) {
            return None; // Font failure hides neither the task-box suffix nor original bullet, preventing partial marker presentation.
        }
        // Each baseString names exactly one original character; combining [x] into one cluster would lose its interior carets.
        let dash = NSGlyphInfo::glyphInfoWithCGGlyph_forFont_baseString(
            glyphs[0],
            &font,
            ns_string!("-"),
        )?;
        let plus = NSGlyphInfo::glyphInfoWithCGGlyph_forFont_baseString(
            glyphs[0],
            &font,
            ns_string!("+"),
        )?;
        let star = NSGlyphInfo::glyphInfoWithCGGlyph_forFont_baseString(
            glyphs[0],
            &font,
            ns_string!("*"),
        )?;
        let unchecked = NSGlyphInfo::glyphInfoWithCGGlyph_forFont_baseString(
            glyphs[1],
            &font,
            ns_string!("["),
        )?;
        let checked = NSGlyphInfo::glyphInfoWithCGGlyph_forFont_baseString(
            glyphs[2],
            &font,
            ns_string!("["),
        )?;
        Some(Self {
            font,
            dash,
            plus,
            star,
            unchecked,
            checked,
        })
    }

    /// Borrows only the glyph assigned to this actual role, with no allocation or new text authority.
    pub(super) fn info(&self, role: MarkdownListGlyphRole) -> Option<&NSGlyphInfo> {
        match role {
            MarkdownListGlyphRole::Dash => Some(&self.dash),
            MarkdownListGlyphRole::Plus => Some(&self.plus),
            MarkdownListGlyphRole::Star => Some(&self.star),
            MarkdownListGlyphRole::Unchecked => Some(&self.unchecked),
            MarkdownListGlyphRole::Checked => Some(&self.checked),
            // Hidden suffixes invent no glyph; active and Source literals likewise carry no NSGlyphInfo.
            MarkdownListGlyphRole::Literal | MarkdownListGlyphRole::Hidden => None,
        }
    }
}

/// Consumes actual Core Item/Task tokens only; an unavailable complete palette preserves every literal source unit.
pub(super) fn native_markdown_list_role(
    marker: Option<&MarkdownPreviewListMarker>,
    index: usize,
    range: &mut std::ops::Range<usize>,
    enabled: bool,
) -> MarkdownListGlyphRole {
    let Some(marker) = marker.filter(|marker| enabled && !marker.active) else {
        // Active editing or an unavailable palette keeps every original character visible and editable.
        return MarkdownListGlyphRole::Literal;
    };
    match marker.kind {
        MarkdownPreviewListMarkerKind::Bullet(source) => match source {
            MarkdownBulletMarker::Dash => MarkdownListGlyphRole::Dash,
            MarkdownBulletMarker::Plus => MarkdownListGlyphRole::Plus,
            MarkdownBulletMarker::Star => MarkdownListGlyphRole::Star,
        },
        MarkdownPreviewListMarkerKind::TaskBullet { .. } => MarkdownListGlyphRole::Hidden,
        MarkdownPreviewListMarkerKind::TaskBox { checked } => {
            if index == marker.utf16_range.start {
                // Only the original opening bracket receives the box glyph; the remaining two source units keep separate caret positions.
                range.end = range.end.min(marker.utf16_range.start + 1);
                if checked {
                    MarkdownListGlyphRole::Checked
                } else {
                    MarkdownListGlyphRole::Unchecked
                }
            } else {
                // The x/space and closing bracket use the existing 0.1-point hidden style rather than a multi-unit glyph cluster.
                range.start = range.start.max(marker.utf16_range.start + 1);
                MarkdownListGlyphRole::Hidden
            }
        }
    }
}
