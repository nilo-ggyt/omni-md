//! 列表预览只替换一个源unit的字形；piece tree、NSString和每个插入点始终保留原坐标。

use super::*;

/// 字形角色进入既有属性LRU的key；相同MARKER样式不能命中活动源码的旧字典。
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

/// 仅活动editor拥有这组固定资源；任一字形不可用时整套保持可编辑的原字面。
pub(super) struct MarkdownListMarkerGlyphPalette {
    pub(super) font: Retained<NSFont>,
    dash: Retained<NSGlyphInfo>,
    plus: Retained<NSGlyphInfo>,
    star: Retained<NSGlyphInfo>,
    unchecked: Retained<NSGlyphInfo>,
    checked: Retained<NSGlyphInfo>,
}

impl MarkdownListMarkerGlyphPalette {
    pub(super) fn new() -> Option<Self> {
        let font = NSFont::fontWithName_size(ns_string!("AppleSymbols"), 14.0)?;
        // 三个显示符号均须由同一真实字体映射成功；不把缺字方框当作可隐藏源码的许可。
        let mut symbols = [0x2022_u16, 0x2610, 0x2611];
        let mut glyphs = [0_u16; 3];
        let core_font: &CTFont = font.as_ref();
        // SAFETY: 两个栈数组均有三个元素；所有符号为单UTF-16 unit，输出在本调用内独占可写。
        let covered = unsafe {
            core_font.glyphs_for_characters(
                NonNull::new(symbols.as_mut_ptr())?,
                NonNull::new(glyphs.as_mut_ptr())?,
                3,
            )
        };
        if !covered || glyphs.contains(&0) {
            return None; // 字体失败不收起任务框尾部或原bullet，避免留下半个控件。
        }
        // baseString精确对应一个原字符；不得把[x]合成一个cluster而丢失内部caret。
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

    pub(super) fn info(&self, role: MarkdownListGlyphRole) -> Option<&NSGlyphInfo> {
        match role {
            MarkdownListGlyphRole::Dash => Some(&self.dash),
            MarkdownListGlyphRole::Plus => Some(&self.plus),
            MarkdownListGlyphRole::Star => Some(&self.star),
            MarkdownListGlyphRole::Unchecked => Some(&self.unchecked),
            MarkdownListGlyphRole::Checked => Some(&self.checked),
            // 隐藏尾部没有自造字形；活动与Source字面也不带NSGlyphInfo。
            MarkdownListGlyphRole::Literal | MarkdownListGlyphRole::Hidden => None,
        }
    }
}

/// 只消费Core真实Item/Task token；没有可用的整套字形时保持原字面且不隐藏任何部分。
pub(super) fn native_markdown_list_role(
    marker: Option<&MarkdownPreviewListMarker>,
    index: usize,
    range: &mut std::ops::Range<usize>,
    enabled: bool,
) -> MarkdownListGlyphRole {
    let Some(marker) = marker.filter(|marker| enabled && !marker.active) else {
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
                // 只有原'['这一个unit取得框字形；内部两个源码unit仍分别可放置光标。
                range.end = range.end.min(marker.utf16_range.start + 1);
                if checked {
                    MarkdownListGlyphRole::Checked
                } else {
                    MarkdownListGlyphRole::Unchecked
                }
            } else {
                // x/空格和']'沿现有0.1pt隐藏策略，不使用多unit的glyph cluster。
                range.start = range.start.max(marker.utf16_range.start + 1);
                MarkdownListGlyphRole::Hidden
            }
        }
    }
}
