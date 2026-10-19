//! 本次Native安装的独立补偿票；payload被重入清理取走后仍持原图和精确关系身份。

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum AssignmentPoint {
    Image,
    SecondaryImage,
    Background,
    ForegroundColor,
    SecondColor,
    Mask,
    SecondMask,
    Children,
}

impl AssignmentPoint {
    fn bit(self) -> u16 {
        1 << self as u8
    }
}

pub(super) struct Assignment<'a> {
    slot: &'a Slot,
    token: Rc<()>,
    image: CFRetained<CGImage>,
    secondary: Option<CFRetained<CGImage>>,
    presentation: PixelPresentation,
    colors: Option<PairedColors>,
    effects: Cell<u16>,
    committed: bool,
    #[cfg(feature = "native-markdown-runtime-probe")]
    probe: Option<&'a dyn Fn(AssignmentPoint)>,
}

impl<'a> Assignment<'a> {
    pub(super) fn begin(
        slot: &'a Slot,
        image: &CFRetained<CGImage>,
        secondary: Option<&CFRetained<CGImage>>,
        presentation: PixelPresentation,
        palette: &Palette,
    ) -> Result<Self> {
        let paired = matches!(presentation, PixelPresentation::OpaquePairedMasks { .. });
        if paired != secondary.is_some() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if slot.payload.borrow().is_some()
            || slot
                .installing
                .borrow()
                .as_ref()
                .is_some_and(|weak| weak.upgrade().is_some())
        {
            // payload即使已被重入clear取走，原guard仍独占此安装栈；禁止后继抢占同槽，
            // 否则旧Native setter返回的效果可能覆盖新token已经发布的关系。
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let image = image.clone();
        let secondary = secondary.cloned();
        let colors = if presentation == PixelPresentation::OpaqueNeutralMask {
            Some(PairedColors {
                background: palette.white.clone(),
                foreground: palette.black.clone(),
                secondary_foreground: palette.black.clone(),
            })
        } else {
            None
        }; // CF claim全部在RefCell借用之外取得，失败/结束时也不在借用中释放。
        let token = Rc::new(());
        slot.installing.replace(Some(Rc::downgrade(&token)));
        Ok(Self {
            slot,
            token,
            image,
            secondary,
            presentation,
            colors,
            effects: Cell::new(0),
            committed: false,
            #[cfg(feature = "native-markdown-runtime-probe")]
            probe: None,
        })
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn set_probe(&mut self, probe: &'a dyn Fn(AssignmentPoint)) {
        self.probe = Some(probe);
    }

    pub(super) fn owns(&self, token: &Rc<()>) -> bool {
        Rc::ptr_eq(&self.token, token)
    }

    fn active(&self) -> bool {
        self.slot
            .installing
            .borrow()
            .as_ref()
            .is_some_and(|weak| weak.ptr_eq(&Rc::downgrade(&self.token)))
    }

    fn require_payload(&self) -> Result<()> {
        let same = self.active()
            && self
                .slot
                .payload
                .borrow()
                .as_ref()
                .is_some_and(|value| self.owns(&value.token));
        if same {
            Ok(())
        } else {
            Err(NativeMarkdownPagedTextViewError::Revoked)
        }
    }

    pub(super) fn capture_colors(&mut self, colors: Option<&PairedColors>) {
        if let Some(colors) = colors {
            self.colors = Some(PairedColors {
                background: colors.background.clone(),
                foreground: colors.foreground.clone(),
                secondary_foreground: colors.secondary_foreground.clone(),
            });
        } // 仅配对颜色覆盖初始空claim；中性原Palette身份保持，全部CFretain/release均无槽借用。
    }

    pub(super) fn publish_payload(&self, colors: Option<PairedColors>) -> Result<()> {
        if !self.active() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let mut payload = self.slot.payload.borrow_mut();
        if payload.is_some() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        *payload = Some(Payload {
            token: Rc::clone(&self.token),
            image: None,
            secondary_image: None,
            presentation: self.presentation,
            paired_colors: colors,
        }); // 只移动预先持活的颜色和Rust token，没有Native调用或CFretain。
        Ok(())
    }

    pub(super) fn apply(
        &self,
        point: AssignmentPoint,
        current: &impl Fn() -> bool,
        native: impl FnOnce(),
    ) -> Result<()> {
        self.require_payload()?;
        checked(current, || {
            // current的前置回调也可能取走payload；先撤旧票，再记录紧邻真实setter的义务。
            self.require_payload()?;
            self.effects.set(self.effects.get() | point.bit());
            native();
            #[cfg(feature = "native-markdown-runtime-probe")]
            if let Some(probe) = self.probe {
                probe(point); // 精确位于Native效果之后、image/installedCells持久登记之前。
            }
            self.require_payload()
        })??;
        // checked的后置current同样可重入清理；不能因它返回true就给已take的payload补登记。
        self.require_payload()
    }

    pub(super) fn commit(mut self) -> Result<()> {
        self.require_payload()?;
        self.committed = true;
        Ok(()) // 此后真实完整payload接管长期claim，局部票只归还本次安装准入。
    }

    fn touched(&self, point: AssignmentPoint) -> bool {
        self.effects.get() & point.bit() != 0
    }

    fn rollback(&self) {
        if !self.active() {
            return; // 后继token不属于本票，不能以旧guard清它已经接管的关系。
        }
        let payload = {
            let mut payload = self.slot.payload.borrow_mut();
            if payload
                .as_ref()
                .is_some_and(|value| !self.owns(&value.token))
            {
                return;
            }
            payload.take()
        }; // 空payload仍允许本票补偿已发生但尚未入账的效果；强claim由guard独立保留。
        self.slot.background_installed.set(false);
        self.slot.foreground_color_installed.set(false);
        self.slot.second_color_installed.set(false);
        self.slot.mask_installed.set(false);
        self.slot.second_mask_installed.set(false);
        self.slot.foreground_installed.set(false);
        self.slot.second_foreground_installed.set(false);

        if self.touched(AssignmentPoint::Image) {
            let target = match self.presentation {
                PixelPresentation::Image => &self.slot.container,
                PixelPresentation::OpaqueNeutralMask
                | PixelPresentation::OpaquePairedMasks { .. } => &self.slot.mask,
            };
            clear_image(target, &self.image);
        }
        if self.touched(AssignmentPoint::SecondaryImage)
            && let Some(image) = &self.secondary
        {
            clear_image(&self.slot.second_mask, image);
        }
        if self.touched(AssignmentPoint::Mask) {
            clear_mask(&self.slot.foreground, &self.slot.mask);
        }
        if self.touched(AssignmentPoint::SecondMask) {
            clear_mask(&self.slot.second_foreground, &self.slot.second_mask);
        }
        if self.touched(AssignmentPoint::Children) {
            clear_parent(&self.slot.foreground, &self.slot.container);
            if matches!(
                self.presentation,
                PixelPresentation::OpaquePairedMasks { .. }
            ) {
                clear_parent(&self.slot.second_foreground, &self.slot.container);
            }
        }
        if let Some(colors) = &self.colors {
            if self.touched(AssignmentPoint::Background) {
                clear_color(&self.slot.container, &colors.background);
            }
            if self.touched(AssignmentPoint::ForegroundColor) {
                clear_color(&self.slot.foreground, &colors.foreground);
            }
            if self.touched(AssignmentPoint::SecondColor) {
                clear_color(&self.slot.second_foreground, &colors.secondary_foreground);
            }
        }
        drop(payload); // CFRelease与原token归还发生在所有RefCell借用之外，也不持业务状态借用。
    }
}

impl Drop for Assignment<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.rollback();
        }
        if self.active() {
            self.slot.installing.borrow_mut().take();
        } // 先完成所有原关系补偿，才释放本次准入；字段CF claim随后在无槽借用时析构。
    }
}

fn clear_image(layer: &CALayer, expected: &CGImage) {
    // 回滚只需身份标量；局部池归还本次getter的临时claim，不将旧image留到外层事件池。
    let same = objc2::rc::autoreleasepool(|_| {
        // SAFETY: 只读opaque身份；原强claim仍持活，不把外方对象转换成CGImage解引用。
        let actual = unsafe { layer.contents() };
        actual.as_deref().is_some_and(|value| {
            std::ptr::from_ref(value).cast::<()>() == std::ptr::from_ref(expected).cast::<()>()
        })
    });
    if same {
        // SAFETY: 仅仍等于本次原image的Native关系可清理，外方替换原样保留。
        unsafe { layer.setContents(None) };
    }
}

fn clear_mask(foreground: &CALayer, mask: &CALayer) {
    let actual = foreground.mask();
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, mask));
    drop(actual);
    if same {
        // SAFETY: 原固定mask身份仍匹配；外方后设mask不归本安装票所有。
        unsafe { foreground.setMask(None) };
    }
}

fn clear_parent(layer: &CALayer, parent: &CALayer) {
    if parent_is(layer, parent) {
        layer.removeFromSuperlayer();
    } // 只解除本次容器里的原child；外方重挂的父关系绝不夺回。
}

fn clear_color(layer: &CALayer, expected: &CGColor) {
    let actual = layer.backgroundColor();
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, expected));
    drop(actual);
    if same {
        layer.setBackgroundColor(None);
    } // 原RGB相同但CGColor身份不同也属于外方，不能借等值颜色洗掉替换事实。
}
