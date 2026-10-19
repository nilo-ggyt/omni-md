//! 有限行号只保存十个真实数字字形；不创建正文字符串、CTLine或字形缓存。

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2_app_kit::{NSFont, NSFontWeightRegular};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;
use objc2_core_text::{CTFont, CTFontOrientation};

use crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError as Error;

/// SF Mono沿AppKit公开monospaced入口取得；系统共享字体不承担项目对象必须析构的义务。
pub(super) struct LineNumberFont {
    font: Retained<NSFont>,
    glyphs: [u16; 10],
    advances: [f64; 10],
    ascent: f64,
    descent: f64,
}

impl LineNumberFont {
    /// 原CodeMirror样式是14pt等宽数字；实际字体映射/度量失败不能静默换另一套标签。
    pub(super) fn new() -> Result<Self, Error> {
        // SAFETY: AppKit公开的regular权重为进程期不可变标量。
        let font = NSFont::monospacedSystemFontOfSize_weight(14.0, unsafe { NSFontWeightRegular });
        let core: &CTFont = font.as_ref();
        let mut digits = (*b"0123456789").map(u16::from);
        let mut glyphs = [0u16; 10];
        // SAFETY: 两个栈数组均恰有十项；输入是完整BMP数字，输出在同步调用内独占。
        let covered = unsafe {
            core.glyphs_for_characters(
                NonNull::from(&mut digits[0]),
                NonNull::from(&mut glyphs[0]),
                10,
            )
        };
        if !covered || glyphs.contains(&0) {
            return Err(Error::Raster("行号字体缺少实际数字字形".into()));
        }
        let mut sizes = [CGSize::ZERO; 10];
        // SAFETY: 字形输入与CGSize输出各覆盖十项，真实字体与输出缓冲在调用内保持存活。
        unsafe {
            core.advances_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::from(&mut glyphs[0]),
                sizes.as_mut_ptr(),
                10,
            );
        }
        // SAFETY: 同步读取上方真实CTFont的不可变度量，没有正文或裸输出指针。
        let (ascent, descent) = unsafe { (core.ascent(), core.descent()) };
        if sizes
            .iter()
            .any(|size| !size.width.is_finite() || size.width <= 0.0)
            || !ascent.is_finite()
            || !descent.is_finite()
            || ascent <= 0.0
            || descent < 0.0
        {
            return Err(Error::InvalidGeometry);
        }
        Ok(Self {
            font,
            glyphs,
            advances: sizes.map(|size| size.width),
            ascent,
            descent,
        })
    }

    /// 宽度随canonical总行数的十进制位数变化；保留原左5/右3、最小20和1pt右边线。
    pub(super) fn width(&self, lines: usize) -> f64 {
        let mut value = lines;
        let mut digits = 1usize;
        while value >= 10 {
            value /= 10; // 只缩小原总行数标量，不遍历文档或可见行。
            digits += 1;
        }
        // 原CM用同位数的连续9撑宽；border-box的最小20已经包含左右padding。
        (self.advances[9] * digits as f64 + 5.0 + 3.0).max(20.0) + 1.0
    }

    /// 右对齐同一逻辑行的有限数字；usize最多二十位，全部字形和位置只占固定栈数组。
    pub(super) fn draw(&self, context: &CGContext, number: usize, right: f64, frame: CGRect) {
        let mut value = number;
        let mut ordinals = [0usize; 20];
        let mut start = ordinals.len();
        loop {
            start -= 1;
            ordinals[start] = value % 10;
            value /= 10;
            if value == 0 {
                break; // 最低一位也真实绘制，零值不会变成空标签。
            }
        }
        let width: f64 = ordinals[start..]
            .iter()
            .map(|index| self.advances[*index])
            .sum();
        let baseline =
            frame.origin.y + (frame.size.height - self.ascent - self.descent) * 0.5 + self.descent;
        let mut x = right - width;
        let mut glyphs = [0u16; 20];
        let mut positions = [CGPoint::ZERO; 20];
        for (target, ordinal) in ordinals[start..].iter().enumerate() {
            glyphs[target] = self.glyphs[*ordinal];
            positions[target] = CGPoint::new(x, baseline);
            x += self.advances[*ordinal]; // 只消费本标签实际数字的advance，不按均值猜宽度。
        }
        let core: &CTFont = self.font.as_ref();
        // SAFETY: count不超过两个二十项栈数组，字形来自该真实字体；context由本同步位图独占。
        unsafe {
            core.draw_glyphs(
                NonNull::from(&mut glyphs[0]),
                NonNull::from(&mut positions[0]),
                ordinals.len() - start,
                context,
            );
        }
    }
}
