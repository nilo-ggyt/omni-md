//! 结构操作的代次边界：首个可见变化立即撤权，所有rollback之后再封闭中途启动的扫描。

use super::{MarkdownSessionAccess, MarkdownWorkspaceError};
use std::cell::Cell;

/// 必须先于文件/rollback guard声明，因而最后drop；只持借用与标量，不执行IO或Native。
pub(super) struct WorkspaceStructurePublication<'session> {
    session: &'session MarkdownSessionAccess,
    invalidate_on_finish: Cell<bool>,
    rollback_unconfirmed: Cell<bool>,
}

impl<'session> WorkspaceStructurePublication<'session> {
    /// 预校验、碰撞和no-op都保持未激活；它们不声称文件系统已经变化。
    pub(super) fn new(session: &'session MarkdownSessionAccess) -> Self {
        Self {
            session,
            invalidate_on_finish: Cell::new(false),
            rollback_unconfirmed: Cell::new(false),
        }
    }

    /// 仅供取得文件的同一根约束校验，返回借用不持新文件或目录资源。
    pub(super) fn canonical_root(&self) -> &std::path::Path {
        &self.session.canonical_root
    }

    /// 在真实mkdir/open(O_EXCL)/rename/unlink成功后立即调用，任何后续?都不能漏撤。
    pub(super) fn changed(&self) {
        self.invalidate_on_finish.set(true);
        self.session.cancel_active_search();
    }

    /// std::remove_dir_all错误不报告进度；通过纯保护后接受删除尝试时保守撤权。
    /// 不声称失败无副作用时发生了mutation，也不为精确计数改写std防链接race的递归实现。
    pub(super) fn recursive_delete_attempt(&self) {
        self.invalidate_on_finish.set(true);
        self.session.cancel_active_search();
    }

    /// 不能证明当前文件仍属于本次完整写入时保留它；只记录失败，不在Drop里丢首因。
    pub(super) fn rollback_unconfirmed(&self) {
        self.rollback_unconfirmed.set(true);
        self.changed();
    }

    /// 所有文件guard必须先在私有操作闭包中析构，才能把清理状态并入原始Result。
    pub(super) fn finish<T>(
        &self,
        result: Result<T, MarkdownWorkspaceError>,
    ) -> Result<T, MarkdownWorkspaceError> {
        if self.rollback_unconfirmed.get() {
            // 仅不确定清理的失败路径分配Box；已发生的主错误保留为可检索source。
            let cause = result
                .err()
                .unwrap_or(MarkdownWorkspaceError::StorageUnavailable);
            Err(MarkdownWorkspaceError::RollbackUnconfirmed {
                cause: Box::new(cause),
            })
        } else {
            // 正常成功和已确认的回滚不改变原有返回类别。
            result
        }
    }
}

impl Drop for WorkspaceStructurePublication<'_> {
    fn drop(&mut self) {
        if self.invalidate_on_finish.get() {
            // 操作中途启动的worker可能观察过部分文件或即将rollback的名字，终态再撤一次。
            // 原文件guard已先drop，成功/失败清理都不能恢复旧authority。
            self.session.cancel_active_search();
        }
    }
}

#[cfg(test)]
mod tests;
