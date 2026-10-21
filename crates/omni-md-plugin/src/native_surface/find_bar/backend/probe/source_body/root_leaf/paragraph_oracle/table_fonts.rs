//! Choose fonts for whole graphemes in fixed short table fixtures using system candidates and actual
//! CTLine coverage, without product-selected fonts.

use super::*;
use objc2_core_foundation::CFString;
use objc2_core_text::CTLine;
use objc2_foundation::NSAttributedString;

/// Require the same actual font identity, size, matrix, and axes; an equal name cannot certify
/// identical Native font facts.
fn same_font(actual: &CTFont, expected: &CTFont) -> bool {
    // SAFETY: This Native stack retains both immutable fonts; getters borrow neither product fonts nor
    // candidate caches.
    unsafe {
        actual == expected
            && actual.size().to_bits() == expected.size().to_bits()
            && actual.matrix() == expected.matrix()
            && actual.symbolic_traits() == expected.symbolic_traits()
            && actual
                .variation()
                .as_deref()
                .filter(|axes| !axes.is_empty())
                == expected
                    .variation()
                    .as_deref()
                    .filter(|axes| !axes.is_empty())
    }
}

/// CTLine may automatically cascade; whole-grapheme coverage passes only if every actual run still uses
/// the original candidate.
/// This reference accepts bounded graphemes in fixed literals of at most 2 KiB; its 256-glyph arrays do
/// not enlarge product budgets.
fn covers(cluster: &NSString, candidate: &NSFont) -> Result<bool, String> {
    let length = cluster.length();
    if length == 0 || length > 256 {
        return Err("固定短表的整簇字体试次超出原256单元参考范围".into());
    }
    let attributed =
        NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), cluster);
    // SAFETY: Font type and complete short-string range are known; do not mix scalar-specific fallback
    // within one grapheme.
    unsafe {
        attributed.addAttribute_value_range(NSFontAttributeName, candidate, NSRange::new(0, length))
    };
    // Borrow the immutable attributed string through actual Foundation inheritance before its public
    // CFAttributedString bridge.
    let native: &NSAttributedString = &attributed;
    // SAFETY: This public bridge lays out the current fixed short grapheme only; CTLine and retained
    // runs return before function exit.
    let line = unsafe { CTLine::with_attributed_string(native.as_ref()) };
    // SAFETY: The immutable CTLine retains its Source attributes and run array.
    let (range, count, runs) =
        unsafe { (line.string_range(), line.glyph_count(), line.glyph_runs()) };
    if range != CFRange::new(0, length as _) || !(1..=256).contains(&count) {
        return Err("短表原生字体试次没有完整的同簇字形域".into());
    }
    let mut covered = [false; 256];
    let mut glyphs = [0_u16; 256];
    let mut indices = [0_isize; 256];
    let mut total = 0;
    for index in 0..runs.len() {
        // SAFETY: Index belongs to the same live run array; retain no font/run outside this reference
        // graph.
        let raw = unsafe { runs.value_at_index(index as _) };
        let run = NonNull::new(raw.cast_mut().cast::<CTRun>()).ok_or("短表选字 run 为空")?;
        // SAFETY: Read this public immutable run only, checking actual Source range and coverage after
        // delivery.
        let (range, count, attributes) = unsafe {
            (
                run.as_ref().string_range(),
                run.as_ref().glyph_count(),
                run.as_ref().attributes(),
            )
        };
        let start = usize::try_from(range.location).map_err(|_| "短表选字 run 起点无效")?;
        let extent = usize::try_from(range.length).map_err(|_| "短表选字 run 长度无效")?;
        let count = usize::try_from(count).map_err(|_| "短表选字 run 字形数无效")?;
        let end = start.checked_add(extent).ok_or("短表选字 run 末端溢出")?;
        if extent == 0 || end > length || count == 0 || count > 256 {
            return Err("短表选字 run 未落在原完整簇范围内".into());
        }
        // SAFETY: The Core Text font key holds CTFont, retained by its dictionary through this identity
        // check.
        let actual = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
        let actual = NonNull::new(actual.cast_mut().cast::<CTFont>()).ok_or("选字 run 缺 Font")?;
        // Automatic cascade or missing glyphs reject candidate coverage; silent system substitution
        // cannot count as candidate success.
        // SAFETY: The immutable run font dictionary retains actual CTFont; its borrow stays on this
        // synchronous trial stack.
        if !same_font(unsafe { actual.as_ref() }, candidate.as_ref()) {
            return Ok(false);
        }
        // SAFETY: Count fits the fixed arrays; actual Native results determine glyph-zero rejection and
        // cluster validity.
        unsafe {
            run.as_ref().glyphs(
                CFRange::new(0, count as _),
                NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            );
            run.as_ref().string_indices(
                CFRange::new(0, count as _),
                NonNull::new(indices.as_mut_ptr()).unwrap(),
            );
        }
        if glyphs[..count].contains(&0) {
            return Ok(false);
        }
        if indices[..count]
            .iter()
            .any(|index| *index < start as isize || *index >= end as isize)
            || covered[start..end].iter().any(|covered| *covered)
        {
            return Err("短表选字 run 的来源越界或重复覆盖".into());
        }
        covered[start..end].fill(true);
        total += count;
    }
    if total != count as usize || covered[..length].iter().any(|covered| !covered) {
        return Err("短表候选未交付整簇的完整实际来源和字形".into());
    }
    Ok(true)
}

/// Fixture endpoints independently declare Unicode graphemes; validate scalar-aligned complete coverage
/// before selecting fonts for whole graphemes.
/// Try Base first, then request one actual system candidate with the first scalar; verify complete
/// coverage again without name guesses.
pub(super) fn apply(
    source: &str,
    base: &Retained<NSFont>,
    attributed: &NSMutableAttributedString,
    boundaries: &[usize],
) -> Result<(), String> {
    let native = NSString::from_str(source);
    let length = native.length();
    if boundaries.first() != Some(&0) || boundaries.last() != Some(&length) {
        return Err("固定短表 GCB 范围没有完整覆盖原 source".into());
    }
    let mut scalar_ends = source.chars().scan(0_usize, |end, scalar| {
        *end += scalar.len_utf16();
        Some(*end)
    });
    let mut previous = 0;
    for &end in boundaries.iter().skip(1) {
        if end <= previous || scalar_ends.by_ref().find(|at| *at >= end) != Some(end) {
            return Err("固定短表 GCB 范围没有沿原 scalar 边界完整前进".into());
        }
        previous = end;
    }
    for pair in boundaries.windows(2) {
        let range = NSRange::new(pair[0], pair[1] - pair[0]);
        let cluster = native.substringWithRange(range);
        let candidate = if covers(&cluster, base)? {
            base.clone()
        } else {
            let first = cluster.to_string().chars().next().ok_or("固定 EGC 为空")?;
            let mut utf8 = [0; 4];
            let scalar = CFString::from_str(first.encode_utf8(&mut utf8));
            let base: &CTFont = (**base).as_ref();
            // SAFETY: The system receives the same complete first scalar; preserve candidate ordering
            // without reading the product selected font.
            let fallback =
                unsafe { base.for_string(&scalar, CFRange::new(0, first.len_utf16() as _)) };
            let fallback: Retained<CTFont> = fallback.into();
            // SAFETY: Apple public CTFont/NSFont toll-free bridge preserves the same retained ownership
            // claim.
            let fallback: Retained<NSFont> = unsafe { Retained::cast_unchecked(fallback) };
            // The system candidate may change actual face but not original fixture size; names or a
            // constant size cannot replace source facts.
            // SAFETY: The outer original font retains Base; size reads actual points from that same
            // CTFont.
            if fallback.pointSize().to_bits() != unsafe { base.size() }.to_bits() {
                return Err("系统首 scalar 候选改变了原短表字号".into());
            }
            if !covers(&cluster, &fallback)? {
                return Err(format!(
                    "系统首 scalar 候选没有完整覆盖固定 EGC：source={range:?}"
                ));
            }
            fallback
        };
        // SAFETY: Whole-grapheme trials verified actual font identity and coverage; change Font only
        // while preserving semantic colors/paragraph attributes.
        unsafe { attributed.addAttribute_value_range(NSFontAttributeName, &candidate, range) };
    }
    Ok(())
}

/// Recheck original-source font in final TextKit lines; independent grapheme trial success cannot
/// excuse later Native cascade.
pub(super) fn require_run_font(
    text: &NSAttributedString,
    source: usize,
    actual: &CTFont,
) -> Result<(), String> {
    // SAFETY: Source is an actual cluster from the same CTLine; this synchronous stack retains the
    // original attributed string.
    let expected = unsafe {
        text.attribute_atIndex_effectiveRange(NSFontAttributeName, source, std::ptr::null_mut())
    }
    .ok_or("短表最终字形来源没有 Font 属性")?;
    let expected = expected
        .downcast_ref::<NSFont>()
        .ok_or("短表最终 Font 属性不是 NSFont")?;
    if !same_font(actual, expected.as_ref()) {
        return Err(format!(
            "短表最终 Native run 改变了整簇字体身份、字号或轴：source={source}"
        ));
    }
    Ok(())
}
