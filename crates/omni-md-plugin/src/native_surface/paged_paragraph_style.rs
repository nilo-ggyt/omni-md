//! 从原段落样式捕获有限制表事实；不读取正文，不以固定空格或猜测间隔代替Native设置。

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

/// Native终止符集合仅供后继真实lookahead查询；Core不能把'.'当作所有语言的decimal边界。
pub struct NativeMarkdownLineParagraph {
    style: MarkdownLineParagraphStyle,
    terminators: Vec<Option<Retained<NSCharacterSet>>>,
    _main_thread: MainThreadMarker,
}
impl NativeMarkdownLineParagraph {
    #[must_use]
    pub fn core_style(&self) -> &MarkdownLineParagraphStyle {
        &self.style
    }

    #[must_use]
    pub fn terminators_for(&self, stop_ordinal: usize) -> Option<&NSCharacterSet> {
        self.terminators
            .get(stop_ordinal)
            .and_then(Option::as_deref)
    }
}

fn step<T>(current: &dyn Fn() -> bool, operation: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownParagraphStyleError::Revoked);
    }
    let value = operation();
    if !current() {
        drop(value); // 即使原getter交出了+1对象，也必须在失败栈归还，不能转交新请求。
        return Err(NativeMarkdownParagraphStyleError::Revoked);
    }
    Ok(value)
}

fn fixed(value: f64) -> Result<i64> {
    if !value.is_finite() || !(0.0..(1_u64 << 46) as f64).contains(&value) {
        return Err(NativeMarkdownParagraphStyleError::InvalidMetadata);
    }
    Ok((value * 64.0).round() as i64) // 同Core公开26.6合同量化一次；后继仍核顺序和正间隔。
}

/// 一次冻结原NSParagraphStyle后只读取≤64个stop；所有Native调用前后均核原调用票。
/// `paragraph_rtl`只能来自同一Source真实段落方向，None不能猜Natural的实际对齐。
///
/// # Errors
/// 失权、超预算、未知对齐、无效坐标/类型或量化后stop不递增均拒绝，不截断或排序补救。
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
        } // 原顺序/唯一性是布局事实；不能把Native错误或量化碰撞静默改成新停靠表。
        let alignment = step(current, || stop.alignment())?;
        let options = step(current, || stop.options())?;
        let key = step(current, || {
            // SAFETY: AppKit导出的不可变字典key进程内有效，未借用调用方或正文存储。
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
        Some(0) // Native已知0表示没有后续间隔、Tab不推进；None留给尚未取得实际样式的请求。
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
