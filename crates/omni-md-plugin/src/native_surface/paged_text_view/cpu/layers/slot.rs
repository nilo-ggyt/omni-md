//! 固定五层只表达原图、中性遮罩或实际校准的双遮罩；不持正文、Source 或回调强边。

use super::*;
use crate::native_surface::viewport_pixels::{MaskPairBlend, PixelPresentation};
use objc2_core_graphics::CGColor;

mod assignment;
mod cleanup;
mod validate;
use assignment::Assignment;
pub(super) use assignment::AssignmentPoint;

/// 中性表示共享原白黑端点；配对表示另外持有本次安装的精确颜色身份。
pub(super) struct Palette {
    white: CFRetained<CGColor>,
    black: CFRetained<CGColor>,
}
impl Palette {
    pub(super) fn new(current: &impl Fn() -> bool) -> Result<Self> {
        let white = checked(current, || CGColor::new_srgb(1.0, 1.0, 1.0, 1.0))?;
        let black = checked(current, || CGColor::new_srgb(0.0, 0.0, 0.0, 1.0))?;
        Ok(Self { white, black })
    }
}

struct PairedColors {
    background: CFRetained<CGColor>,
    foreground: CFRetained<CGColor>,
    secondary_foreground: CFRetained<CGColor>,
}
impl PairedColors {
    /// 端点必须与校准采用完全相同的 byte/255 sRGB 构造；颜色本身不授予编码资格。
    fn new(
        background: [u8; 3],
        foreground: [u8; 3],
        blend: MaskPairBlend,
        current: &impl Fn() -> bool,
    ) -> Result<Self> {
        let make = |rgb: [u8; 3]| {
            checked(current, || {
                CGColor::new_srgb(
                    f64::from(rgb[0]) / 255.0,
                    f64::from(rgb[1]) / 255.0,
                    f64::from(rgb[2]) / 255.0,
                    1.0,
                )
            })
        };
        let background = make(background)?;
        let foreground = make(foreground)?;
        // 两种表示只分解同一组原RGBA像素；第二次实际合成的颜色身份须随封存证书保存。
        let secondary_foreground = match blend {
            MaskPairBlend::ForegroundThenForeground => foreground.clone(),
            MaskPairBlend::ForegroundThenBackground => background.clone(),
        };
        Ok(Self {
            background,
            foreground,
            secondary_foreground,
        })
    }
}

/// 两次实际内容安装分别登记；中途撤权也能只归还自己的关系。
struct Payload {
    token: Rc<()>,
    image: Option<CFRetained<CGImage>>,
    secondary_image: Option<CFRetained<CGImage>>,
    presentation: PixelPresentation,
    paired_colors: Option<PairedColors>,
}

#[derive(Clone, Copy)]
pub(super) struct Receipt {
    pub(super) tile: PixelTile,
    image: usize,
    secondary_image: Option<usize>,
    presentation: PixelPresentation,
}
impl Receipt {
    pub(super) fn new(
        tile: PixelTile,
        image: &CGImage,
        secondary_image: Option<&CGImage>,
        presentation: PixelPresentation,
    ) -> Self {
        Self {
            tile,
            image: std::ptr::from_ref(image) as usize,
            secondary_image: secondary_image.map(|value| std::ptr::from_ref(value) as usize),
            presentation,
        }
    }
}

pub(super) struct Layout {
    pub(super) rect: NSRect,
    pub(super) scale: f64,
    pub(super) presentation: PixelPresentation,
}

/// 容器自身承担背景，两组前景/遮罩保持实际合成顺序；闲置角色必须完全游离。
pub(super) struct Slot {
    container: Retained<CALayer>,
    foreground: Retained<CALayer>,
    mask: Retained<CALayer>,
    second_foreground: Retained<CALayer>,
    second_mask: Retained<CALayer>,
    payload: RefCell<Option<Payload>>,
    installing: RefCell<Option<std::rc::Weak<()>>>,
    background_installed: Cell<bool>,
    foreground_color_installed: Cell<bool>,
    mask_installed: Cell<bool>,
    foreground_installed: Cell<bool>,
    second_color_installed: Cell<bool>,
    second_mask_installed: Cell<bool>,
    second_foreground_installed: Cell<bool>,
}
impl Slot {
    pub(super) fn new(current: &impl Fn() -> bool) -> Result<Self> {
        Ok(Self {
            container: checked(current, CALayer::new)?,
            foreground: checked(current, CALayer::new)?,
            mask: checked(current, CALayer::new)?,
            second_foreground: checked(current, CALayer::new)?,
            second_mask: checked(current, CALayer::new)?,
            payload: RefCell::new(None),
            installing: RefCell::new(None),
            background_installed: Cell::new(false),
            foreground_color_installed: Cell::new(false),
            mask_installed: Cell::new(false),
            foreground_installed: Cell::new(false),
            second_color_installed: Cell::new(false),
            second_mask_installed: Cell::new(false),
            second_foreground_installed: Cell::new(false),
        })
    }
    pub(super) fn container(&self) -> &Retained<CALayer> {
        &self.container
    }

    pub(super) fn add_weaks(&self, values: &mut Vec<Weak<AnyObject>>) {
        values.extend([
            layer_weak(&self.container),
            layer_weak(&self.foreground),
            layer_weak(&self.mask),
            layer_weak(&self.second_foreground),
            layer_weak(&self.second_mask),
        ]);
    }

    /// 保留既有负控的三个原角色，不能悄悄把外部替换测试转到新增层。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn diagnostic_layers(&self) -> [Retained<CALayer>; 3] {
        [
            self.container.clone(),
            self.foreground.clone(),
            self.mask.clone(),
        ]
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn diagnostic_all_layers(&self) -> [Retained<CALayer>; 5] {
        [
            self.container.clone(),
            self.foreground.clone(),
            self.mask.clone(),
            self.second_foreground.clone(),
            self.second_mask.clone(),
        ]
    }

    pub(super) fn install(
        &self,
        image: &CFRetained<CGImage>,
        secondary_image: Option<&CFRetained<CGImage>>,
        layout: Layout,
        palette: &Palette,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        let assignment =
            Assignment::begin(self, image, secondary_image, layout.presentation, palette)?;
        self.install_assigned(image, secondary_image, layout, palette, current, assignment)
    }

    /// 仅诊断入口可在真实setter效果之后、持久账本登记之前撤权，默认产品不保存hook。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn install_with_probe<'a>(
        &'a self,
        image: &CFRetained<CGImage>,
        secondary_image: Option<&CFRetained<CGImage>>,
        layout: Layout,
        palette: &Palette,
        current: &impl Fn() -> bool,
        probe: &'a dyn Fn(AssignmentPoint),
    ) -> Result<()> {
        let mut assignment =
            Assignment::begin(self, image, secondary_image, layout.presentation, palette)?;
        assignment.set_probe(probe);
        self.install_assigned(image, secondary_image, layout, palette, current, assignment)
    }

    fn install_assigned(
        &self,
        image: &CFRetained<CGImage>,
        secondary_image: Option<&CFRetained<CGImage>>,
        layout: Layout,
        palette: &Palette,
        current: &impl Fn() -> bool,
        mut assignment: Assignment<'_>,
    ) -> Result<()> {
        let paired = matches!(
            layout.presentation,
            PixelPresentation::OpaquePairedMasks { .. }
        );
        if paired != secondary_image.is_some() {
            // 半张配对图或普通表示额外带图都不属于封存器交付的完整瓦片。
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        configure(&self.container, layout.rect, layout.scale, false, current)?;
        let colors = match layout.presentation {
            PixelPresentation::OpaquePairedMasks {
                background,
                foreground,
                blend,
            } => Some(PairedColors::new(background, foreground, blend, current)?),
            PixelPresentation::Image | PixelPresentation::OpaqueNeutralMask => None,
        };
        // 短期强 claim 在槽借用前取得；后续 Native/current 和真实 CFRelease 均不跨 RefCell 借用。
        let color_claims = colors.as_ref().map(|value| {
            (
                value.background.clone(),
                value.foreground.clone(),
                value.secondary_foreground.clone(),
            )
        });
        assignment.capture_colors(colors.as_ref());
        assignment.publish_payload(colors)?;
        let local = NSRect::new(NSPoint::ZERO, layout.rect.size);
        match layout.presentation {
            PixelPresentation::Image => {
                self.install_contents(&self.container, image, false, current, &assignment)?
            }
            PixelPresentation::OpaqueNeutralMask => {
                self.install_mask_geometry(local, layout.scale, false, current)?;
                self.install_colors(&palette.white, &palette.black, None, current, &assignment)?;
                self.install_mask_relations(false, current, &assignment)?;
                self.install_contents(&self.mask, image, false, current, &assignment)?;
            }
            PixelPresentation::OpaquePairedMasks { .. } => {
                let (background, foreground, secondary_foreground) = color_claims
                    .as_ref()
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
                self.install_mask_geometry(local, layout.scale, true, current)?;
                self.install_colors(
                    background,
                    foreground,
                    Some(secondary_foreground),
                    current,
                    &assignment,
                )?;
                self.install_mask_relations(true, current, &assignment)?;
                self.install_contents(&self.mask, image, false, current, &assignment)?;
                self.install_contents(
                    &self.second_mask,
                    secondary_image.ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
                    true,
                    current,
                    &assignment,
                )?;
            }
        }
        assignment.commit()
    }

    fn install_mask_geometry(
        &self,
        rect: NSRect,
        scale: f64,
        paired: bool,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        configure(&self.foreground, rect, scale, false, current)?;
        configure(&self.mask, rect, scale, true, current)?;
        if paired {
            // 第二遮罩使用相同真实局部几何，不能缩放或错位近似原像素。
            configure(&self.second_foreground, rect, scale, false, current)?;
            configure(&self.second_mask, rect, scale, true, current)?;
        }
        Ok(())
    }

    fn install_colors(
        &self,
        background: &CGColor,
        foreground: &CGColor,
        secondary_foreground: Option<&CGColor>,
        current: &impl Fn() -> bool,
        assignment: &Assignment<'_>,
    ) -> Result<()> {
        assignment.apply(AssignmentPoint::Background, current, || {
            self.container.setBackgroundColor(Some(background));
        })?;
        self.background_installed.set(true);
        assignment.apply(AssignmentPoint::ForegroundColor, current, || {
            self.foreground.setBackgroundColor(Some(foreground));
        })?;
        self.foreground_color_installed.set(true);
        if let Some(secondary_foreground) = secondary_foreground {
            // 回混背景和再次加前景必须使用证书记录的原身份，不能在安装后只据RGB推断。
            assignment.apply(AssignmentPoint::SecondColor, current, || {
                self.second_foreground
                    .setBackgroundColor(Some(secondary_foreground));
            })?;
            self.second_color_installed.set(true);
        }
        Ok(())
    }

    fn install_mask_relations(
        &self,
        paired: bool,
        current: &impl Fn() -> bool,
        assignment: &Assignment<'_>,
    ) -> Result<()> {
        assignment.apply(AssignmentPoint::Mask, current, || {
            // SAFETY: 本槽独占的固定标准 CALayer，不持视图、正文或业务回调。
            unsafe { self.foreground.setMask(Some(&self.mask)) };
        })?;
        self.mask_installed.set(true);
        if paired {
            assignment.apply(AssignmentPoint::SecondMask, current, || {
                // SAFETY: 第二组同样只归本槽，实际身份和父关系在关闭及验证时分别核对。
                unsafe { self.second_foreground.setMask(Some(&self.second_mask)) };
            })?;
            self.second_mask_installed.set(true);
        }
        let children = checked(current, || {
            if paired {
                NSArray::from_slice(&[&*self.foreground, &*self.second_foreground])
            } else {
                NSArray::from_slice(&[&*self.foreground])
            }
        })?;
        assignment.apply(AssignmentPoint::Children, current, || {
            // SAFETY: 顺序固定的原前景均完整持活；候选容器尚未显示，不可互换两次合成顺序。
            unsafe { self.container.setSublayers(Some(&children)) };
        })?;
        self.foreground_installed.set(true);
        self.second_foreground_installed.set(paired);
        drop(children);
        Ok(())
    }

    fn install_contents(
        &self,
        target: &CALayer,
        image: &CFRetained<CGImage>,
        secondary: bool,
        current: &impl Fn() -> bool,
        assignment: &Assignment<'_>,
    ) -> Result<()> {
        let claim = image.clone();
        assignment.apply(
            if secondary {
                AssignmentPoint::SecondaryImage
            } else {
                AssignmentPoint::Image
            },
            current,
            || {
                // SAFETY: 原封存 CGImage 完整持活，只以 opaque 内容身份交给 CALayer。
                unsafe {
                    target.setContents(Some(&*std::ptr::from_ref(&**image).cast::<AnyObject>()))
                };
            },
        )?;
        let previous = {
            let mut payload = self.payload.borrow_mut();
            let value = payload
                .as_mut()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
            if !assignment.owns(&value.token) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if secondary {
                value.secondary_image.replace(claim)
            } else {
                value.image.replace(claim)
            }
        };
        drop(previous); // 槽借用已经结束，任何最后 CFRelease 均在其后执行。
        Ok(())
    }
}

fn configure(
    layer: &CALayer,
    rect: NSRect,
    scale: f64,
    mask_plane: bool,
    current: &impl Fn() -> bool,
) -> Result<()> {
    checked(current, || layer.setFrame(rect))?;
    checked(current, || {
        layer.setBounds(NSRect::new(NSPoint::ZERO, rect.size))
    })?;
    checked(current, || layer.setContentsScale(scale))?;
    // A8遮罩只消费alpha，向系统请求单通道存储；实际收益仍须以真实窗口峰值判断。
    // 容器和固色层保留RGBA8；遮罩hint已与原格式逐字节校准，不能据此改源像素。
    // SAFETY: 两个QuartzCore格式常量均为系统静态值，role由当前实际安装目标确定。
    checked(current, || unsafe {
        layer.setContentsFormat(if mask_plane {
            kCAContentsFormatGray8Uint
        } else {
            kCAContentsFormatRGBA8Uint
        })
    })?;
    // SAFETY: 同上，固色和遮罩均消费封存器交付的真实 tile 几何。
    checked(current, || unsafe {
        layer.setContentsGravity(kCAGravityResize)
    })?;
    Ok(())
}
