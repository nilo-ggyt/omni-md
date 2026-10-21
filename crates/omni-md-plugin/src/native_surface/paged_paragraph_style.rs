//! Capture bounded tab facts from the original paragraph style without reading text or replacing Native
//! settings with spaces/guessed intervals.

use objc2::rc::Retained;
use objc2_app_kit::{NSParagraphStyle, NSTabColumnTerminatorsAttributeName, NSTextAlignment};
use objc2_foundation::{MainThreadMarker, NSCharacterSet, NSCopying};
use omni_markdown_core::{
    MarkdownLineParagraphStyle, MarkdownLineTabAlignment, MarkdownLineTabStop,
};

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownParagraphStyleError {
    #[error("原段落样式捕获已失权")]
    Revoked,
    #[error("原段落制表事实无效或超出固定预算")]
    InvalidMetadata,
    #[error("原自然对齐尚未绑定真实段落方向")]
    DirectionRequired,
}
type Result<T> = std::result::Result<T, NativeMarkdownParagraphStyleError>;

/// Native terminator sets support actual later lookahead; Core cannot treat a period as the decimal
/// boundary in every language.
pub struct NativeMarkdownLineParagraph {
    style: MarkdownLineParagraphStyle,
    terminators: Vec<Option<Retained<NSCharacterSet>>>,
    _main_thread: MainThreadMarker,
}
impl NativeMarkdownLineParagraph {
    #[must_use]
    /// Borrow frozen Core tab facts without reentering AppKit.
    pub fn core_style(&self) -> &MarkdownLineParagraphStyle {
        &self.style
    }

    #[must_use]
    /// Borrow the original stop's copied Native terminator set for bounded lookahead.
    pub fn terminators_for(&self, stop_ordinal: usize) -> Option<&NSCharacterSet> {
        self.terminators
            .get(stop_ordinal)
            .and_then(Option::as_deref)
    }
}

/// Guard one potentially reentrant Native operation with original caller authority.
fn step<T>(current: &dyn Fn() -> bool, operation: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownParagraphStyleError::Revoked);
    }
    let value = operation();
    if !current() {
        drop(value); // Even a +1 object returned by the getter belongs to this failed stack and cannot transfer to another request.
        return Err(NativeMarkdownParagraphStyleError::Revoked);
    }
    Ok(value)
}

/// Convert a finite nonnegative point value once into the shared 26.6 coordinate domain.
fn fixed(value: f64) -> Result<i64> {
    if !value.is_finite() || !(0.0..(1_u64 << 46) as f64).contains(&value) {
        return Err(NativeMarkdownParagraphStyleError::InvalidMetadata);
    }
    Ok((value * 64.0).round() as i64) // Quantize once under the Core 26.6 contract; later checks still enforce order and positive intervals.
}

/// Freeze the original NSParagraphStyle once and read at most 64 stops, checking original caller
/// authority around all Native operations.
/// `paragraph_rtl` must come from the same Source's actual paragraph direction; None cannot guess
/// Natural alignment.
///
/// # Errors
/// Reject revocation, excess budget, unknown alignment, invalid coordinates/types, or nonincreasing
/// quantized stops without truncation or sorting repairs.
pub fn capture_paged_paragraph_style(
    paragraph: &NSParagraphStyle,
    paragraph_rtl: Option<bool>,
    mtm: MainThreadMarker,
    current: &dyn Fn() -> bool,
) -> Result<NativeMarkdownLineParagraph> {
    let snapshot = step(current, || paragraph.copy())?;
    let native_stops = step(current, || snapshot.tabStops())?;
    let count = step(current, || native_stops.len())?;
    if count > 64 {
        return Err(NativeMarkdownParagraphStyleError::InvalidMetadata);
    }
    let mut tab_stops = Vec::with_capacity(count);
    let mut terminators = Vec::with_capacity(count);
    for ordinal in 0..count {
        let stop = step(current, || native_stops.objectAtIndex(ordinal))?;
        let location = step(current, || stop.location())?;
        let location_26_6 = fixed(location)?;
        if tab_stops
            .last()
            .is_some_and(|prior: &MarkdownLineTabStop| prior.location_26_6 >= location_26_6)
        {
            return Err(NativeMarkdownParagraphStyleError::InvalidMetadata);
        } // Original order/uniqueness are layout facts; a Native error or quantization collision cannot silently produce another tab table.
        let alignment = step(current, || stop.alignment())?;
        let options = step(current, || stop.options())?;
        let key = step(current, || {
            // SAFETY: The exported immutable AppKit dictionary key is process-long and borrows neither
            // caller nor text storage.
            unsafe { NSTabColumnTerminatorsAttributeName }
        })?;
        let terminator = step(current, || options.objectForKey(key))?;
        let terminator = match terminator {
            Some(value) => {
                let set = step(current, || value.downcast::<NSCharacterSet>())?
                    .map_err(|_| NativeMarkdownParagraphStyleError::InvalidMetadata)?;
                Some(step(current, || set.copy())?)
            }
            None => None,
        };
        let alignment = match alignment {
            NSTextAlignment::Left | NSTextAlignment::Justified => MarkdownLineTabAlignment::Left,
            NSTextAlignment::Right if terminator.is_some() => MarkdownLineTabAlignment::Decimal,
            NSTextAlignment::Right => MarkdownLineTabAlignment::Right,
            NSTextAlignment::Center => MarkdownLineTabAlignment::Center,
            NSTextAlignment::Natural => match paragraph_rtl {
                Some(true) => MarkdownLineTabAlignment::Right,
                Some(false) => MarkdownLineTabAlignment::Left,
                None => return Err(NativeMarkdownParagraphStyleError::DirectionRequired),
            },
            _ => return Err(NativeMarkdownParagraphStyleError::InvalidMetadata),
        };
        tab_stops.push(MarkdownLineTabStop {
            location_26_6,
            alignment,
        });
        terminators.push(terminator);
        step(current, || drop(options))?;
        step(current, || drop(stop))?;
    }
    let interval = step(current, || snapshot.defaultTabInterval())?;
    let default_tab_interval_26_6 = if interval == 0.0 {
        Some(0) // An actual Native zero means no further interval and no Tab advance; None is reserved for requests lacking actual style facts.
    } else {
        let interval = fixed(interval)?;
        if interval == 0 {
            return Err(NativeMarkdownParagraphStyleError::InvalidMetadata);
        }
        Some(interval)
    };
    step(current, || drop(native_stops))?;
    step(current, || drop(snapshot))?;
    Ok(NativeMarkdownLineParagraph {
        style: MarkdownLineParagraphStyle {
            tab_stops,
            default_tab_interval_26_6,
        },
        terminators,
        _main_thread: mtm,
    })
}
