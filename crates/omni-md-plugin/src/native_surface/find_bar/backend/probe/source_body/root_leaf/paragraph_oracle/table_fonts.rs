//! 固定短表的整 EGC 选字：候选来自系统，覆盖事实来自真实 CTLine，不读取产品已选字体。

use super::*;
use objc2_core_foundation::CFString;
use objc2_core_text::CTLine;
use objc2_foundation::NSAttributedString;

/// 同一候选须保持真实字体身份、字号、矩阵与轴；名称相同不代表同一次 Native 字体事实。
fn same_font(actual: &CTFont, expected: &CTFont) -> bool {
    // SAFETY: 两个不可变字体均由当前原生栈持活，getter 不借用产品字体或候选缓存。
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

/// 原 Native 可以在 CTLine 内自动 cascade；只有所有实际 run 都仍使用原候选，才算整簇通过。
/// 本参考只接固定≤2KiB字面中的有限 EGC；数组上限沿原短行的256项字形参考，不扩展产品能力。
fn covers(cluster: &NSString, candidate: &NSFont) -> Result<bool, String> {
    let length = cluster.length();
    if length == 0 || length > 256 {
        return Err("固定短表的整簇字体试次超出原256单元参考范围".into());
    }
    let attributed =
        NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), cluster);
    // SAFETY: Font 类型与完整短串范围已确定；这里不把逐 scalar 的 fallback 混入同一个 EGC。
    unsafe {
        attributed.addAttribute_value_range(NSFontAttributeName, candidate, NSRange::new(0, length))
    };
    // 先沿真实 Foundation 继承关系借不可变属性串，再使用其公开 CFAttributedString 桥接。
    let native: &NSAttributedString = &attributed;
    // SAFETY: 此公开桥接只布局当前固定短簇；CTLine 和它保有的 run 均在函数返回前归还。
    let line = unsafe { CTLine::with_attributed_string(native.as_ref()) };
    // SAFETY: 不可变 CTLine 的来源与 run 数组均由 line 持活。
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
        // SAFETY: index 来自同一活 run 数组，不把 Font 或 run 保存到参考图之外。
        let raw = unsafe { runs.value_at_index(index as _) };
        let run = NonNull::new(raw.cast_mut().cast::<CTRun>()).ok_or("短表选字 run 为空")?;
        // SAFETY: 所有读取均限定在这个公开不可变 run；来源交付后逐项核范围与覆盖。
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
        // SAFETY: CoreText Font 键的实际值为 CTFont；字典持活它直到本轮身份检查结束。
        let actual = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
        let actual = NonNull::new(actual.cast_mut().cast::<CTFont>()).ok_or("选字 run 缺 Font")?;
        // 自动 cascade 或缺字是该候选未覆盖，不把系统暗中换字后的成功冒充候选自身成功。
        // SAFETY: 原不可变 run 的字体字典仍持活实际 CTFont；借用不离开本次同步试字栈。
        if !same_font(unsafe { actual.as_ref() }, candidate.as_ref()) {
            return Ok(false);
        }
        // SAFETY: count 已核在固定数组内；glyph0 与真实 cluster 均由原生结果本身判断。
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

/// fixture 端点独立声明 UAX 分组；这里只核 scalar 合法与完整拼接，然后按整簇合同选字体。
/// Base 先试，失败才用首 scalar 向系统求一个真实候选；候选必须再次证明完整覆盖，不能猜名字。
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
            // SAFETY: 系统仅接同一个完整首 scalar；这与产品候选顺序一致，不读其已选 Font。
            let fallback =
                unsafe { base.for_string(&scalar, CFRange::new(0, first.len_utf16() as _)) };
            let fallback: Retained<CTFont> = fallback.into();
            // SAFETY: Apple 的 CTFont/NSFont 公开 toll-free 桥接保持同一 +1 所有权。
            let fallback: Retained<NSFont> = unsafe { Retained::cast_unchecked(fallback) };
            // 系统候选只能换实际 face，不能悄悄改变固定短例原字号；不以名称或常量14代替来源。
            // SAFETY: base 仍由外层原字体持活，size 只读同一 CTFont 的实际点数。
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
        // SAFETY: 整簇试次已证明实际 Font 身份与完整覆盖；只替换 Font，保原语义颜色和行属性。
        unsafe { attributed.addAttribute_value_range(NSFontAttributeName, &candidate, range) };
    }
    Ok(())
}

/// 最终 TextKit 行再核同一来源 Font；不能仅凭独立簇试次成功就忽略后继 Native cascade。
pub(super) fn require_run_font(
    text: &NSAttributedString,
    source: usize,
    actual: &CTFont,
) -> Result<(), String> {
    // SAFETY: source 来自同一 CTLine 的实际 cluster，原属性串仍在这个同步栈内持活。
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
