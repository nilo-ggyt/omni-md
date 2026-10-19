//! 同步原生回调的载荷使用租约；尚在栈内的业务副本不得与 accepted release 重叠。

use std::cell::Cell;

/// 不持有业务数据或原生对象；只为调用者所属的主线程 Cell 维护平衡计数。
pub(crate) struct PayloadUseGuard<'a>(&'a Cell<usize>);

impl<'a> PayloadUseGuard<'a> {
    /// 进入可能持有业务临时值并回调原生代码的同步区间。
    pub(crate) fn enter(depth: &'a Cell<usize>) -> Self {
        depth.set(
            depth
                .get()
                .checked_add(1)
                .expect("payload-use nesting overflow"),
        );
        Self(depth)
    }
}

impl Drop for PayloadUseGuard<'_> {
    fn drop(&mut self) {
        // 正常返回及 unwind 都结束同一个租约；不能留下允许假释放或永久 Busy 的计数。
        self.0.set(
            self.0
                .get()
                .checked_sub(1)
                .expect("payload-use nesting underflow"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::PayloadUseGuard;
    use std::cell::Cell;

    #[test]
    fn nested_native_callbacks_remain_busy_until_outer_payload_returns() {
        let depth = Cell::new(0);
        let outer = PayloadUseGuard::enter(&depth);
        {
            // 内层退出不能提前使仍持有业务副本的外层变成可接受关闭。
            let inner = PayloadUseGuard::enter(&depth);
            assert_eq!(depth.get(), 2);
            drop(inner);
            assert_eq!(depth.get(), 1);
        }
        drop(outer);
        assert_eq!(depth.get(), 0);
    }

    #[test]
    fn native_work_unwind_restores_only_its_owned_claim() {
        let depth = Cell::new(0);
        let outer = PayloadUseGuard::enter(&depth);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inner = PayloadUseGuard::enter(&depth);
            panic!("synthetic payload callback failure");
        }));
        assert!(result.is_err());
        assert_eq!(depth.get(), 1);
        drop(outer);
        assert_eq!(depth.get(), 0);
    }
}
