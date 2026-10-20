//! 每个候选只有一条实际主队列延续；闭包仅存原view/surface的Weak与标量serial。

use super::*;
use dispatch2::MainThreadBound;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) mod probe;

impl CpuSurface {
    pub(super) fn schedule(
        self: &Rc<Self>,
        view: &NativeMarkdownPagedTextView,
        serial: u64,
    ) -> Result<()> {
        if self.active_serial.get() != Some(serial) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if self.scheduled.get() == Some(serial) {
            return Ok(());
        }
        self.scheduled.set(Some(serial));
        let mtm = view.mtm();
        let weak_view = MainThreadBound::new(objc2::rc::Weak::from(view), mtm);
        let weak_surface = MainThreadBound::new(Rc::downgrade(self), mtm);
        crate::native_surface::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("CPU延续只在原AppKit主队列执行");
            let Some(surface) = weak_surface.get(mtm).upgrade() else {
                return;
            };
            let Some(view) = weak_view.get(mtm).load() else {
                return;
            };
            if surface.scheduled.get() != Some(serial)
                || surface.active_serial.get() != Some(serial)
            {
                return; // 旧排队事件没有清理后继scheduled槽或领取新scene的权利。
            }
            surface.scheduled.set(None);
            let _use = PayloadUseGuard::enter(&view.ivars().depth);
            let painting = match PaintUse::enter(&surface.painting) {
                Ok(painting) => painting,
                Err(_) => {
                    surface.deferred.set(Some(serial));
                    return; // 原同步绘图栈退出时唯一wake，不能在嵌套runloop里空转调度。
                }
            };
            let result = (|| {
                let candidate = surface
                    .candidate
                    .borrow_mut()
                    .take()
                    .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
                if candidate.serial != serial {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                surface.advance(&view, candidate)
            })();
            drop(painting);
            let wake = surface.wake_deferred(&view);
            let result = match result {
                Err(error) => Err(error),
                Ok(()) => wake,
            };
            view.record(result); // 每轮真实错误仅记录一次；没有定时器、嵌套runloop或失败重放。
        });
        Ok(())
    }
    pub(super) fn wake_deferred(self: &Rc<Self>, view: &NativeMarkdownPagedTextView) -> Result<()> {
        let continuation = if let Some(serial) = self.deferred.take()
            && self.active_serial.get() == Some(serial)
        {
            self.schedule(view, serial)
        } else {
            Ok(())
        };
        // 原同步栈已经归还 PaintUse，才能让 AppKit 重新交付 updateLayer。
        // 此处只标脏当前真实客户，不领取 Scene，也不把旧票重新签给已接受的新输入。
        let repaint = if self.repaint_requested.replace(false) && !view.ivars().closed.get() {
            (|| {
                let origin = view.origin()?;
                self.layer_current(view, &|| origin.current(view))?;
                origin.step(view, || view.setNeedsDisplay(true))
            })()
        } else {
            Ok(())
        };
        // 两项都执行，保证原 serial 的调度错误不会丢掉较新 Scene 的重绘请求。
        // 原 continuation 首错仍优先，调用者再保留原 paint/Source 的更早失败。
        continuation.and(repaint)
    }
}
