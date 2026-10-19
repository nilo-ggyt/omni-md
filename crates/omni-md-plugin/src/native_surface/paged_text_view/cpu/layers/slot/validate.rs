//! 每次消费完整槽都核实际图像、颜色、顺序、遮罩和父关系，任何外部替换均撤销原 Ready。

use super::*;
use objc2::rc::autoreleasepool;

/// 这组纯标量只描述一张已封存遮罩，不保 Native 图像或可变槽借用。
struct MaskIdentity {
    rect: NSRect,
    scale: f64,
    color: usize,
    image: usize,
}

impl Slot {
    pub(in super::super) fn validate(
        &self,
        receipt: &Receipt,
        rect: NSRect,
        scale: f64,
        palette: &Palette,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        let facts = {
            let payload = self
                .payload
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedTextViewError::Pending)?;
            payload.as_ref().map(|value| {
                (
                    value.presentation,
                    value
                        .image
                        .as_ref()
                        .map(|image| std::ptr::from_ref(&**image) as usize),
                    value
                        .secondary_image
                        .as_ref()
                        .map(|image| std::ptr::from_ref(&**image) as usize),
                    value.paired_colors.as_ref().map(|colors| {
                        (
                            std::ptr::from_ref(&*colors.background) as usize,
                            std::ptr::from_ref(&*colors.foreground) as usize,
                            std::ptr::from_ref(&*colors.secondary_foreground) as usize,
                        )
                    }),
                )
            })
        }; // 只提取身份标量，结束槽借用后才进入 Native/current。
        let Some((presentation, image, secondary, colors)) = facts else {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        };
        require(
            presentation == receipt.presentation
                && image == Some(receipt.image)
                && secondary == receipt.secondary_image,
            current,
        )?;
        let paired = matches!(
            receipt.presentation,
            PixelPresentation::OpaquePairedMasks { .. }
        );
        require(
            paired == receipt.secondary_image.is_some() && paired == colors.is_some(),
            current,
        )?;
        common(&self.container, rect, scale, false, current)?;
        let local = NSRect::new(NSPoint::ZERO, rect.size);
        no_mask(&self.container, current)?;
        match receipt.presentation {
            PixelPresentation::Image => {
                // 原图只用容器 contents，四个辅助层必须真正空闲。
                no_color(&self.container, current)?;
                no_children(&self.container, current)?;
                for layer in [
                    &*self.foreground,
                    &*self.mask,
                    &*self.second_foreground,
                    &*self.second_mask,
                ] {
                    empty(layer, current)?;
                }
                contents(&self.container, receipt.image, current)?;
            }
            PixelPresentation::OpaqueNeutralMask => {
                // 白黑路径保持原单遮罩行为，不能因新增两层而残留第二张图。
                color(
                    &self.container,
                    std::ptr::from_ref(&*palette.white) as usize,
                    current,
                )?;
                no_contents(&self.container, current)?;
                mask_group(
                    &self.container,
                    &self.foreground,
                    &self.mask,
                    MaskIdentity {
                        rect: local,
                        scale,
                        color: std::ptr::from_ref(&*palette.black) as usize,
                        image: receipt.image,
                    },
                    current,
                )?;
                children(&self.container, &[&*self.foreground], current)?;
                empty(&self.second_foreground, current)?;
                empty(&self.second_mask, current)?;
            }
            PixelPresentation::OpaquePairedMasks { .. } => {
                let (background, foreground, secondary_foreground) =
                    colors.ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
                color(&self.container, background, current)?;
                no_contents(&self.container, current)?;
                mask_group(
                    &self.container,
                    &self.foreground,
                    &self.mask,
                    MaskIdentity {
                        rect: local,
                        scale,
                        color: foreground,
                        image: receipt.image,
                    },
                    current,
                )?;
                mask_group(
                    &self.container,
                    &self.second_foreground,
                    &self.second_mask,
                    MaskIdentity {
                        rect: local,
                        scale,
                        color: secondary_foreground,
                        image: receipt
                            .secondary_image
                            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?,
                    },
                    current,
                )?;
                children(
                    &self.container,
                    &[&*self.foreground, &*self.second_foreground],
                    current,
                )?;
            }
        }
        Ok(())
    }
}

/// 每组前景/遮罩分别验证；实际 mask.superlayer 也必须仍指向原前景。
fn mask_group(
    container: &CALayer,
    foreground: &CALayer,
    mask: &CALayer,
    identity: MaskIdentity,
    current: &impl Fn() -> bool,
) -> Result<()> {
    common(foreground, identity.rect, identity.scale, false, current)?;
    common(mask, identity.rect, identity.scale, true, current)?;
    color(foreground, identity.color, current)?;
    no_contents(foreground, current)?;
    no_children(foreground, current)?;
    no_children(mask, current)?;
    no_color(mask, current)?;
    no_mask(mask, current)?;
    let actual = checked(current, || foreground.mask())?;
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, mask));
    drop(actual);
    require(same, current)?;
    parent(foreground, container, current)?;
    parent(mask, foreground, current)?;
    contents(mask, identity.image, current)
}

fn children(layer: &CALayer, expected: &[&CALayer], current: &impl Fn() -> bool) -> Result<()> {
    // SAFETY: 公开数组只比较类型正确的原层身份，不向外方层发送业务消息。
    let actual = checked(current, || unsafe { layer.sublayers() })?;
    let same = if let Some(actual) = actual {
        if checked(current, || actual.len())? != expected.len() {
            false // 数量变化不能用原私有字段仍持活来冒充挂载成功。
        } else {
            let mut same = true;
            for (index, expected) in expected.iter().enumerate() {
                let value = checked(current, || actual.objectAtIndex(index))?;
                same &= std::ptr::eq::<CALayer>(&*value, *expected);
            }
            same
        }
    } else {
        expected.is_empty()
    };
    require(same, current)
}

fn parent(layer: &CALayer, expected: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    let actual = checked(current, || layer.superlayer())?;
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, expected));
    drop(actual);
    require(same, current)
}

fn contents(layer: &CALayer, expected: usize, current: &impl Fn() -> bool) -> Result<()> {
    // 这次读取只需要身份标量；+0 getter 的临时 claim 必须随本次消费结束，不能留在
    // 外层事件池中继续持有刚验证过的整组像素，延长到后来关闭正文之后才释放。
    let same = autoreleasepool(|_| -> Result<bool> {
        // SAFETY: 只比较原 opaque CGImage 身份，不把外方 contents 当 CF 图像解引用。
        let actual = checked(current, || unsafe { layer.contents() })?;
        Ok(actual
            .as_deref()
            .is_some_and(|value| std::ptr::from_ref(value) as usize == expected))
    })?;
    require(same, current)
}

fn require(fact: bool, current: &impl Fn() -> bool) -> Result<()> {
    if fact && current() {
        Ok(())
    } else {
        Err(NativeMarkdownPagedTextViewError::Revoked)
    }
}

fn common(
    layer: &CALayer,
    rect: NSRect,
    scale: f64,
    mask_plane: bool,
    current: &impl Fn() -> bool,
) -> Result<()> {
    // 任一辅助层被隐藏或改透明都会改变实际合成，不能仅凭图像和父关系相同继续签 Ready。
    require(!checked(current, || layer.isHidden())?, current)?;
    require(checked(current, || layer.opacity())? == 1.0, current)?;
    // 实际Z顺序会覆盖children的数组顺序；背景回混不能接受仅数组身份不变的反向叠放。
    require(checked(current, || layer.zPosition())? == 0.0, current)?;
    require(checked(current, || layer.frame())? == rect, current)?;
    require(
        checked(current, || layer.bounds())? == NSRect::new(NSPoint::ZERO, rect.size),
        current,
    )?;
    require(
        checked(current, || layer.contentsScale())? == scale,
        current,
    )?;
    let format = checked(current, || layer.contentsFormat())?;
    require(
        checked(current, || {
            // SAFETY: 系统常量静态有效；实际mask只认单通道hint，其余层仍要求RGBA8。
            // 外部替换hint会撤权，不能只因原图像身份仍相同而签发Ready。
            unsafe {
                format.isEqualToString(if mask_plane {
                    kCAContentsFormatGray8Uint
                } else {
                    kCAContentsFormatRGBA8Uint
                })
            }
        })?,
        current,
    )?;
    let gravity = checked(current, || layer.contentsGravity())?;
    require(
        checked(current, || {
            // SAFETY: 同一 resize 常量只消费原真实瓦片几何，不接受后继截图缩放。
            unsafe { gravity.isEqualToString(kCAGravityResize) }
        })?,
        current,
    )?;
    require(checked(current, || layer.delegate())?.is_none(), current)
}

fn color(layer: &CALayer, expected: usize, current: &impl Fn() -> bool) -> Result<()> {
    let actual = checked(current, || layer.backgroundColor())?;
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::from_ref(value) as usize == expected);
    drop(actual);
    require(same, current)
}
fn no_color(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    require(
        checked(current, || layer.backgroundColor())?.is_none(),
        current,
    )
}
fn no_mask(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    require(checked(current, || layer.mask())?.is_none(), current)
}
fn no_children(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    // SAFETY: 标准 CALayer 的公开数组只核空值，不进入任何外方绘制。
    let actual = checked(current, || unsafe { layer.sublayers() })?;
    let empty = actual.as_ref().is_none_or(|value| value.is_empty());
    drop(actual);
    require(empty, current)
}
fn no_contents(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    // SAFETY: opaque getter 只核空，不访问外方对象的 CF 内容。
    require(
        checked(current, || unsafe { layer.contents() })?.is_none(),
        current,
    )
}
fn empty(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    no_contents(layer, current)?;
    no_children(layer, current)?;
    no_color(layer, current)?;
    no_mask(layer, current)?;
    require(checked(current, || layer.superlayer())?.is_none(), current)?;
    require(checked(current, || layer.delegate())?.is_none(), current)
}
