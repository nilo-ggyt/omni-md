//! 分页预览的共同排版值；宽度、行高和绘图消费同一合同，不持正文、字体或布局图。

use super::paged_font_geometry::NativeMarkdownPagedFontMetricValues;
use super::paged_line_fonts::NativeMarkdownPreviewFontMetrics;
use omni_markdown_core::MarkdownPresentationStyles as Styles;
use omni_markdown_core::{
    MarkdownComposedLineFacts, MarkdownComposedLinePartFacts, MarkdownComposedLinePartKind,
    MarkdownPresentationMode,
};

/// Producer 的分步读与 Viewport 的有限准备共用同一个字体/段落累计器，不另建行高目录。
pub(in crate::native_surface) struct PreviewLineMetrics {
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
    pub paragraph: PreviewParagraph,
    mode: MarkdownPresentationMode,
    system_above: f64,
    system_below: f64,
    control_above: f64,
    control_below: f64,
    has_text: bool,
}

impl PreviewLineMetrics {
    /// 模式来自原Body/Scene几何身份；普通正文空样式不具有选择另一种行槽的权限。
    pub fn new(mode: MarkdownPresentationMode) -> Self {
        Self {
            ascent: 0.0,
            descent: 0.0,
            leading: f64::NEG_INFINITY,
            paragraph: PreviewParagraph::LITERAL,
            mode,
            system_above: 0.0,
            system_below: 0.0,
            control_above: 0.0,
            control_below: 0.0,
            has_text: false,
        }
    }

    /// 只有封存的 glyph/tab 可决定段落角色；默认换行控制不借邻接文字补造样式。
    pub fn add(
        &mut self,
        part: &MarkdownComposedLinePartFacts,
        metrics: NativeMarkdownPagedFontMetricValues,
        preview: Option<NativeMarkdownPreviewFontMetrics>,
    ) -> Option<()> {
        self.ascent = self.ascent.max(metrics.ascent);
        self.descent = self.descent.max(metrics.descent);
        self.leading = self.leading.max(metrics.leading);
        if self.mode == MarkdownPresentationMode::InlinePreview {
            let preview =
                preview.expect("Preview实际Font持系统行槽；不能用Source浮点或猜测取整替换");
            let offset =
                super::text_attributes::native_markdown_baseline_offset(part.presentation, false)
                    .unwrap_or(0.0);
            // 实测TextKit上移内容抬高行基线，下移内容保字体原基线并扩下沿；EOL仍不撑高非空行。
            let above = preview.baseline + offset.max(0.0);
            let below = (preview.line_height - preview.baseline - offset).max(0.0);
            if matches!(
                part.kind,
                MarkdownComposedLinePartKind::Glyphs | MarkdownComposedLinePartKind::Tab
            ) {
                self.has_text = true;
                self.system_above = self.system_above.max(above);
                self.system_below = self.system_below.max(below);
            } else {
                self.control_above = self.control_above.max(above);
                self.control_below = self.control_below.max(below);
            } // 真实非空行不把EOL字体混进可见内容槽；只有空行/EOF才由自身控制Font确定高度。
        } // 混合字体分别保真实baseline上/下界，同一结果仍由Producer和Viewport共同消费。
        if part.kind != MarkdownComposedLinePartKind::FoldedSection {
            let paragraph = PreviewParagraph::from_styles(part.presentation)
                .unwrap_or(PreviewParagraph::LITERAL);
            if paragraph.inset_26_6() != part.paragraph_inset_26_6 {
                return None; // 软换行所用的封存宽度必须与最终行框一致，不能靠绘图偏移修正旧输出。
            }
            if paragraph != PreviewParagraph::LITERAL {
                if self.paragraph != PreviewParagraph::LITERAL && self.paragraph != paragraph {
                    return None; // 同一源段不能拼进互相冲突的块行高；保留原失败而非任取第一种样式。
                }
                self.paragraph = paragraph;
            }
        }
        Some(())
    }

    /// 首末软行只比较同一封存源段落端点；视口边缘没有首末段距权限。
    pub fn geometry(&self, line: &MarkdownComposedLineFacts) -> Option<PreviewLineGeometry> {
        if self.mode == MarkdownPresentationMode::InlinePreview {
            let (above, below) = if self.has_text {
                (self.system_above, self.system_below)
            } else {
                (self.control_above, self.control_below)
            };
            return self.paragraph.measure_preview(
                above,
                below,
                line.source_utf16.start == line.paragraph_utf16.start
                    && line.paragraph_utf16.start != 0,
                line.source_utf16.end == line.paragraph_utf16.end,
            ); // 首段没有前一段，原paragraphSpacingBefore不在正文起点另造8pt空白。
        }
        self.paragraph.measure(
            self.ascent,
            self.descent,
            self.leading,
            line.source_utf16.start == line.paragraph_utf16.start,
            line.source_utf16.end == line.paragraph_utf16.end,
        )
    }
}

/// 段落背景只是已排好行框的视觉角色，不承担行高、缩进或源坐标转换。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) enum PreviewParagraphBackground {
    Code,
    Quote,
    Rule,
}

/// 只含原 AppKit 段落合同的有限标量；表格行高仍由表格自己的真实排版负责。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_surface) struct PreviewParagraph {
    pub before: f64,
    pub after: f64,
    pub minimum_line_height: f64,
    pub maximum_line_height: f64,
    pub line_height_multiple: f64,
    pub inset: f64,
    pub background: Option<PreviewParagraphBackground>,
}

/// 同一份实际字体度量只计算一次纵向分配；行框、baseline 与滚动高度不得分别猜值。
#[derive(Clone, Copy, Debug)]
pub(in crate::native_surface) struct PreviewLineGeometry {
    pub advance: f64,
    pub baseline_from_top: f64,
    pub text_height: f64,
    pub before: f64,
    pub after: f64,
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// 原有限窗口真实行的纯值观察；没有 Source、Reader、Font 或 Native 对象强边。
pub(in crate::native_surface) struct PreviewLineObservation {
    pub source: std::ops::Range<usize>,
    pub paragraph: std::ops::Range<usize>,
    pub frame: objc2_foundation::NSRect,
    pub baseline: objc2_foundation::NSPoint,
    pub text_height: f64,
    pub before: f64,
    pub after: f64,
}

impl PreviewParagraph {
    pub const LITERAL: Self = Self {
        before: 0.0,
        after: 0.0,
        minimum_line_height: 0.0,
        maximum_line_height: 0.0,
        line_height_multiple: 0.0,
        inset: 0.0,
        background: None,
    };

    /// 只解释原语义发布已经给出的样式；此函数不授予隐藏权，也不把未知容器变成普通段落。
    pub fn from_styles(styles: Styles) -> Option<Self> {
        let hidden = styles.contains(Styles::HIDDEN_MARKER);
        if hidden && styles.contains(Styles::FENCED_CODE) {
            return None; // 收起的围栏沿原合同没有代码块的 padding、背景或最小行高。
        }
        if styles.contains(Styles::TABLE_ROW_PLACEHOLDER)
            || (hidden && styles.contains(Styles::TABLE_DELIMITER))
        {
            return None; // 表格自己的行槽与分隔带不能再叠加正文段落几何。
        }

        let mut paragraph = Self::LITERAL;
        let heading = [
            Styles::HEADING_1,
            Styles::HEADING_2,
            Styles::HEADING_3,
            Styles::HEADING_4,
            Styles::HEADING_5,
            Styles::HEADING_6,
        ]
        .into_iter()
        .any(|role| styles.contains(role));
        if heading {
            paragraph.before = 8.0;
            paragraph.after = 6.0;
            paragraph.line_height_multiple = 1.08;
        } // 标题内的粗体、链接等只改变字形；段落间距仍由同一标题拥有。

        if hidden && styles.contains(Styles::HORIZONTAL_RULE) {
            paragraph.minimum_line_height = 12.0;
            paragraph.maximum_line_height = 12.0;
            paragraph.before = 6.0;
            paragraph.after = 6.0;
            paragraph.background = Some(PreviewParagraphBackground::Rule);
            return Some(paragraph); // 活动 rule 保持字面编辑；只有原隐藏语义才能领取细线槽。
        }
        if styles.contains(Styles::FENCED_CODE) {
            paragraph.minimum_line_height = 22.0;
            paragraph.after = 0.0;
            paragraph.inset = 6.0;
            paragraph.before += paragraph.inset;
            paragraph.after += paragraph.inset;
            paragraph.background = Some(PreviewParagraphBackground::Code);
            return Some(paragraph); // 左右缩进先进入可用宽度，四边空隙再随真实段首末行分配。
        }
        if styles.contains(Styles::BLOCK_QUOTE) {
            paragraph.line_height_multiple = 1.2;
            paragraph.after = 2.0;
            paragraph.inset = 7.0;
            paragraph.before += paragraph.inset;
            paragraph.after += paragraph.inset;
            paragraph.background = Some(PreviewParagraphBackground::Quote);
            return Some(paragraph); // 保留原 6 点 padding 与 1 点边框；容器权限仍归原解析来源。
        }
        heading.then_some(paragraph)
    }

    /// 原有段落值都是精确的整数点；封存到同 Item 的 26.6 字段后仍无需浮点推断可用宽度。
    pub fn inset_26_6(self) -> i32 {
        (self.inset * 64.0) as i32
    }

    /// 系统自然行槽先乘原paragraph倍数，再受min/max约束；额外高度按真实TextKit基线合同置于文字之前。
    fn measure_preview(
        self,
        above: f64,
        below: f64,
        first: bool,
        last: bool,
    ) -> Option<PreviewLineGeometry> {
        let natural = above + below;
        let mut text_height = if self.line_height_multiple == 0.0 {
            natural
        } else {
            natural * self.line_height_multiple
        };
        if self.minimum_line_height != 0.0 {
            text_height = text_height.max(self.minimum_line_height);
        }
        if self.maximum_line_height != 0.0 {
            text_height = text_height.min(self.maximum_line_height);
        }
        let before = if first { self.before } else { 0.0 };
        let after = if last { self.after } else { 0.0 };
        let baseline_from_top = before + above + text_height - natural;
        let advance = before + text_height + after;
        if !natural.is_finite()
            || natural <= 0.0
            || !advance.is_finite()
            || advance <= 0.0
            || !baseline_from_top.is_finite()
            || !text_height.is_finite()
            || text_height <= 0.0
        {
            return None; // 真正无效的系统Font/段落结果不能借min-height或夹值制造成功。
        }
        Some(PreviewLineGeometry {
            advance,
            baseline_from_top,
            text_height,
            before,
            after,
        })
    }

    /// 字体来自同封存 part 的实际注册；只有源段落的真正首末软行领取外段距。
    pub fn measure(
        self,
        ascent: f64,
        descent: f64,
        leading: f64,
        first: bool,
        last: bool,
    ) -> Option<PreviewLineGeometry> {
        let natural = ascent + descent + leading;
        if !natural.is_finite() || natural <= 0.0 {
            return None; // 无效的实际字体不能借段落最小高度变成貌似成功的布局。
        }
        let mut text_height = if self.line_height_multiple == 0.0 {
            natural
        } else {
            natural * self.line_height_multiple
        };
        if self.minimum_line_height != 0.0 {
            text_height = text_height.max(self.minimum_line_height);
        }
        if self.maximum_line_height != 0.0 {
            text_height = text_height.min(self.maximum_line_height);
        }
        let before = if first { self.before } else { 0.0 };
        let after = if last { self.after } else { 0.0 };
        // 字面模式保持原来的加法及 baseline 次序；新增几何不改变既有 Source 的浮点结果。
        let extra = if text_height == natural {
            leading
        } else {
            text_height - ascent - descent
        };
        let baseline_from_top = before + ascent + extra / 2.0;
        let advance = before + text_height + after;
        if !advance.is_finite()
            || advance <= 0.0
            || !baseline_from_top.is_finite()
            || !text_height.is_finite()
            || text_height <= 0.0
        {
            return None; // 真正的算术或几何失败返回原调用者，不截断高度或画出一部分候选。
        }
        Some(PreviewLineGeometry {
            advance,
            baseline_from_top,
            text_height,
            before,
            after,
        })
    }
}
