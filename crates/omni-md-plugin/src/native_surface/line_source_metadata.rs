//! Authenticate already laid-out line metadata without looking up a text
//! position through another complete physical-paragraph copy.

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

/// Borrow only for this synchronous observation. The caller's original ticket
/// still owns document, mode, selection, and manager identity.
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

/// Check the same authority before and after every reentrant getter. Returned
/// Native temporaries stay on this stack and never enter a cache.
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

/// Authenticate the original line through its actual attributed-value extent,
/// exact vertical hit, and provider endpoint round trips, without reading text.
///
/// The caller must first authenticate manager/element/fragment source ranges,
/// then recheck the original graph and geometry after this call's Native claims
/// end. Empty visual EOF uses its separate document.end contract.
///
/// # Errors
/// Reject revoked authority or changed ranges/geometry. Never force layout,
/// substitute a successor paragraph, or recapture permission for stale output.
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
            return Err(Error::Geometry); // Estimated/invalid fragments cannot acquire original layout authority by forcing a query.
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
            return Err(Error::Source); // Both endpoints use the same actual element origin, without inferring offsets from a fragment start.
        }
        let attributed = step(current, || line.attributedString())?;
        if step(current, || attributed.length())? != element_length {
            return Err(Error::Source); // characterRange belongs to this actual value; a local slice cannot stand in for the entire element.
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
            return Err(Error::Geometry); // Nonfinite coordinates or zero-height placeholders cannot certify an exact hit.
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
            return Err(Error::Geometry); // Require actual line identity and values, without traversing a giant paragraph's complete line array.
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
                return Err(Error::Source); // The same provider's global endpoint must round-trip exactly, without clamping or tolerance.
            }
        }
        if step(current, || line.characterRange())? != local_range
            || step(current, || line.typographicBounds())? != bounds
            || step(current, || fragment.state())? != NSTextLayoutFragmentState::LayoutAvailable
        {
            return Err(Error::Geometry); // Later getters may reenter; recheck original line values and available layout before completion.
        }
        Ok(())
    })(); // Return attributed-value/line/location claims before checking whether final destruction revoked authority.
    if !current() {
        return Err(Error::Revoked);
    }
    result
}
