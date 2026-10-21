//! Fixed options for an itemized font run; source-length-dependent input stays in paged buffers while features/language have separate finite limits.
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

/// Address feature ranges by original global UTF-16 clusters, preserving half-open endpoints without upstream saturating conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapingFeature {
    pub tag: [u8; 4],
    pub value: u32,
    pub source_utf16_start: u32,
    pub source_utf16_end: u32,
}

/// Select OpenType shaping through actual font tables, without adding private platform punctuation/spacing policy beyond explicit features.
/// Full-width punctuation can differ from CoreText UI-font output; geometry must consume this job's actual sealed glyphs.
/// Post-hoc adjustments by font name, language, or another engine's width difference would separate drawing, caret, and hit-test authority.
#[derive(Clone, Debug)]
pub struct MarkdownShapingOptions {
    pub direction: MarkdownShapingDirection,
    /// ISO15924 script already established by itemization; this job does not guess every script from the first character.
    pub script: [u8; 4],
    pub language: Option<String>,
    pub points: f32,
    /// Itemization supplies actual text boundaries; step/piece edges must not set these flags.
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
            // Reject feature limits before allocating conversion Vecs or shaper mappings; do not omit features to fit.
            return Err(Error::InvalidOptions);
        }
        let scale = f64::from(options.points) * 64.0;
        if scale.round() > f64::from(i32::MAX) || scale.round() < 1.0 {
            return Err(Error::InvalidOptions); // Rounded 26.6 scale must fit a valid positive integer without saturation.
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
                    // An invalid language tag cannot become default language and silently select another shaping rule set.
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
                return Err(Error::InvalidOptions); // Invalid syntax/coordinates reject the request rather than applying a valid-looking prefix.
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

    /// Quantize once from internal scale to public 26.6, rounding negative offsets to nearest with halves away from zero too.
    pub(super) fn quantize(&self, value: i32) -> i32 {
        let value = i64::from(value);
        let divisor = i64::from(self.output_divisor);
        let magnitude = (value.abs() + divisor / 2) / divisor;
        (if value < 0 { -magnitude } else { magnitude }) as i32
    }
}
