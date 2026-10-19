//! 已布局行的元数据认证；避免按文字位置反查再次复制完整物理段落。

use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSTextContentManager, NSTextElementProvider, NSTextLayoutFragment, NSTextLayoutFragmentState,
    NSTextLineFragment, NSTextLocation,
};
use objc2_foundation::{NSRange, NSRect};
use std::ops::Range;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NativeMarkdownLineMetadataError {
    #[error("原行的布局权限已经撤销")]
    Revoked,
    #[error("原行与元素的来源范围不一致")]
    Source,
    #[error("原行的实际几何或所属片段已经改变")]
    Geometry,
}

/// 全部借用只覆盖当前同步调用；外部原票仍负责文档、模式、选择与manager的身份。
pub struct NativeMarkdownLineMetadataRequest<'a> {
    pub fragment: &'a NSTextLayoutFragment,
    pub line: &'a NSTextLineFragment,
    pub content: &'a NSTextContentManager,
    pub document_start: &'a ProtocolObject<dyn NSTextLocation>,
    pub document_length: usize,
    pub element_source: &'a Range<usize>,
    pub source: &'a Range<usize>,
    pub local_range: NSRange,
    pub bounds: NSRect,
}

/// 每个可重入getter前后都核同一票；getter返回的临时对象由调用栈独占，不进入缓存。
fn step<T>(
    current: &impl Fn() -> bool,
    operation: impl FnOnce() -> T,
) -> Result<T, NativeMarkdownLineMetadataError> {
    if !current() {
        return Err(NativeMarkdownLineMetadataError::Revoked);
    }
    let value = operation();
    if !current() {
        return Err(NativeMarkdownLineMetadataError::Revoked);
    }
    Ok(value)
}

/// 通过原行的真实属性值坐标、竖向精确命中与provider两端往返认证来源，不读取正文。
///
/// 调用方须先认证原manager/element/fragment源范围，并在本函数返回及局部Native claim
/// 析构后继续核原图与几何。空EOF视觉行使用其独立document.end合同，不进入普通字符行。
///
/// # Errors
/// 原票、范围或几何失效均拒绝；不主动ensure布局，不改用后继段落或重新采集原票。
pub fn verify_native_markdown_line_metadata(
    request: NativeMarkdownLineMetadataRequest<'_>,
    current: &impl Fn() -> bool,
) -> Result<(), NativeMarkdownLineMetadataError> {
    use NativeMarkdownLineMetadataError as Error;
    let result = (|| {
        let NativeMarkdownLineMetadataRequest {
            fragment,
            line,
            content,
            document_start,
            document_length,
            element_source,
            source,
            local_range,
            bounds,
        } = request;
        if step(current, || fragment.state())? != NSTextLayoutFragmentState::LayoutAvailable {
            return Err(Error::Geometry); // estimated或无效片段不得由查询补排后冒充原布局。
        }
        let element_length = element_source
            .end
            .checked_sub(element_source.start)
            .ok_or(Error::Source)?;
        let local_end = local_range
            .location
            .checked_add(local_range.length)
            .ok_or(Error::Source)?;
        if source.is_empty()
            || element_source.end > document_length
            || local_end > element_length
            || element_source.start.checked_add(local_range.location) != Some(source.start)
            || element_source.start.checked_add(local_end) != Some(source.end)
        {
            return Err(Error::Source); // 两端必须采用同一element原点；不按fragment起点猜偏移。
        }
        let attributed = step(current, || line.attributedString())?;
        if step(current, || attributed.length())? != element_length {
            return Err(Error::Source); // characterRange属于这份真实属性值，不能把局部slice误认为整个element。
        }
        if ![
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            bounds.size.height,
        ]
        .iter()
        .all(|value| value.is_finite())
            || bounds.size.height <= 0.0
        {
            return Err(Error::Geometry); // 不用非有限中点或零高占位行生成一次成功命中。
        }
        let y = bounds.origin.y + bounds.size.height * 0.5;
        if !y.is_finite() {
            return Err(Error::Geometry);
        }
        let exact = step(current, || {
            fragment.textLineFragmentForVerticalOffset_requiresExactMatch(y, true)
        })?
        .ok_or(Error::Geometry)?;
        if !std::ptr::eq::<NSTextLineFragment>(&*exact, line)
            || step(current, || exact.characterRange())? != local_range
            || step(current, || exact.typographicBounds())? != bounds
        {
            return Err(Error::Geometry); // 同一原生行身份与实际值同时匹配；无需遍历巨段落的整个line数组。
        }
        for endpoint in [source.start, source.end] {
            let offset = isize::try_from(endpoint).map_err(|_| Error::Source)?;
            let location = step(current, || {
                content.locationFromLocation_withOffset(document_start, offset)
            })?
            .ok_or(Error::Source)?;
            if step(current, || {
                content.offsetFromLocation_toLocation(document_start, &location)
            })? != offset
            {
                return Err(Error::Source); // 同一provider的真实全局端点必须精确往返，绝不clamp或容差。
            }
        }
        if step(current, || line.characterRange())? != local_range
            || step(current, || line.typographicBounds())? != bounds
            || step(current, || fragment.state())? != NSTextLayoutFragmentState::LayoutAvailable
        {
            return Err(Error::Geometry); // 后一个getter可重入，发布前再核最初的行和可用布局。
        }
        Ok(())
    })(); // 属性值、精确行及每个位置claim在这里先归还，随后核最后一次析构是否撤权。
    if !current() {
        return Err(Error::Revoked);
    }
    result
}
