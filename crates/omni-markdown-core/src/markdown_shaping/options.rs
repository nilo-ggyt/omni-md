//! 字体run的固定选项；长度相关输入只有分页buffer，样式和语言另有明确小预算。
use harfrust::{Direction, Feature, Language, Script, Tag};

use super::{MarkdownShapingError as Error, Result};

pub const MARKDOWN_SHAPING_MAX_FEATURES: usize = 64;
const MAX_LANGUAGE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownShapingDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// feature范围以原全局UTF16 cluster寻址；半开区间两端不经上游饱和转换改写。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapingFeature {
    pub tag: [u8; 4],
    pub value: u32,
    pub source_utf16_start: u32,
    pub source_utf16_end: u32,
}

#[derive(Clone, Debug)]
pub struct MarkdownShapingOptions {
    pub direction: MarkdownShapingDirection,
    /// 已完成itemization的ISO15924脚本；本job不会把所有脚本猜为首个字符的脚本。
    pub script: [u8; 4],
    pub language: Option<String>,
    pub points: f32,
    /// 真实段边界由itemization调用方提供；step/piece边界不得设这两个标志。
    pub beginning_of_text: bool,
    pub end_of_text: bool,
    pub features: Vec<MarkdownShapingFeature>,
}

#[derive(Clone)]
pub(super) struct ValidatedOptions {
    pub direction: Direction,
    pub script: Script,
    pub language: Option<Language>,
    pub points: f32,
    pub scale: i32,
    pub output_divisor: i32,
    pub beginning_of_text: bool,
    pub end_of_text: bool,
    pub features: Vec<Feature>,
}

impl ValidatedOptions {
    pub(super) fn new(
        options: MarkdownShapingOptions,
        font: &crate::MarkdownLayoutFont,
    ) -> Result<Self> {
        if options.features.len() > MARKDOWN_SHAPING_MAX_FEATURES
            || !options.points.is_finite()
            || options.points <= 0.0
            || !options.script.iter().all(u8::is_ascii_alphabetic)
        {
            // 样式预算在分配转换Vec或任何shaper映射之前拒绝，不以少算feature换通过。
            return Err(Error::InvalidOptions);
        }
        let scale = f64::from(options.points) * 64.0;
        if scale.round() > f64::from(i32::MAX) || scale.round() < 1.0 {
            return Err(Error::InvalidOptions); // 26.6尺度必须能精确表示为一个有效正整数。
        }
        let (scale, output_divisor) = crate::markdown_layout_font::positioning::scale(
            font,
            options.points,
            scale.round() as i32,
        )
        .map_err(|_| Error::InvalidOptions)?;
        let script = Script::from_iso15924_tag(Tag::from_be_bytes(options.script))
            .ok_or(Error::InvalidOptions)?;
        let language = match &options.language {
            None => None,
            Some(value) => {
                if value.is_empty()
                    || value.len() > MAX_LANGUAGE_BYTES
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                {
                    // 不合法语言标签不变成默认语言；否则字形可能悄悄选择另一套规则。
                    return Err(Error::InvalidOptions);
                }
                Language::new(value.as_bytes())
            }
        };
        let mut features = Vec::with_capacity(options.features.len());
        for feature in options.features {
            if feature.source_utf16_start > feature.source_utf16_end
                || !feature.tag.iter().all(|byte| byte.is_ascii_graphic())
            {
                return Err(Error::InvalidOptions); // 语法或坐标非法不应用可用前缀。
            }
            features.push(Feature {
                tag: Tag::from_be_bytes(feature.tag),
                value: feature.value,
                start: feature.source_utf16_start,
                end: feature.source_utf16_end,
            });
        }
        let direction = match options.direction {
            MarkdownShapingDirection::LeftToRight => Direction::LeftToRight,
            MarkdownShapingDirection::RightToLeft => Direction::RightToLeft,
            MarkdownShapingDirection::TopToBottom => Direction::TopToBottom,
            MarkdownShapingDirection::BottomToTop => Direction::BottomToTop,
        };
        Ok(Self {
            direction,
            script,
            language,
            points: options.points,
            scale,
            output_divisor,
            beginning_of_text: options.beginning_of_text,
            end_of_text: options.end_of_text,
            features,
        })
    }

    /// 仅一次从内部尺度转为公开26.6；负offset同样按最近值、半值离零量化。
    pub(super) fn quantize(&self, value: i32) -> i32 {
        let value = i64::from(value);
        let divisor = i64::from(self.output_divisor);
        let magnitude = (value.abs() + divisor / 2) / divisor;
        (if value < 0 { -magnitude } else { magnitude }) as i32
    }
}
