//! 完整Uniform扫描封闭的控制事实；未完成Future不能把局部“未见控制”交给分项消费者。

use super::super::{BidiError, MarkdownBidiCancellation};

#[cfg(feature = "paged-shaping-frontier")]
enum InlineControls {
    Absent,
    Present,
}

pub(in crate::markdown_bidi) struct UniformCertificate {
    #[cfg(feature = "paged-shaping-frontier")]
    controls: InlineControls,
    #[cfg(feature = "paged-shaping-frontier")]
    paragraphs: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    interior_controls_absent: bool,
}

impl UniformCertificate {
    pub(in crate::markdown_bidi) fn has_paragraph_boundaries(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.paragraphs
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn from_properties(
        properties: crate::markdown_document_buffer::MarkdownUnicodeProperties,
    ) -> Self {
        Self {
            controls: if properties.separators() == 0 {
                InlineControls::Absent
            } else {
                // 摘要只决定完整原范围的精确存在性；含TAB/LS等仍可Uniform，但不能
                // 给ItemScan签无控制事实，避免把UBA类别当成Markdown控制策略。
                InlineControls::Present
            },
            paragraphs: properties.bidi_classes()
                & crate::markdown_document_buffer::MarkdownUnicodeProperties::class_mask(
                    unicode_bidi::BidiClass::B,
                )
                != 0,
            // 六种Markdown控制中只有TAB和LS不是B；其它控制只有真实B段端点可排除。
            // 这不声称FF/VT/SHY等不是GCB_Control，GCB与真实字体前沿照旧独立处理它们。
            interior_controls_absent: properties.separators() & ((1 << 0) | (1 << 4)) == 0,
        }
    }

    pub(super) fn clear() -> Self {
        Self {
            #[cfg(feature = "paged-shaping-frontier")]
            controls: InlineControls::Absent,
            #[cfg(feature = "paged-shaping-frontier")]
            paragraphs: false,
            #[cfg(feature = "paged-shaping-frontier")]
            interior_controls_absent: true,
        }
    }

    pub(super) fn observe(&mut self, scalar: char) {
        #[cfg(feature = "paged-shaping-frontier")]
        if matches!(
            scalar,
            '\t' | '\n' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}'
        ) {
            // 集合精确对应原Markdown inline_control，不由UBA的S/B类别推断；
            // U+2028等WS也有原生换行职责，而其它S/B并不自动取得产品控制语义。
            self.controls = InlineControls::Present;
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let _ = scalar; // 未启用分项快路时不新增控制事实或改变原UBA规则。
    }

    pub(in crate::markdown_bidi) fn seal(
        self,
        cancellation: MarkdownBidiCancellation,
    ) -> UniformResolution {
        UniformResolution {
            cancellation,
            #[cfg(feature = "paged-shaping-frontier")]
            controls: self.controls,
            #[cfg(feature = "paged-shaping-frontier")]
            paragraphs: self.paragraphs,
            #[cfg(feature = "paged-shaping-frontier")]
            interior_controls_absent: self.interior_controls_absent,
            #[cfg(feature = "paged-shaping-frontier")]
            paragraph: super::paragraph::PreparedParagraph::default(),
        }
    }
}

pub(in crate::markdown_bidi) struct UniformResolution {
    cancellation: MarkdownBidiCancellation,
    #[cfg(feature = "paged-shaping-frontier")]
    controls: InlineControls,
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_bidi) paragraphs: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_bidi) interior_controls_absent: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_bidi) paragraph: super::paragraph::PreparedParagraph,
}

impl UniformResolution {
    pub(in crate::markdown_bidi) fn check(&self) -> Result<(), BidiError> {
        self.cancellation.check()
    }

    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_bidi) fn control_free(&self) -> bool {
        !self.paragraphs && matches!(self.controls, InlineControls::Absent)
    }
}
