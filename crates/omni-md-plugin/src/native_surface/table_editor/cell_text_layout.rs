//! 可见单格的一份 Core Text 排版同时供测量、绘制和鼠标命中；不读取或持有第二份源码。

use super::*;
use objc2_core_foundation::{
    CFAttributedString, CFRetained, CGAffineTransform, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{CGColor, CGContext, CGMutablePath};
use objc2_core_text::{
    CTFrame, CTFramesetter, CTLine, CTRun, kCTFontAttributeName, kCTForegroundColorAttributeName,
};

/// 排版仅随原可见窗口存活；每行只保存原 CTFrame 内的实际 baseline 和字体边界。
pub(in crate::native_surface) struct NativeMarkdownTableCellTextLayout {
    frame: CFRetained<CTFrame>,
    lines: Vec<CellLine>,
    pub(in crate::native_surface) size: NSSize,
    strikes: Vec<CellStrike>,
}

struct CellLine {
    origin: CGPoint,
    ascent: f64,
    descent: f64,
}

/// Core Text 不绘制 AppKit 的删除线属性；只缓存原视觉 run 的实际端点、字体线宽和颜色。
struct CellStrike {
    start: CGPoint,
    end: CGPoint,
    thickness: f64,
    color: CFRetained<CGColor>,
}

/// 用实际视觉 glyph 的位置与 advance 取 run 边界；指针不可借时只复制固定 32 glyph 栈片段。
fn run_visual_extent(run: &CTRun, count: usize) -> Option<(f64, f64, f64)> {
    // SAFETY: 原不可变 CTFrame/CTLine 数组仍持有 run；返回指针仅在本函数借用。
    let (positions, advances) = unsafe { (run.positions_ptr(), run.advances_ptr()) };
    let mut position_scratch = [CGPoint::new(0.0, 0.0); 32];
    let mut advance_scratch = [CGSize::new(0.0, 0.0); 32];
    let (mut left, mut right, mut baseline) = (f64::INFINITY, f64::NEG_INFINITY, None);
    for start in (0..count).step_by(32) {
        let length = (count - start).min(32);
        let range = CFRange::new(start.try_into().ok()?, length.try_into().ok()?);
        if positions.is_null() {
            // SAFETY: 原实际 count 限定 range，固定缓冲区至少有 length 个可写 CGPoint。
            unsafe { run.positions(range, NonNull::from(&mut position_scratch[0])) };
        }
        if advances.is_null() {
            // SAFETY: 同一实际 glyph 范围，缓冲区可写且不跨同步调用保存输出指针。
            unsafe { run.advances(range, NonNull::from(&mut advance_scratch[0])) };
        }
        for index in 0..length {
            let position = if positions.is_null() {
                position_scratch[index] // Native 明确不给直接指针时消费这次真实片段，不制造全 run 数组。
            } else {
                // SAFETY: count 是该不可变 run 公开实际 glyph 数，start+index 严格在其中。
                unsafe { *positions.add(start + index) }
            };
            let advance = if advances.is_null() {
                advance_scratch[index]
            } else {
                // SAFETY: 同一 count 范围内的原 advance 数组，run 在本函数返回前始终存活。
                unsafe { *advances.add(start + index) }
            };
            if !position.x.is_finite() || !position.y.is_finite() || !advance.width.is_finite() {
                return None; // 非有限实际字体几何不能悄悄转成一条任意长度删除线。
            }
            baseline.get_or_insert(position.y);
            // RTL/重排 run 不按字符次序猜左右端；负 advance 与零 advance mark 同样按真实位置处理。
            left = left.min(position.x.min(position.x + advance.width));
            right = right.max(position.x.max(position.x + advance.width));
        }
    }
    Some((left, right, baseline?))
}

/// 只补 AppKit 专属删除线属性；字体、颜色和方向来自同 frame 的原生实际 run。
fn append_line_strikes(
    line: &CTLine,
    origin: CGPoint,
    strikes: &mut Vec<CellStrike>,
) -> Option<()> {
    // SAFETY: line 由构造中的 frame/lines 数组持有；公开返回数组元素为 CTRun。
    let runs = unsafe { line.glyph_runs() };
    for index in 0..runs.count() {
        // SAFETY: index 严格来自原数组 count，公开元素类型是不可变 CTRun。
        let raw = unsafe { runs.value_at_index(index) };
        let raw = NonNull::new(raw.cast_mut().cast::<CTRun>())?;
        // SAFETY: 原 line/runs 仍存活，借用不逃逸本轮读取。
        let run = unsafe { raw.as_ref() };
        // SAFETY: 返回的是该实际 run 的不可变属性字典，保活至本轮结束。
        let attributes = unsafe { run.attributes() };
        // SAFETY: Core Text 属性字典与 NSDictionary 公开桥接；值由原 attributed string 的固定键类型构造。
        let native: &MarkdownTextAttributes = unsafe { &*std::ptr::from_ref(&*attributes).cast() };
        // SAFETY: AppKit 静态键在本主线程已加载；实际值仍按 NSNumber 下转。
        let style = unsafe { native.objectForKey(NSStrikethroughStyleAttributeName) };
        let Some(style) = style else {
            continue;
        }; // 无删除线的视觉 run 不产生额外装饰或颜色 claim。
        if style.downcast::<NSNumber>().ok()?.unsignedIntegerValue() == 0 {
            continue;
        }
        // SAFETY: Core Text 公开字体键值类型为 CTFont；不是从原请求字体猜 fallback run 的实际字体。
        let font = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
        let font = NonNull::new(font.cast_mut().cast::<CTFont>())?;
        // SAFETY: 原字典持有该实际 CTFont，三个只读字体度量均在此借用内取得。
        let (x_height, cap_height, ascent, thickness) = unsafe {
            let font = font.as_ref();
            (
                font.x_height(),
                font.cap_height(),
                font.ascent(),
                font.underline_thickness(),
            )
        };
        // 无拉丁 x-height 的字体仍用它自己的 cap-height/ascent；不拿另一套默认字体替代真实 run。
        let strike_height = if x_height > 0.0 {
            x_height
        } else if cap_height > 0.0 {
            cap_height
        } else {
            ascent
        } / 2.0;
        if !strike_height.is_finite() || !thickness.is_finite() || thickness <= 0.0 {
            return None;
        }
        // SAFETY: Core Text 公开前景键若存在，其实际类型为 CGColor；字典在 retain 前始终持有它。
        let foreground =
            unsafe { attributes.value(std::ptr::from_ref(kCTForegroundColorAttributeName).cast()) };
        let color = if let Some(color) = NonNull::new(foreground.cast_mut().cast::<CGColor>()) {
            // SAFETY: 上面的公开类型契约及原属性字典保证本 CGColor 可独立 retain。
            unsafe { CFRetained::retain(color) }
        } else {
            // AppKit 创建的原属性可能保留 NSColor 编码；这里只转换同一实际属性，没有默认颜色兜底。
            // SAFETY: 固定 AppKit 键有效，真实对象仍按 NSColor 下转。
            let color = unsafe { native.objectForKey(NSForegroundColorAttributeName) }?
                .downcast::<NSColor>()
                .ok()?;
            let color = color.CGColor();
            // SAFETY: CGColor getter 返回活对象，此独立 CF claim 随可见 layout 释放。
            unsafe { CFRetained::retain(NonNull::from(&*color)) }
        };
        // SAFETY: 不可变 run 的真实 glyph 数与字体矩阵只在此准备阶段读取。
        let (count, matrix) = unsafe { (run.glyph_count(), run.text_matrix()) };
        let count = usize::try_from(count).ok()?;
        if count == 0 {
            continue;
        } // 没有实际 glyph 的 run 不能凭字符串长度制造装饰。
        let (left, right, baseline) = run_visual_extent(run, count)?;
        let transform = |x: f64| {
            CGPoint::new(
                origin.x + matrix.a * x + matrix.c * (baseline + strike_height) + matrix.tx,
                origin.y + matrix.b * x + matrix.d * (baseline + strike_height) + matrix.ty,
            )
        };
        let (start, end) = (transform(left), transform(right));
        if ![start.x, start.y, end.x, end.y]
            .iter()
            .all(|value| value.is_finite())
        {
            return None;
        }
        strikes.push(CellStrike {
            start,
            end,
            thickness,
            color,
        });
    }
    Some(())
}

impl NativeMarkdownTableCellTextLayout {
    #[cfg(feature = "native-markdown-runtime-probe")]
    /// 合成事件只借原 CTLine 的实际 caret，不用等宽估算或创建另一份排版。
    pub(in crate::native_surface) fn probe_point_for_display_index(
        &self,
        index: usize,
        text_frame: NSRect,
    ) -> Option<NSPoint> {
        // SAFETY: 本 layout 持有原不可变 frame，公开数组中的行在本方法结束前存活。
        let lines = unsafe { self.frame.lines() };
        for (ordinal, geometry) in self.lines.iter().enumerate() {
            // SAFETY: ordinal 来自该 frame 的实际行数组，元素公开类型为 CTLine。
            let raw = unsafe { lines.value_at_index(ordinal.try_into().ok()?) };
            let raw = NonNull::new(raw.cast_mut().cast::<CTLine>())?;
            // SAFETY: 原 frame/lines 持有该不可变 line；只读真实源范围与 caret 偏移。
            let (range, offset) = unsafe {
                let line = raw.as_ref();
                (
                    line.string_range(),
                    line.offset_for_string_index(index.try_into().ok()?, std::ptr::null_mut()),
                )
            };
            let start = usize::try_from(range.location).ok()?;
            let end = start.checked_add(usize::try_from(range.length).ok()?)?;
            if index < start || index >= end {
                continue;
            }
            return Some(NSPoint::new(
                text_frame.origin.x + geometry.origin.x + offset + 0.1,
                text_frame.origin.y + self.size.height - geometry.origin.y - geometry.ascent / 2.0,
            ));
        }
        None
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// 删除线像素检查取真实空格两端 caret 的最近视觉间隔，覆盖 RTL 边界的 secondary caret。
    pub(in crate::native_surface) fn probe_strike_space_point(
        &self,
        index: usize,
        text_frame: NSRect,
    ) -> Option<NSPoint> {
        // SAFETY: 原不可变 frame 持有实际 CTLine 数组，方法内无修改或再排版。
        let lines = unsafe { self.frame.lines() };
        for (ordinal, geometry) in self.lines.iter().enumerate() {
            // SAFETY: ordinal 在实际行数内，公开元素类型为 CTLine。
            let raw = unsafe { lines.value_at_index(ordinal.try_into().ok()?) };
            let raw = NonNull::new(raw.cast_mut().cast::<CTLine>())?;
            // SAFETY: 原 frame/lines 保活该行；两个 secondary 输出均为可写栈标量。
            let (range, starts, ends) = unsafe {
                let line = raw.as_ref();
                let (mut a, mut b) = (0.0, 0.0);
                let left = line.offset_for_string_index(index.try_into().ok()?, &mut a);
                let right =
                    line.offset_for_string_index(index.checked_add(1)?.try_into().ok()?, &mut b);
                (line.string_range(), [left, a], [right, b])
            };
            let start = usize::try_from(range.location).ok()?;
            let end = start.checked_add(usize::try_from(range.length).ok()?)?;
            if index < start || index.checked_add(1)? > end {
                continue;
            }
            let (_, x) = starts
                .into_iter()
                .flat_map(|a| ends.map(|b| ((a - b).abs(), (a + b) / 2.0)))
                .filter(|(width, x)| width.is_finite() && *width > 0.2 && x.is_finite())
                .min_by(|a, b| a.0.total_cmp(&b.0))?;
            let x = geometry.origin.x + x;
            let strike = self.strikes.iter().find(|strike| {
                x > strike.start.x.min(strike.end.x)
                    && x < strike.start.x.max(strike.end.x)
                    && (strike.start.y - geometry.origin.y).abs() < geometry.ascent
            })?;
            return Some(NSPoint::new(
                text_frame.origin.x + x,
                text_frame.origin.y + self.size.height - strike.start.y,
            ));
        }
        None
    }

    /// 原 attributed string 已受单格预算限制；最终列宽只排版该值，失败不发布部分几何。
    pub(in crate::native_surface) fn new(
        attributed: &NSAttributedString,
        width: f64,
    ) -> Option<Self> {
        if !width.is_finite() || width <= 0.0 {
            return None; // 无真实内容宽度时不能隐藏原表格或签发点击位置。
        }
        let display_length = attributed.length();
        let cf_attributed: &CFAttributedString = attributed.as_ref();
        // SAFETY: NSAttributedString 与 CFAttributedString 公开桥接；本调用只读原不可变值。
        let setter = unsafe { CTFramesetter::with_attributed_string(cf_attributed) };
        let mut fit = CFRange::new(0, 0);
        // SAFETY: 无附加字典；fit 是本栈可写范围，framesetter 保持原可见属性值存活。
        let suggested = unsafe {
            setter.suggest_frame_size_with_constraints(
                CFRange::new(0, 0),
                None,
                CGSize::new(width, f64::MAX),
                &mut fit,
            )
        };
        if usize::try_from(fit.length).ok()? != display_length
            || fit.location != 0
            || !suggested.width.is_finite()
            || !suggested.height.is_finite()
            || suggested.height < 0.0
        {
            return None; // 有限源没有完整排入时，不能用残缺高度遮盖正文。
        }
        // 向上取整保留实际字体小数边界；空格仍由原最小行高承担，不制造占位字符。
        let height = suggested.height.ceil().max(1.0);
        let path = CGMutablePath::new();
        // SAFETY: path 为本栈独占可变路径；空 transform 表示原坐标，不跨回调保存指针。
        unsafe {
            CGMutablePath::add_rect(
                Some(&path),
                std::ptr::null(),
                CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height)),
            );
        }
        // SAFETY: path 与 setter 在调用期间有效；没有不定型 frameAttributes。
        let frame = unsafe { setter.frame(CFRange::new(0, 0), &path, None) };
        // SAFETY: 只读本栈持有的实际不可变 frame，验证所有可见文字确已排入。
        let visible = unsafe { frame.visible_string_range() };
        if visible.location != 0 || usize::try_from(visible.length).ok()? != display_length {
            return None;
        }
        // SAFETY: CTFrame 的公开返回数组元素均为 CTLine；frame 与数组覆盖本次读取。
        let native_lines = unsafe { frame.lines() };
        let count = usize::try_from(native_lines.count()).ok()?;
        if count == 0 && display_length != 0 {
            return None; // 非空文字没有真实字形行就不可绘制或点击，不能发布一个空投影。
        }
        let mut origins = vec![CGPoint::new(0.0, 0.0); count];
        if let Some(first) = NonNull::new(origins.as_mut_ptr()).filter(|_| count != 0) {
            // SAFETY: 缓冲区恰有实际行数个 CGPoint；非空且只在本同步调用写入。
            unsafe { frame.line_origins(CFRange::new(0, count.try_into().ok()?), first) };
        }
        let mut lines = Vec::with_capacity(count);
        let mut strikes = Vec::new();
        for (index, origin) in origins.into_iter().enumerate() {
            // SAFETY: index 来自数组实际 count，CTFrame 契约保证元素为存活 CTLine。
            let raw = unsafe { native_lines.value_at_index(index.try_into().ok()?) };
            let raw = NonNull::new(raw.cast_mut().cast::<CTLine>())?;
            // SAFETY: 原 frame/array 仍存活，line 不变且只读。
            let line = unsafe { raw.as_ref() };
            let (mut ascent, mut descent) = (0.0, 0.0);
            // SAFETY: 两个输出是本栈可写标量，未使用的 leading 传空。
            unsafe { line.typographic_bounds(&mut ascent, &mut descent, std::ptr::null_mut()) };
            if !origin.x.is_finite()
                || !origin.y.is_finite()
                || !ascent.is_finite()
                || !descent.is_finite()
            {
                return None; // 异常字体几何不能进入绘制或鼠标坐标转换。
            }
            append_line_strikes(line, origin, &mut strikes)?;
            lines.push(CellLine {
                origin,
                ascent,
                descent,
            });
        }
        Some(Self {
            frame,
            lines,
            size: NSSize::new(suggested.width, height),
            strikes,
        })
    }

    /// 在 flipped 正文坐标里绘制原 frame；裁剪、baseline 与 hit 使用同一高度和行对象。
    pub(in crate::native_surface) fn draw(&self, text_frame: NSRect, context: &CGContext) {
        CGContext::save_g_state(Some(context));
        CGContext::clip_to_rect(Some(context), text_frame);
        CGContext::translate_ctm(
            Some(context),
            text_frame.origin.x,
            text_frame.origin.y + self.size.height,
        );
        CGContext::scale_ctm(Some(context), 1.0, -1.0);
        CGContext::set_text_matrix(
            Some(context),
            CGAffineTransform {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                tx: 0.0,
                ty: 0.0,
            },
        );
        // SAFETY: frame 与当前主线程 CGContext 在同步 draw 期间存活，没有布局或源码重建。
        unsafe { self.frame.draw(context) };
        // 同一 CTFrame 的原 run 字形端点仍在当前 y-up 坐标；不触发第二次文字布局。
        for strike in &self.strikes {
            CGContext::set_stroke_color_with_color(Some(context), Some(&strike.color));
            CGContext::set_line_width(Some(context), strike.thickness);
            CGContext::begin_path(Some(context));
            CGContext::move_to_point(Some(context), strike.start.x, strike.start.y);
            CGContext::add_line_to_point(Some(context), strike.end.x, strike.end.y);
            CGContext::stroke_path(Some(context));
        }
        CGContext::restore_g_state(Some(context));
    }

    /// 原预览点位先由真实 CTLine 返回 display UTF16，绝不用源码 field 的几何代替。
    pub(in crate::native_surface) fn display_index_at_point(
        &self,
        point: NSPoint,
        text_frame: NSRect,
        display_length: usize,
    ) -> Option<usize> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return None;
        }
        if display_length == 0 {
            return Some(0); // 空格只有其真实源插入锚点，没有伪造的 glyph。
        }
        let x = point.x - text_frame.origin.x;
        let y = self.size.height - (point.y - text_frame.origin.y);
        // 行距中的点采用最近的实际字形行；边界仍由该行原生 hit 决定。
        let (index, geometry) = self
            .lines
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                let distance = |line: &CellLine| {
                    (y - line.origin.y - line.ascent)
                        .max(line.origin.y - line.descent - y)
                        .max(0.0)
                };
                distance(left).total_cmp(&distance(right))
            })?;
        // SAFETY: 原不可变 frame 仍存活，元素类型来自 CTFrame 的公开 CTLine 数组契约。
        let lines = unsafe { self.frame.lines() };
        // SAFETY: index 来自本 frame 创建时的实际行数，frame 从未修改。
        let raw = unsafe { lines.value_at_index(index.try_into().ok()?) };
        let raw = NonNull::new(raw.cast_mut().cast::<CTLine>())?;
        // SAFETY: frame/lines 在调用期持有该 line，点是原 baseline 的局部坐标。
        let hit = unsafe {
            raw.as_ref().string_index_for_position(CGPoint::new(
                x - geometry.origin.x,
                y - geometry.origin.y,
            ))
        };
        usize::try_from(hit)
            .ok()
            .filter(|index| *index <= display_length)
    }
}
