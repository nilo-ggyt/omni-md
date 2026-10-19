//! 原请求终点的一次性纯 Rust 观察；不拥有正文、Native 对象或后继请求。

use std::cell::{Cell, RefCell};

#[derive(Default)]
pub(in crate::native_surface) struct ProbeTerminalObserver {
    registered: Cell<bool>,
    pending: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl ProbeTerminalObserver {
    /// 注册前已完成时直接交还排队权限；Pending 只保存一次通知，不能据时间自行重试。
    pub(in crate::native_surface) fn observe(
        &self,
        completed: Option<bool>,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        if self.registered.replace(true) {
            return Err("原请求终点不能重复注册观察".to_owned());
        }
        if completed.is_some() {
            // true 和 false 都是真实终点；实际功能谓词仍由后继验证完整判定。
            return Ok(true);
        }
        *self.pending.borrow_mut() = Some(Box::new(observer));
        Ok(false)
    }

    /// 仅由原 completed 写入点调用；先释放观察槽借用，随后回调才可投递 Host 原生事件。
    pub(in crate::native_surface) fn notify(&self) {
        let observer = self.pending.borrow_mut().take();
        if let Some(observer) = observer {
            observer(); // 重入通知只见空槽，不能重复派发或保留回调拥有者。
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn pending_waits_for_one_terminal_and_releases_borrow_before_callback() {
        let observer = Rc::new(ProbeTerminalObserver::default());
        let calls = Rc::new(Cell::new(0));
        let callback_observer = Rc::clone(&observer);
        let callback_calls = Rc::clone(&calls);
        assert!(
            !observer
                .observe(None, move || {
                    assert!(callback_observer.pending.try_borrow_mut().is_ok());
                    callback_calls.set(callback_calls.get() + 1);
                    callback_observer.notify(); // 原终点重入也不能再次交付。
                })
                .unwrap()
        );
        assert_eq!(calls.get(), 0); // 注册自身没有推进验证或制造成功收据。
        assert!(observer.observe(None, || {}).is_err());
        observer.notify();
        observer.notify();
        assert_eq!(calls.get(), 1);
        assert!(observer.pending.borrow().is_none());
    }

    #[test]
    fn completed_before_registration_includes_real_failure_without_replay() {
        for completed in [false, true] {
            let observer = ProbeTerminalObserver::default();
            let calls = Rc::new(Cell::new(0));
            let callback_calls = Rc::clone(&calls);
            assert!(
                observer
                    .observe(Some(completed), move || callback_calls.set(1))
                    .unwrap()
            );
            observer.notify();
            assert_eq!(calls.get(), 0); // 同步 Ready 由调用方排一次，不同时再执行旧闭包。
            assert!(observer.observe(Some(completed), || {}).is_err());
        }
    }
}
