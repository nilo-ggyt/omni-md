//! Observes actual weak identities of original Foundation values without copying their characters.
//! A standard fragment shell is not proof that its document-bearing values were released.

use super::*;
use objc2_app_kit::{
    NSTextContentManager, NSTextLayoutFragment, NSTextLayoutFragmentState, NSTextLayoutManager,
    NSTextLocation, NSTextParagraph,
};

const MAX_VISIBLE_LINE_OBSERVATIONS: usize = 256;
static LAST_LOOKUP_DIAGNOSTICS: AtomicUsize = AtomicUsize::new(0);

fn checked<T>(
    current: &(impl Fn() -> bool + ?Sized),
    work: impl FnOnce() -> T,
) -> Result<T, String> {
    // Observation getters and ledger admission may reenter. Neither can advance to another value
    // after the original paint's source/geometry authority has been revoked.
    if !current() {
        return Err("业务值观察前原光栅票已撤销".into());
    }
    let result = work();
    if !current() {
        return Err("业务值观察中原光栅票已撤销".into());
    }
    Ok(result)
}

fn capture_attributed(
    ledger: &NativeMarkdownOwnedLedger,
    attributed: &NSAttributedString,
    units: usize,
    roles: (&'static str, &'static str),
    current: &(impl Fn() -> bool + ?Sized),
) -> Result<(), String> {
    if units == 0 {
        return Ok(()); // A real empty value carries no body data; shared empty constants cannot prove its release.
    }
    let string = checked(current, || attributed.string())?;
    if checked(current, || string.length())? != units {
        return Err("实际attributedString与其原string长度不一致".into());
    }
    // Record the original attributed value under the same owned budget and actual-deallocation gate,
    // including one-unit values; small content size does not exempt a retained business value.
    checked(current, || ledger.include(roles.0, attributed))??;
    let borrowed_literal = checked(current, || {
        ledger.observe_preexisting_literal_string(roles.1, &string, units, current)
    })??;
    if !borrowed_literal {
        // Only the known LF literal, observed weakly before opening the document and after returning
        // its strong claim, may be borrowed. Class, tagging, equal text, or a later cache explanation
        // cannot exempt another NSString, frozen value, or project-created object from the ledger.
        checked(current, || ledger.include(roles.1, &string))??;
    } // A borrowed literal has explicit provenance, not a fabricated deallocation receipt.
    // The unique ledger keeps only weak observations; return this getter's temporary strong claim.
    drop(string);
    if !current() {
        return Err("归还原业务字符串临时值后光栅票已撤销".into());
    }
    Ok(())
}

pub(super) struct ParagraphObservation {
    // Hold only the framework's original attributed value during this synchronous observation.
    // This reference route does not make paragraph getters evidence of strict paged-body laziness.
    units: usize,
    value: Retained<NSAttributedString>,
}

pub(super) fn capture_paragraph(
    ledger: &NativeMarkdownOwnedLedger,
    fragment: &NSTextLayoutFragment,
    current: &(impl Fn() -> bool + ?Sized),
) -> Result<ParagraphObservation, String> {
    // Observe the actual fragment's paragraph rather than a reconstructed string or substituted value.
    // A non-paragraph element cannot silently count as complete business-value coverage.
    let element = checked(current, || fragment.textElement())?
        .ok_or("标准fragment缺少原text element，业务覆盖不完整")?;
    let paragraph = checked(current, || element.downcast_ref::<NSTextParagraph>())?
        .ok_or("标准fragment不是可证明的paragraph，业务覆盖不完整")?;
    let attributed = checked(current, || paragraph.attributedString())?;
    let units = checked(current, || attributed.length())?;
    capture_attributed(
        ledger,
        &attributed,
        units,
        (
            "editor.raster.paragraph-value",
            "editor.raster.paragraph-string",
        ),
        current,
    )?;
    drop(element);
    if !current() {
        return Err("归还原paragraph临时claim后光栅票已撤销".into());
    }
    Ok(ParagraphObservation {
        units,
        value: attributed,
    })
}

/// Borrows the same Authority's content/layout graph, without another persistent document or layout owner.
pub(super) struct VisibleLineObservation<'a> {
    pub(super) content: &'a NSTextContentManager,
    pub(super) manager: &'a NSTextLayoutManager,
}

pub(super) fn capture_visible_lines(
    ledger: &NativeMarkdownOwnedLedger,
    original: VisibleLineObservation<'_>,
    fragment: &NSTextLayoutFragment,
    frame: NSRect,
    visible: NSRect,
    origin: NSPoint,
    current: &(impl Fn() -> bool + ?Sized),
) -> Result<(), String> {
    let (content, manager) = (original.content, original.manager);
    if checked(current, || fragment.state())? != NSTextLayoutFragmentState::LayoutAvailable {
        return Err("观察可见line时标准fragment不在真实state3".into());
    }
    let paragraph = capture_paragraph(ledger, fragment, current)?;
    let paragraph_units = paragraph.units;
    let top = visible.origin.y - origin.y - frame.origin.y;
    let bottom = visible.origin.y + visible.size.height - origin.y - frame.origin.y;
    if !top.is_finite() || !bottom.is_finite() || bottom <= top {
        return Err("观察可见line时原视口坐标无效".into());
    }
    let source = checked(current, || fragment.rangeInElement())?;
    // Relate the original paragraph length to its exact provider range before selecting visible lines.
    let source_start = checked(current, || source.location())?;
    let end_location = checked(current, || source.endLocation())?;
    let provider: &ProtocolObject<dyn NSTextElementProvider> = ProtocolObject::from_ref(content);
    let span = checked(current, || {
        provider.offsetFromLocation_toLocation(&source_start, &end_location)
    })?;
    let empty = checked(current, || source.isEmpty())?;
    if span == 0 && empty && paragraph_units == 0 {
        // Only an actual empty canonical range, paragraph, and that paragraph's own string prove
        // absence of line data. An empty range alone or a substituted shared empty value does not.
        let actual_empty_string = checked(current, || paragraph.value.string())?;
        if checked(current, || actual_empty_string.length())? != 0 {
            return Err("原空paragraph的实际string仍含业务值".into());
        }
        drop(actual_empty_string);
        let canonical = checked(current, || {
            provider.locationFromLocation_withOffset(&source_start, 0)
        })?
        .ok_or("真实空fragment缺少合法canonical位置")?;
        if checked(current, || canonical.compare(&end_location))?
            != objc2_foundation::NSComparisonResult::Same
        {
            return Err("真实空fragment的canonical位置不匹配原范围".into());
        }
        return Ok(());
    }
    if empty || span <= 0 || usize::try_from(span).ok() != Some(paragraph_units) {
        return Err("标准fragment范围与实际非空paragraph长度不一致".into());
    }
    // endLocation is a boundary, not necessarily a character position. Read at most two UTF-16
    // units from the already observed original string to locate its last whole scalar; never
    // copy the paragraph or treat a low surrogate as a standalone character.
    let string = checked(current, || paragraph.value.string())?;
    if checked(current, || string.length())? != paragraph_units {
        return Err("原paragraph string在末行定位时长度变化".into());
    }
    let final_unit = checked(current, || string.characterAtIndex(paragraph_units - 1))?;
    let scalar_units: isize = if (0xdc00..=0xdfff).contains(&final_unit) {
        // A trailing low surrogate requires its actual preceding high surrogate from this same value.
        if paragraph_units < 2 {
            return Err("原paragraph末尾包含孤立low surrogate".into());
        }
        let before = checked(current, || string.characterAtIndex(paragraph_units - 2))?;
        if !(0xd800..=0xdbff).contains(&before) {
            return Err("原paragraph末尾surrogate配对无效".into());
        }
        2
    } else if (0xd800..=0xdbff).contains(&final_unit) {
        return Err("原paragraph末尾包含孤立high surrogate".into());
    } else {
        1
    };
    drop(string);
    let last_location = checked(current, || {
        provider.locationFromLocation_withOffset(&end_location, -scalar_units)
    })?
    .ok_or("最后完整scalar缺少原content合法位置")?;
    let before_end = checked(current, || {
        provider.offsetFromLocation_toLocation(&last_location, &end_location)
    })?;
    let from_start = checked(current, || {
        provider.offsetFromLocation_toLocation(&source_start, &last_location)
    })?;
    let roundtrip = checked(current, || {
        provider.locationFromLocation_withOffset(&last_location, scalar_units)
    })?
    .ok_or("最后完整scalar不能roundtrip至原endLocation")?;
    // Authenticate the inside position through signed offsets, ordering, and a round-trip to EOF.
    // This avoids using an adjacent fragment's line merely because it shares an endpoint boundary.
    if before_end != scalar_units
        || from_start != span - scalar_units
        || from_start < 0
        || checked(current, || roundtrip.compare(&end_location))?
            != objc2_foundation::NSComparisonResult::Same
        || checked(current, || last_location.compare(&source_start))?
            == objc2_foundation::NSComparisonResult::Ascending
        || checked(current, || last_location.compare(&end_location))?
            != objc2_foundation::NSComparisonResult::Ascending
    {
        return Err("末行inside位置未通过原source offset和roundtrip认证".into());
    }
    if !checked(current, || fragment.textLayoutManager())?
        .as_deref()
        .is_some_and(|actual| std::ptr::eq::<NSTextLayoutManager>(actual, manager))
        || !checked(current, || {
            manager.textLayoutFragmentForLocation(&last_location)
        })?
        .as_deref()
        .is_some_and(|actual| std::ptr::eq::<NSTextLayoutFragment>(actual, fragment))
    {
        return Err("末行inside位置不属于同一原manager和fragment".into());
    }
    // Downstream affinity at the inside position chooses the line containing that actual scalar,
    // instead of requesting upstream affinity at the ambiguous end boundary.
    let last = checked(current, || {
        fragment.textLineFragmentForTextLocation_isUpstreamAffinity(&last_location, false)
    })?;
    if last.is_none() && LAST_LOOKUP_DIAGNOSTICS.fetch_add(1, Ordering::AcqRel) < 8 {
        // At this failure site, collect only existing object scalars. Do not replace the last-line
        // permit, read the complete line array, or materialize document text to improve diagnostics.
        let provider: &ProtocolObject<dyn NSTextElementProvider> =
            ProtocolObject::from_ref(content);
        let document = checked(current, || provider.documentRange())?;
        let document_origin = checked(current, || document.location())?;
        let source_start = checked(current, || source.location())?;
        let from = checked(current, || {
            provider.offsetFromLocation_toLocation(&document_origin, &source_start)
        })?;
        let to = checked(current, || {
            provider.offsetFromLocation_toLocation(&document_origin, &end_location)
        })?;
        let empty = checked(current, || source.isEmpty())?;
        let first = checked(current, || {
            fragment.textLineFragmentForVerticalOffset_requiresExactMatch(top, false)
        })?;
        let first_range = match first.as_ref() {
            Some(first) => checked(current, || first.characterRange())?,
            None => NSRange::new(NSNotFound as usize, 0),
        };
        let last_range = match last.as_ref() {
            Some(last) => checked(current, || last.characterRange())?,
            None => NSRange::new(NSNotFound as usize, 0),
        };
        let same_identity = first
            .as_deref()
            .zip(last.as_deref())
            .is_some_and(|(a, b)| std::ptr::eq::<objc2_app_kit::NSTextLineFragment>(a, b));
        eprintln!(
            "native_raster_surface event=line_lookup_diagnostic source_start={from} source_end={to} source_empty={empty} paragraph_units={paragraph_units} frame={},{},{},{} visible={},{},{},{} top={top} bottom={bottom} last_inside_from_end={scalar_units} last_present={} first_vertical_present={} same_identity={same_identity} last_range={},{} first_range={},{} formal=false",
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
            visible.origin.x,
            visible.origin.y,
            visible.size.width,
            visible.size.height,
            last.is_some(),
            first.is_some(),
            last_range.location,
            last_range.length,
            first_range.location,
            first_range.length,
        );
        drop(first);
        if !current() {
            return Err("末行诊断原票已撤销".into());
        }
    }
    let last = last.ok_or("标准fragment缺少可证明的末行身份")?;
    let mut cursor = top;
    // Use macOS 14's public per-line lookup. Do not obtain textLineFragments or mirror the offscreen
    // lines of a giant paragraph; the fixed observation budget bounds this synchronous scan.
    for _ in 0..MAX_VISIBLE_LINE_OBSERVATIONS {
        let line = checked(current, || {
            fragment.textLineFragmentForVerticalOffset_requiresExactMatch(cursor, false)
        })?
        .ok_or("可见标准fragment缺少实际line，业务覆盖不完整")?;
        let bounds = checked(current, || line.typographicBounds())?;
        let range = checked(current, || line.characterRange())?;
        if !geometry::finite(bounds) || bounds.size.height <= 0.0 || bounds.size.width < 0.0 {
            return Err("实际可见line的typographicBounds无效".into());
        }
        let attributed = checked(current, || line.attributedString())?;
        let units = checked(current, || attributed.length())?;
        // The framework's original line range must fit its actual attributed value before observation.
        range
            .location
            .checked_add(range.length)
            .filter(|end| *end <= units)
            .ok_or("实际可见line的characterRange越出原业务值")?;
        capture_attributed(
            ledger,
            &attributed,
            units,
            ("editor.raster.line-value", "editor.raster.line-string"),
            current,
        )?;
        let next = bounds.origin.y + bounds.size.height;
        // Only the actual last-line identity proves no successor exists. A line string can be an
        // independent substring, so its length cannot replace identity. Otherwise require strict
        // progress through actual geometry instead of counting the same line as fresh coverage.
        let complete = std::ptr::eq::<objc2_app_kit::NSTextLineFragment>(&*line, &*last)
            || bounds.origin.y >= bottom
            || next >= bottom;
        drop(attributed);
        drop(line);
        if !current() {
            return Err("归还可见line临时claim后原票已撤销".into());
        }
        if complete {
            return Ok(());
        }
        if !next.is_finite() || next <= cursor {
            return Err("实际可见line未严格前进，业务覆盖不完整".into());
        }
        // Cross the real next-line boundary so a repeated boundary lookup cannot return the same line.
        cursor = next.next_up();
    }
    Err("可见line业务值观察超过固定预算".into())
}
