//! 只观察原Foundation业务值的实际Weak；不复制字符，也不把标准fragment壳当释放收据。

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
        return Ok(()); // 真实空值不是业务正文；不能把共享空常量包装成非空数据释放证据。
    }
    let string = checked(current, || attributed.string())?;
    if checked(current, || string.length())? != units {
        return Err("实际attributedString与其原string长度不一致".into());
    }
    // 原attributed对象仍完整进入同一owned预算与actual-dealloc门禁，不因其正文只有一个unit放宽。
    checked(current, || ledger.include(roles.0, attributed))??;
    let borrowed_literal = checked(current, || {
        ledger.observe_preexisting_literal_string(roles.1, &string, units, current)
    })??;
    if !borrowed_literal {
        // 只有文档打开前已归还强claim、且仍为同一原Weak身份的固定LF才是借用值。
        // 不按类、tag位、文本相等或事后“缓存”解释放过任何其他String/Frozen/自建对象。
        checked(current, || ledger.include(roles.1, &string))??;
    } // 借用已有明确来源与标量观察记录；不制造该预存原值的“已析构”收据。
    drop(string); // 原 attributed/string 的 Weak 仍归唯一账本；这里只结束本次 getter 的强 claim。
    if !current() {
        return Err("归还原业务字符串临时值后光栅票已撤销".into());
    }
    Ok(())
}

pub(super) struct ParagraphObservation {
    units: usize,
    value: Retained<NSAttributedString>,
}

pub(super) fn capture_paragraph(
    ledger: &NativeMarkdownOwnedLedger,
    fragment: &NSTextLayoutFragment,
    current: &(impl Fn() -> bool + ?Sized),
) -> Result<ParagraphObservation, String> {
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

/// 仅借用同一次原Authority已有的图与element范围；没有第二个持久正文或布局owner。
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
    let source_start = checked(current, || source.location())?;
    let end_location = checked(current, || source.endLocation())?;
    let provider: &ProtocolObject<dyn NSTextElementProvider> = ProtocolObject::from_ref(content);
    let span = checked(current, || {
        provider.offsetFromLocation_toLocation(&source_start, &end_location)
    })?;
    let empty = checked(current, || source.isEmpty())?;
    if span == 0 && empty && paragraph_units == 0 {
        // 只有真实canonical空范围、实际paragraph与它自己的string都确为空，才没有行业务值。
        // 不能只看range空，也不能用某个共享空值替换原来的不可变业务值。
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
    // endLocation是边界，不保证可作为字符位置；只读原已观察string尾部最多两个UTF16单元，
    // 用实际surrogate关系选最后完整scalar的起点，不复制段落、不把low surrogate当独立字符。
    let string = checked(current, || paragraph.value.string())?;
    if checked(current, || string.length())? != paragraph_units {
        return Err("原paragraph string在末行定位时长度变化".into());
    }
    let final_unit = checked(current, || string.characterAtIndex(paragraph_units - 1))?;
    let scalar_units: isize = if (0xdc00..=0xdfff).contains(&final_unit) {
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
    // inside位置使用downstream affinity，归属包含该实际scalar的line；不在end边界上请求上游。
    let last = checked(current, || {
        fragment.textLineFragmentForTextLocation_isUpstreamAffinity(&last_location, false)
    })?;
    if last.is_none() && LAST_LOOKUP_DIAGNOSTICS.fetch_add(1, Ordering::AcqRel) < 8 {
        // 只在原失败点补已有对象的标量；不更换末行票、不读取行数组或正文。
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
    // 只用macOS14公开逐行查询；不取textLineFragments数组，巨段落的离屏line不产生镜像。
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
        // 只有实际末行同一身份才能证明尾部没有后继line；每行string可能是独立子串，长度不能代替身份。
        // 其余情况必须在真实几何上严格前进，不能重复同一line伪造覆盖。
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
        cursor = next.next_up(); // 跨到真实下一行边界，不重复领取同一行的边界坐标。
    }
    Err("可见line业务值观察超过固定预算".into())
}
