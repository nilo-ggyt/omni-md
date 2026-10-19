//! 暗色与选区只扩展合成验收负载；采样原tile像素与已发布图像，不保全帧副本。

use super::scene::Scene;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSAppearance, NSAppearanceCustomization, NSColor, NSColorSpace, NSWindow};
use objc2_core_foundation::CFType;
use objc2_core_graphics::{
    CGBitmapContextGetBitsPerPixel, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData,
    CGBitmapContextGetHeight, CGBitmapContextGetWidth, CGColorSpace, CGColorSpaceModel, CGContext,
    CGImage, CGImageAlphaInfo,
};
use omni_md_plugin::native_surface::paged_text_view::{
    NativeMarkdownPagedTextScene, NativeMarkdownPagedTextView,
    NativeMarkdownPagedTextViewError as Error,
};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    #[default]
    White,
    Dark,
    Selection,
}

/// 值来自原产品固定palette与同DarkAqua系统选区；不保Appearance或Color原生强边。
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub mode: Mode,
    background: [f64; 4],
    foreground: [f64; 4],
    selected: [f64; 4],
}
impl Default for Theme {
    fn default() -> Self {
        Self {
            mode: Mode::White,
            background: [1.0; 4],
            foreground: [0.0, 0.0, 0.0, 1.0],
            selected: [0.0; 4],
        }
    }
}
impl Theme {
    pub fn capture(mode: Mode, window: &NSWindow) -> Self {
        if mode == Mode::White {
            return Self::default(); // 旧白底对照保持原行为，不提前解析额外系统颜色。
        }
        // SAFETY: 与原window.rs相同的公开静态DarkAqua名称，只有独立合成窗口被设置。
        let appearance =
            NSAppearance::appearanceNamed(unsafe { objc2_app_kit::NSAppearanceNameDarkAqua })
                .expect("原产品DarkAqua必须可用");
        window.setAppearance(Some(&appearance));
        let selected = Cell::new(None);
        let block = block2::StackBlock::new(|| {
            // 解析发生在与原产品相同的DarkAqua drawing appearance中，不借默认浅色缓存的分量。
            let color = NSColor::selectedTextBackgroundColor()
                .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
                .expect("系统选区颜色必须可无损解析到实际sRGB分量");
            selected.set(Some([
                color.redComponent(),
                color.greenComponent(),
                color.blueComponent(),
                color.alphaComponent(),
            ]));
        });
        appearance.performAsCurrentDrawingAppearance(&block);
        let selected = selected.get().expect("同步appearance回调必须实际执行");
        assert!(
            selected
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        );
        let result = Self {
            mode,
            // 原editor.rs的背景与text_attributes.rs的普通文字；不为通过资格改成中性灰。
            background: [0.075, 0.084, 0.11, 1.0],
            foreground: [0.86, 0.88, 0.92, 1.0],
            selected,
        };
        println!(
            "paged_composed_graphics mode={mode:?} actual_product_background={:?} actual_product_foreground={:?} actual_system_selected_srgb={selected:?} actual_appearance=DarkAqua font_changed=false formal=false",
            result.background, result.foreground
        );
        result
    }
}

#[derive(Clone, Copy)]
pub struct Sample {
    background: [u8; 4],
    selected: Option<([u8; 4], usize)>,
}
pub struct PixelFacts {
    pub ink: usize,
    pub selected_fill: usize,
    pub selected_text: usize,
}

/// 生命周期由原context借用覆盖；只读有效width像素，不把stride padding当成内容。
struct Pixels<'a> {
    _context: &'a CGContext,
    address: *const u8,
    width: usize,
    height: usize,
    row: usize,
}
impl<'a> Pixels<'a> {
    fn capture(context: &'a CGContext, current: &dyn Fn() -> bool) -> Result<Self, Error> {
        if !current() {
            return Err(Error::Revoked); // 尚未触碰Native时先拒绝失权原票，不能采后继context。
        }
        CGContext::flush(Some(context));
        let address = CGBitmapContextGetData(Some(context)).cast::<u8>();
        let width = CGBitmapContextGetWidth(Some(context));
        let height = CGBitmapContextGetHeight(Some(context));
        let row = CGBitmapContextGetBytesPerRow(Some(context));
        if address.is_null()
            || CGBitmapContextGetBitsPerPixel(Some(context)) != 32
            || !(1..=512).contains(&width)
            || !(1..=512).contains(&height)
            || width.checked_mul(4).is_none_or(|active| active > row)
            || row
                .checked_mul(height)
                .is_none_or(|bytes| bytes > 2 * 1024 * 1024)
        {
            return Err(Error::InvalidGeometry); // 只读取真实CPU有界RGBA tile；其他context不猜格式。
        }
        if !current() {
            return Err(Error::Revoked); // 公开getter也可能重入；没有后置原票就不能构造可读slice。
        }
        Ok(Self {
            _context: context,
            address,
            width,
            height,
            row,
        })
    }
    fn each(&self, mut visitor: impl FnMut([u8; 4])) {
        for y in 0..self.height {
            // SAFETY: capture已核原context完整有限stride*height；借用期context存活，纯循环内无Native回调。
            let pixels = unsafe {
                std::slice::from_raw_parts(self.address.add(y * self.row), self.width * 4)
            };
            for pixel in pixels.as_chunks::<4>().0 {
                visitor(*pixel);
            }
        }
    }
}

fn fill(context: &CGContext, color: [f64; 4]) {
    CGContext::set_rgb_fill_color(Some(context), color[0], color[1], color[2], color[3]);
}

/// 先用真实几何绘选区，再由原Font/session画字；固定颜色和标量足以验证文字覆盖高亮。
pub fn begin(
    scene: &Scene,
    context: &CGContext,
    current: &dyn Fn() -> bool,
) -> Result<Sample, Error> {
    scene.validate_ready(current)?;
    let theme = scene.paints.theme.get();
    fill(context, theme.background);
    if !current() {
        return Err(Error::Revoked); // Native设色完成后失权，整tile由原候选终态归还。
    }
    CGContext::fill_rect(Some(context), scene.key.request.viewport);
    let background = {
        let pixels = Pixels::capture(context, current)?;
        // SAFETY: 实际完整背景刚填入原有限RGBA tile；capture保证首像素至少4byte可读。
        unsafe { *pixels.address.cast::<[u8; 4]>() }
    };
    if let Some(selection) = &scene.selection {
        fill(context, theme.selected);
        if !current() {
            return Err(Error::Revoked); // 不在失权context上继续安装后继选区。
        }
        let count = scene.window.selection_rects(selection.clone(), current, |rect| {
            CGContext::fill_rect(Some(context), rect);
            if current() { Ok(()) } else {
                Err(omni_md_plugin::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError::Revoked)
            }
        }).map_err(super::scene::error)?;
        if count == 0 {
            return Err(Error::InvalidGeometry); // 原可见行计算出的非空选择必须有真实几何，零矩形不是通过。
        }
    } // 原UTF16范围由实际可见行交集生成；无几何时拒绝，不能用任意彩色矩形冒充选择。
    let selected = {
        let pixels = Pixels::capture(context, current)?;
        let mut candidate = None;
        let mut votes = 0usize;
        pixels.each(|pixel| {
            if pixel == background {
                return; // 原底色不能参与选区主色投票，否则少量高亮会被整块暗底淹没。
            }
            if votes == 0 {
                candidate = Some(pixel);
                votes = 1; // 配对抵消后只有实际出现的颜色可以成为下一候选。
            } else if candidate == Some(pixel) {
                votes += 1; // 同色增加有限标量，不保存像素集合或直方图。
            } else {
                votes -= 1; // 不同边缘覆盖值只抵消票；下面再逐byte验证多数，不把投票当证明。
            }
        }); // 只找有限填色的多数候选；下一遍精确计数证实，不把猜到的颜色当背景事实。
        let mut non_background = 0usize;
        let mut selected_count = 0usize;
        pixels.each(|pixel| {
            non_background += usize::from(pixel != background);
            selected_count += usize::from(Some(pixel) == candidate);
        });
        if non_background == 0 {
            None // 当前tile不与真实选区相交；其他tile仍须贡献实际选中文字像素。
        } else if scene.selection.is_some() && selected_count > non_background / 2 {
            Some((candidate.ok_or(Error::InvalidGeometry)?, selected_count))
        } else {
            return Err(Error::InvalidGeometry); // 不是已验证的单一真实填色就拒绝测试，不能掩盖未知底图。
        }
    };
    fill(context, theme.foreground);
    if !current() {
        return Err(Error::Revoked); // 原字体session尚未创建，失权时不允许后续画字。
    }
    Ok(Sample {
        background,
        selected,
    })
}

impl Sample {
    pub fn observe(
        self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<PixelFacts, Error> {
        let pixels = Pixels::capture(context, current)?;
        let mut ink = 0usize;
        let mut selected_after = 0usize;
        pixels.each(|pixel| {
            let is_selected = self.selected.is_some_and(|(value, _)| pixel == value);
            ink += usize::from(pixel != self.background && !is_selected);
            selected_after += usize::from(is_selected);
        });
        let (selected_fill, selected_text) = if let Some((_, before)) = self.selected {
            (
                before,
                before
                    .checked_sub(selected_after)
                    .ok_or(Error::InvalidGeometry)?,
            )
        } else {
            (0, 0) // 没有选区相交的tile不能贡献虚构的高亮或选中文字像素。
        };
        if !current() {
            return Err(Error::Revoked); // 纯采样结束仍核原票，失权不能签本轮字形像素事实。
        }
        Ok(PixelFacts {
            ink,
            selected_fill,
            selected_text,
        })
    }
}

#[derive(Default, Debug)]
pub struct Presentations {
    pub mask: usize,
    pub paired: usize,
    pub indexed: usize,
    pub rgb24: usize,
    pub rgba: usize,
    pub solid: usize,
}

/// 只观察当前已发布真实CGImage格式；不copy provider数据，不凭原context颜色推测编码选择。
pub fn presentations(
    body: &NativeMarkdownPagedTextView,
    scene: &Scene,
) -> Result<Presentations, Error> {
    let current = || {
        body.capture_ready_scene_key()
            .is_ok_and(|key| key.same(&scene.key))
    };
    if !current() {
        return Err(Error::Revoked); // 未Ready或新nonce不读取其后继Native图组。
    }
    let backing = body.layer().ok_or(Error::InvalidGeometry)?;
    // SAFETY: 此独立harness只使用原CPU owner；Ready与后置原key验证限制读取同一原发布图。
    let mounts = unsafe { backing.sublayers() }.ok_or(Error::InvalidGeometry)?;
    if mounts.len() != 1 {
        return Err(Error::InvalidGeometry); // 唯一CPU挂载点的结构被替换就不能给另一图组记数。
    }
    let mount = mounts.objectAtIndex(0);
    // SAFETY: 同上，只读取有限原mount，不向内容opaque对象发送ObjC业务消息。
    let groups = unsafe { mount.sublayers() }.ok_or(Error::InvalidGeometry)?;
    if groups.len() != 1 {
        return Err(Error::InvalidGeometry); // 原子发布一次只显示一个完整group，不能拼多个半帧统计。
    }
    let group = groups.objectAtIndex(0);
    // SAFETY: 原group只有实际有限tile容器，不能扫描离屏图层或全篇对象。
    let slots = unsafe { group.sublayers() }.ok_or(Error::InvalidGeometry)?;
    if slots.is_empty() || slots.len() > 64 {
        return Err(Error::InvalidGeometry); // 只观察固定有限真实tile，空帧或超过池上界均拒绝。
    }
    let mut result = Presentations::default();
    for index in 0..slots.len() {
        let container = slots.objectAtIndex(index);
        // SAFETY: 实际Image路径contents是原CF图像；空contents时必须验证完整mask路径才读mask图像。
        let direct = unsafe { container.contents() };
        let (content, mask) = if let Some(content) = direct {
            (content, false)
        } else {
            // SAFETY: 原封存A8表示的实际结构必须是唯一前景和实际mask，不接受任意后继child。
            let foregrounds = unsafe { container.sublayers() }.ok_or(Error::InvalidGeometry)?;
            if foregrounds.len() == 2 {
                // 配对表示是一块 tile 的两张独立 A8，逐一核真实 CF 类型、位宽和相同几何，
                // 不能把新增 mask 计成第二块瓦片，也不能仅凭私有 presentation 字段分类。
                let mut dimensions = None;
                let mut first_image = None;
                for plane in 0..2 {
                    let foreground = foregrounds.objectAtIndex(plane);
                    let mask = foreground.mask().ok_or(Error::InvalidGeometry)?;
                    // SAFETY: Ready 原图的 opaque 内容先核实际 CF type，后读图像元数据。
                    let content = unsafe { mask.contents() }.ok_or(Error::InvalidGeometry)?;
                    // SAFETY: 同一已验证slot交付原CGImage；Retained在整个CF类型核对期间持活。
                    // 此处只桥接其CF基类，实际CGImage元数据仍须下方downcast成功后才访问。
                    let value =
                        unsafe { &*std::ptr::from_ref::<AnyObject>(&content).cast::<CFType>() };
                    let image = value
                        .downcast_ref::<CGImage>()
                        .ok_or(Error::InvalidGeometry)?;
                    if !CGImage::is_mask(Some(image)) || CGImage::bits_per_pixel(Some(image)) != 8 {
                        return Err(Error::InvalidGeometry);
                    }
                    let actual = (CGImage::width(Some(image)), CGImage::height(Some(image)));
                    if actual.0 == 0
                        || actual.0 > 512
                        || actual.1 == 0
                        || actual.1 > 512
                        || dimensions.is_some_and(|expected| expected != actual)
                    {
                        return Err(Error::InvalidGeometry);
                    }
                    let identity = std::ptr::from_ref(image) as usize;
                    if first_image == Some(identity) {
                        return Err(Error::InvalidGeometry); // 两个独立 provider 不能偷换成同一图像身份。
                    }
                    first_image = Some(identity);
                    dimensions = Some(actual);
                }
                result.paired += 1;
                continue;
            }
            if foregrounds.len() != 1 {
                return Err(Error::InvalidGeometry); // A8不是任意子图，必须有唯一黑前景的实际关系。
            }
            let foreground = foregrounds.objectAtIndex(0);
            let mask = foreground.mask().ok_or(Error::InvalidGeometry)?;
            // SAFETY: 此mask来自原CPU实际图；下面先用CF type id检查，再读CGImage格式。
            let content = unsafe { mask.contents() }.ok_or(Error::InvalidGeometry)?;
            (content, true)
        };
        // SAFETY: 原CPU只封存CoreFoundation图像，contents原Retained在整个同步读期保活；
        // 先按CFType核真实type id，不把opaque ObjC对象直接当CGImage解引用。
        let value = unsafe { &*std::ptr::from_ref::<AnyObject>(&content).cast::<CFType>() };
        let image = value
            .downcast_ref::<CGImage>()
            .ok_or(Error::InvalidGeometry)?;
        let bits = CGImage::bits_per_pixel(Some(image));
        if mask {
            if !CGImage::is_mask(Some(image)) || bits != 8 {
                return Err(Error::InvalidGeometry); // 真实图像类型和位宽必须同时对应原A8封存表示。
            }
            result.mask += 1;
        } else if bits == 8 {
            let color = CGImage::color_space(Some(image)).ok_or(Error::InvalidGeometry)?;
            if CGColorSpace::model(Some(&color)) != CGColorSpaceModel::Indexed {
                return Err(Error::InvalidGeometry); // 普通8bit灰空间不能冒充已验证的精确sRGB索引图。
            }
            result.indexed += 1;
        } else if bits == 24 && !CGImage::is_mask(Some(image)) {
            let color = CGImage::color_space(Some(image)).ok_or(Error::InvalidGeometry)?;
            if CGImage::bits_per_component(Some(image)) != 8
                || CGImage::alpha_info(Some(image)) != CGImageAlphaInfo::None
                || CGColorSpace::model(Some(&color)) != CGColorSpaceModel::RGB
            {
                return Err(Error::InvalidGeometry); // 三字节只表示全不透明原RGB，不能把含alpha或别的色空间归入。
            }
            result.rgb24 += 1;
        } else if bits == 32 && !CGImage::is_mask(Some(image)) {
            result.rgba += 1;
            result.solid +=
                usize::from(CGImage::width(Some(image)) == 1 && CGImage::height(Some(image)) == 1);
        } else {
            return Err(Error::InvalidGeometry); // 其他Native格式从未由本CPU封存器发布，不能推测分类。
        }
    }
    if !current() {
        return Err(Error::Revoked); // 完成有限观察后再核同key，切换scene的统计不能混入原帧。
    }
    Ok(result)
}
