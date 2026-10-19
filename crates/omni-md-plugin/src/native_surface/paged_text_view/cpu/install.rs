//! 尚未发布的安装只回滚自己创建的真实关系，不能靠View最终析构或夺取后继层来清理。

use super::*;

#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) mod probe;

pub(super) struct Installation<'a> {
    view: &'a NativeMarkdownPagedTextView,
    backing: Option<Retained<CALayer>>,
    mount: Option<Retained<CALayer>>,
    armed: bool,
}
impl<'a> Installation<'a> {
    pub(super) fn new(view: &'a NativeMarkdownPagedTextView) -> Self {
        Self {
            view,
            backing: None,
            mount: None,
            armed: true,
        }
    }
    pub(super) fn enable(&mut self, origin: &Origin) -> Result<Retained<CALayer>> {
        if !origin.current(self.view) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        self.view.setWantsLayer(true);
        self.backing = self.view.layer(); // 后置current可以撤权，故先记下本次setter实际产生的原backing。
        if !origin.current(self.view) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        self.backing
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)
    }
    pub(super) fn mount(&mut self, claim: Retained<CALayer>) {
        self.mount = Some(claim);
    }
    pub(super) fn commit(&mut self) {
        self.armed = false;
    }
}
impl Drop for Installation<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        CATransaction::begin();
        let transaction = Transaction;
        CATransaction::setDisableActions(true);
        if let (Some(mount), Some(backing)) = (&self.mount, &self.backing) {
            let parent = mount.superlayer();
            let ours = parent
                .as_deref()
                .is_some_and(|parent| std::ptr::eq::<CALayer>(parent, &**backing));
            drop(parent);
            if ours {
                mount.removeFromSuperlayer(); // 只断本次mount仍连原backing的关系，不移走其他owner的新层。
            }
            if mount.superlayer().is_none() {
                // SAFETY: 仅清本次未发布且已经真实分离的mount，没有正文图像或后继owner的关系。
                unsafe { mount.setSublayers(None) };
            }
        }
        let unpublished = self.view.ivars().cpu.borrow().is_none();
        let actual = self.view.layer();
        let same = match (&actual, &self.backing) {
            (Some(actual), Some(backing)) => std::ptr::eq::<CALayer>(&**actual, &**backing),
            (None, None) => true, // 本次启用没有取得backing时，只清仍为nil的原构造关系。
            _ => false,           // 新层或后继CPU owner已出现，回滚不得夺走它。
        };
        drop(actual);
        if unpublished && same && self.view.ivars().cpu.borrow().is_none() {
            self.view.setWantsLayer(false);
            let actual = self.view.layer();
            let ours = actual.as_ref().is_some_and(|actual| {
                self.backing
                    .as_ref()
                    .is_some_and(|backing| std::ptr::eq::<CALayer>(&**actual, &**backing))
            });
            drop(actual);
            if ours && self.view.ivars().cpu.borrow().is_none() {
                self.view.setLayer(None);
            }
        }
        drop(transaction);
        CATransaction::flush(); // 未发布失败也真实提交自己清空的关系，不靠空框架缓存最终回收。
    }
}
