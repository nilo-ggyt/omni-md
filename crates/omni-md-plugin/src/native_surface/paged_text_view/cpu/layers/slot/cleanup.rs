//! 撤权后仍归还所有原关系；外方后来替换的图像、颜色、遮罩和父层不得被覆盖清除。

use super::*;

impl Slot {
    pub(in super::super) fn clear(&self, group: &CALayer, palette: &Palette) -> bool {
        let payload = self.payload.borrow_mut().take();
        let mut intact = true;
        if let Some(payload) = &payload {
            let target = match payload.presentation {
                PixelPresentation::Image => &*self.container,
                PixelPresentation::OpaqueNeutralMask
                | PixelPresentation::OpaquePairedMasks { .. } => self.primary().1,
            };
            intact &= clear_image(target, payload.image.as_deref());
            if let Some(second_mask) = self.second_mask.as_deref() {
                // 第二 provider 只有实际双遮罩才存在；空 claim 仍不能清除外方后来装入的内容。
                intact &= clear_image(second_mask, payload.secondary_image.as_deref());
            }
        }
        if let (Some(foreground), Some(mask)) = (&self.foreground, &self.mask) {
            intact &= clear_mask(foreground, mask, &self.mask_installed);
            intact &= clear_parent(foreground, &self.container, &self.foreground_installed);
        }
        if let (Some(foreground), Some(mask)) = (&self.second_foreground, &self.second_mask) {
            intact &= clear_mask(foreground, mask, &self.second_mask_installed);
            intact &= clear_parent(
                foreground,
                &self.container,
                &self.second_foreground_installed,
            );
        }
        let colors = payload
            .as_ref()
            .and_then(|value| value.paired_colors.as_ref());
        let (background, foreground, secondary_foreground) = colors.map_or(
            (&*palette.white, &*palette.black, &*palette.black),
            |value| {
                (
                    &*value.background,
                    &*value.foreground,
                    &*value.secondary_foreground,
                )
            },
        );
        if self.background_installed.replace(false) {
            intact &= clear_color(&self.container, background);
        }
        if self.foreground_color_installed.replace(false) {
            intact &= clear_color(self.primary().0, foreground);
        }
        if self.second_color_installed.replace(false) {
            intact &= clear_color(self.secondary().0, secondary_foreground);
        }
        drop(payload); // 关系逐项核对后才归还原图/颜色 claim，不在槽借用中执行 CFRelease。

        let parent = self.container.superlayer();
        if parent
            .as_deref()
            .is_some_and(|value| !std::ptr::eq(value, group))
        {
            intact = false; // 外方重挂保留原样，关闭不能抢回后继父层。
        }
        drop(parent);
        if parent_is(&self.container, group) {
            self.container.removeFromSuperlayer();
        }
        intact &= empty(&self.container);
        for layer in self.auxiliary_layers() {
            // 只检查实际存在的角色；缺席角色没有 Native 资源，不以创建空对象证明释放。
            intact &= empty(layer) && layer.superlayer().is_none();
        }
        intact
    }
}

fn clear_image(layer: &CALayer, expected: Option<&CGImage>) -> bool {
    // 读取只交出身份结果；本次+0 getter的临时claim随局部池结束，不能跨帧拖住旧provider。
    let same = objc2::rc::autoreleasepool(|_| {
        // SAFETY: getter只返回opaque身份；原强claim持活时才比较地址，不解引用外方对象。
        let actual = unsafe { layer.contents() };
        match expected {
            Some(expected) => actual.as_deref().is_some_and(|value| {
                std::ptr::from_ref(value).cast::<()>() == std::ptr::from_ref(expected).cast::<()>()
            }),
            None => actual.is_none(), // 安装未发生的角色不能据空记录清掉任何后继内容。
        }
    });
    if same && expected.is_some() {
        // SAFETY: 只清仍为本槽原图像的关系，外方等值但不同身份的图像也保持。
        unsafe { layer.setContents(None) };
    }
    same
}

fn clear_mask(foreground: &CALayer, mask: &CALayer, installed: &Cell<bool>) -> bool {
    if !installed.replace(false) {
        return true; // 本次没有装过该关系，不获得清理外方 mask 的权限。
    }
    let actual = foreground.mask();
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, mask));
    drop(actual);
    if same {
        // SAFETY: 原固定 mask 的身份仍相同，解除关系不修改外方后来换入的对象。
        unsafe { foreground.setMask(None) };
    }
    same
}

fn clear_parent(layer: &CALayer, parent: &CALayer, installed: &Cell<bool>) -> bool {
    if !installed.replace(false) {
        return true;
    }
    if parent_is(layer, parent) {
        layer.removeFromSuperlayer();
        true
    } else {
        false // 后继改变的父关系必须保留；原关闭收据仍报告撤权。
    }
}

fn clear_color(layer: &CALayer, expected: &CGColor) -> bool {
    let actual = layer.backgroundColor();
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, expected));
    drop(actual);
    if same {
        layer.setBackgroundColor(None);
    }
    same // 只清安装时记录的原颜色身份，不把外方新建的同值颜色冒认为自己。
}

fn empty(layer: &CALayer) -> bool {
    // SAFETY: 公开 getter 只核空值，不向任何外方 opaque 内容发送 CF 或业务消息。
    let contents = unsafe { layer.contents() };
    // SAFETY: 标准 CALayer 的公开 children 数组为空时没有子图资源。
    let children = unsafe { layer.sublayers() };
    contents.is_none()
        && children.as_ref().is_none_or(|value| value.is_empty())
        && !layer.isHidden()
        && layer.opacity() == 1.0
        && layer.zPosition() == 0.0
        && layer.backgroundColor().is_none()
        && layer.mask().is_none()
        && layer.delegate().is_none()
}
