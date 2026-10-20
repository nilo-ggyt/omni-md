//! 实际 clip 几何变化只唤醒原 continuation；不依赖下一次偶然绘图来推进滚动。

use std::{
    ptr::NonNull,
    sync::{Arc, atomic::Ordering},
};

use block2::RcBlock;
use dispatch2::MainThreadBound;
use objc2::{rc::Weak, runtime::AnyObject};
use objc2_app_kit::{
    NSClipView, NSViewBoundsDidChangeNotification, NSViewFrameDidChangeNotification,
};
use objc2_foundation::{MainThreadMarker, NSNotification, NSNotificationCenter};

use super::{NativeMarkdownPagedBody, NativeMarkdownPagedBodyError, Result};

impl NativeMarkdownPagedBody {
    pub(super) fn observe_clip(&self, clip: &NSClipView) {
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: AppKit 导出的不可变通知名进程内有效；object 精确限制为原 clip。
        let names = unsafe {
            [
                NSViewBoundsDidChangeNotification,
                NSViewFrameDidChangeNotification,
            ]
        };
        let mut tokens = Vec::with_capacity(names.len());
        for name in names {
            let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
            let off_main = Arc::clone(&self.off_main_notice);
            let callback = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                let Some(mtm) = MainThreadMarker::new() else {
                    off_main.store(true, Ordering::Release);
                    return; // 非主线程伪通知只永久撤权，不能访问 Rc、AppKit 或布局状态。
                };
                let Some(owner) = weak.get(mtm).upgrade() else {
                    return;
                };
                if !owner.active.get() {
                    return;
                }
                owner.cancel_mode_for_external_clip(); // 原prepare外的真实clip意图撤旧mode位置，不能反向拉回用户。
                // 既有 Viewport observer 负责同步真实几何；这里只排同一主队列续体。
                // 因而不依赖 observer 注册次序，也不在通知栈里读取混合前后坐标。
                if let Err(cause) = owner.schedule() {
                    owner.fail(cause);
                }
            });
            // SAFETY: object 是本主线程的真实 clip，None queue 沿通知线程同步执行。
            // block 在访问 MainThreadBound/Rc 之前核实主线程，token 在关闭时对称注销。
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    Some(clip),
                    None,
                    &callback,
                )
            };
            tokens.push(token);
        }
        *self.notifications.borrow_mut() = tokens;
    }

    pub(super) fn remove_clip_observers(&self) {
        let tokens = std::mem::take(&mut *self.notifications.borrow_mut());
        let center = NSNotificationCenter::defaultCenter();
        for token in tokens {
            // SAFETY: 只注销本 owner 保存的原 token，不清同名系统通知或其他会话的监听。
            unsafe {
                center.removeObserver(<objc2::runtime::ProtocolObject<
                    dyn objc2_foundation::NSObjectProtocol,
                > as AsRef<AnyObject>>::as_ref(&*token))
            };
        }
    }

    /// 构造账本列出本 Body 实际创建的全部 Native 角色，不以关闭后残留对象筛选清单。
    pub fn native_weaks(&self) -> Result<Vec<(&'static str, Weak<AnyObject>)>> {
        let view = self.view()?;
        let viewport = self
            .viewport
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedBodyError::Closed)?;
        let mut roles = Vec::new();
        let weak = |value: &AnyObject| Weak::from(value);
        roles.push(("body-view", weak(&view)));
        if let Some(typography) = self.typography.borrow().as_ref() {
            roles.push(("preview-font-metrics", weak(&typography.metrics_owner)));
        } // 同一有限系统行标量 owner 也必须在真实关闭后销毁，不能漏在字体/正文弱图之外。
        if let Some(context) = view.inputContext() {
            roles.push(("input-context", weak(&context)));
        }
        let document = viewport.document_view()?;
        roles.push(("scroll-document", weak(&document)));
        for observer in viewport.observer_weaks()? {
            roles.push(("viewport-observer", observer));
        }
        let tokens = self
            .notifications
            .borrow()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for token in tokens {
            let actual =
                <objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol> as AsRef<
                    AnyObject,
                >>::as_ref(&*token);
            roles.push(("body-observer", weak(actual)));
        }
        if let Some(layers) = view.cpu_layer_weaks() {
            roles.extend(layers.into_iter().map(|layer| ("cpu-role", layer)));
        }
        roles.extend(
            view.context_menu_weaks()
                .into_iter()
                .map(|menu| ("edit-menu", menu)),
        );
        if let Some(timer) = view.drag_scroll_weak() {
            roles.push(("drag-scroll-timer", timer));
        }
        // Rc 只用于把 Native claim 移到借用外；清单本身只有弱引用。
        drop((view, viewport));
        Ok(roles)
    }
}
