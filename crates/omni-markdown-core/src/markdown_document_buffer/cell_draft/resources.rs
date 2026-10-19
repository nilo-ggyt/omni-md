//! 固定草稿作用域只观察真实分支与读取payload退场，不保活文档、树、段或映射。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use super::{MarkdownCellDraftError, MarkdownDocumentBufferError};
use crate::MarkdownOriginalBackingObserver;

pub(super) const MAX_READ_ROOTS: usize = 2;

pub(in crate::markdown_document_buffer) struct Scope {
    pub(super) cancelled: AtomicBool,
    pub(super) serial: AtomicU64,
    pub(super) content_serial: AtomicU64,
    branch_live: AtomicBool,
    read_roots: AtomicUsize,
    peak_read_roots: AtomicUsize,
    delta_bytes: AtomicUsize,
    peak_delta_bytes: AtomicUsize,
    first_source_errno: AtomicI32,
    // 仅本raw分支新建的实际映射票；空堆编辑不分配Vec库存，继承canonical映射不登记。
    raw_backings: Mutex<Vec<MarkdownOriginalBackingObserver>>,
    first_raw_cleanup_errno: AtomicI32,
    first_candidate_cleanup_errno: AtomicI32,
    candidate_cleanup_failed: AtomicBool,
    prepared_commit_live: AtomicBool,
    prepared_commit_bytes: AtomicUsize,
    peak_prepared_commit_bytes: AtomicUsize,
}

impl Scope {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            serial: AtomicU64::new(0),
            content_serial: AtomicU64::new(0),
            branch_live: AtomicBool::new(true),
            read_roots: AtomicUsize::new(0),
            peak_read_roots: AtomicUsize::new(0),
            delta_bytes: AtomicUsize::new(0),
            peak_delta_bytes: AtomicUsize::new(0),
            first_source_errno: AtomicI32::new(0),
            raw_backings: Mutex::new(Vec::new()),
            first_raw_cleanup_errno: AtomicI32::new(0),
            first_candidate_cleanup_errno: AtomicI32::new(0),
            candidate_cleanup_failed: AtomicBool::new(false),
            prepared_commit_live: AtomicBool::new(false),
            prepared_commit_bytes: AtomicUsize::new(0),
            peak_prepared_commit_bytes: AtomicUsize::new(0),
        })
    }

    pub(super) fn record_source(&self, error: MarkdownDocumentBufferError) {
        if let MarkdownDocumentBufferError::OriginalBackingUnavailable(errno) = error {
            // 保存真实首errno；取消、过期或后继成功不能把原I/O失败洗成健康票。
            let _ = self.first_source_errno.compare_exchange(
                0,
                errno,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    fn require_live(&self) -> Result<(), MarkdownCellDraftError> {
        let errno = self.first_source_errno.load(Ordering::Acquire);
        if errno != 0 {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        if self.cancelled.load(Ordering::Acquire) {
            // 取消后的不可变字节仍可由旧owner持有，但不再提供输入或布局权限。
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(())
    }

    pub(super) fn require(&self, serial: u64) -> Result<(), MarkdownCellDraftError> {
        self.require_live()?;
        if self.serial.load(Ordering::Acquire) != serial {
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(())
    }

    pub(super) fn require_content(&self, serial: u64) -> Result<(), MarkdownCellDraftError> {
        self.require_live()?;
        if self.content_serial.load(Ordering::Acquire) != serial {
            // 只撤销旧正文；选区或unmark不会给同一root制造新的布局域。
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(())
    }

    pub(super) fn reserve_reader(self: &Arc<Self>) -> Result<ReadClaim, MarkdownCellDraftError> {
        let mut count = self.read_roots.load(Ordering::Acquire);
        loop {
            if count >= MAX_READ_ROOTS {
                // 两个实际payload是硬上限；不以追加Vec、丢旧owner或提前释放计数腾位。
                return Err(MarkdownCellDraftError::ReadersBusy);
            }
            match self.read_roots.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.peak_read_roots.fetch_max(count + 1, Ordering::AcqRel);
                    return Ok(ReadClaim(Arc::clone(self)));
                }
                Err(actual) => count = actual,
            }
        }
    }

    pub(super) fn has_readers(&self) -> bool {
        self.read_roots.load(Ordering::Acquire) != 0
    }

    pub(super) fn has_retired_owners(&self) -> bool {
        self.has_readers()
            || self.prepared_commit_live.load(Ordering::Acquire)
            || self.candidate_cleanup_failed.load(Ordering::Acquire)
            || !self.raw_backings_released()
    }

    pub(super) fn reserve_commit(self: &Arc<Self>) -> Result<CommitClaim, MarkdownCellDraftError> {
        self.require_live()?;
        if self
            .prepared_commit_live
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // 先占唯一名额再创建有限String或匿名输出；不能反复捕获相同serial制造N份准入副本。
            return Err(MarkdownCellDraftError::CommitBusy);
        }
        Ok(CommitClaim {
            scope: Arc::clone(self),
            backing: None,
        })
    }

    /// 文件首次交给raw段表前登记真实票；不持有映射强边，也不把继承的canonical段重复记账。
    pub(super) fn observe_raw_backing(&self, backing: MarkdownOriginalBackingObserver) {
        let mut backings = self.raw_backings.lock().unwrap();
        backings.retain(|previous| {
            // 已回退且真实unmap完成的旧候选可退出清单；实际错误仍永久留在原Scope首因中。
            self.record_raw_cleanup(previous);
            !previous.unmap_succeeded() || previous.mapped_bytes() != 0
        });
        backings.push(backing);
    }

    /// 只观察真实系统操作事实；raw候选清理错误独立于未改变的原Source权限。
    fn record_raw_cleanup(&self, backing: &MarkdownOriginalBackingObserver) {
        if let Some(errno) = backing.first_errno() {
            let _ = self.first_raw_cleanup_errno.compare_exchange(
                0,
                errno,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    /// 仅资源退场查询遍历实际票；正文健康/布局读取不锁清单，也不扫描任何映射。
    fn raw_backings_released(&self) -> bool {
        let backings = self.raw_backings.lock().unwrap();
        let mut released = true;
        for backing in backings.iter() {
            // 即使前项失败也读完其余实际票，不能用短路掩盖另一个文件的真实首errno。
            self.record_raw_cleanup(backing);
            released &=
                backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed();
        }
        released && self.first_raw_cleanup_errno.load(Ordering::Acquire) == 0
    }

    /// 分支与最后Reader都已结束后才核raw映射；不能在仍有合法owner时伪报泄漏errno。
    fn finish_raw_backings(&self) {
        if !self.branch_live.load(Ordering::Acquire) && !self.has_readers() {
            self.raw_backings_released();
        }
    }

    pub(super) fn publish_delta(&self, bytes: usize) {
        self.delta_bytes.store(bytes, Ordering::Release);
        self.peak_delta_bytes.fetch_max(bytes, Ordering::AcqRel);
    }
}

pub(super) struct BranchClaim(pub(super) Arc<Scope>);
impl Drop for BranchClaim {
    fn drop(&mut self) {
        // 此字段必须排在candidate root与独立段表之后；它们真实Drop后才撤分支事实。
        self.0.delta_bytes.store(0, Ordering::Release);
        self.0.branch_live.store(false, Ordering::Release);
        self.0.cancelled.store(true, Ordering::Release);
        self.0.finish_raw_backings();
    }
}

pub(in crate::markdown_document_buffer) struct ReadClaim(Arc<Scope>);
impl Drop for ReadClaim {
    fn drop(&mut self) {
        // FrozenText中它排在树、段与健康backing之后；内部snapshot clone也必须真实退场后才归还。
        self.0.read_roots.fetch_sub(1, Ordering::AcqRel);
        self.0.finish_raw_backings();
    }
}

pub(super) struct CommitClaim {
    scope: Arc<Scope>,
    backing: Option<MarkdownOriginalBackingObserver>,
}

impl CommitClaim {
    pub(super) fn allocated(&self, bytes: usize) {
        // 只记录准入实际保留的堆正文capacity；文件extent与原树共享字节不是堆分配。
        self.scope
            .prepared_commit_bytes
            .store(bytes, Ordering::Release);
        self.scope
            .peak_prepared_commit_bytes
            .fetch_max(bytes, Ordering::AcqRel);
    }

    /// seal得到实际映射后立即绑定；候选读取/缩回Inline失败也不能绕过最终释放核验。
    pub(super) fn observe_backing(&mut self, backing: MarkdownOriginalBackingObserver) {
        self.backing = Some(backing);
    }

    /// 仅供同一映射已被canonical成功publish接管，或临时Inline映射已实证释放后移出票。
    pub(super) fn release_backing_ticket(&mut self) {
        self.backing = None;
    }
}

impl Drop for CommitClaim {
    fn drop(&mut self) {
        // PreparedCommit中本字段排在实际候选存储之后；String或映射owner退场后才归还唯一名额。
        if let Some(backing) = &self.backing {
            if let Some(errno) = backing.first_errno() {
                let _ = self.scope.first_candidate_cleanup_errno.compare_exchange(
                    0,
                    errno,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            if !backing.unmap_succeeded() || backing.mapped_bytes() != 0 || backing.failed() {
                // replacement字段已先Drop；保留实际负面事实，不能把归还准入名额当作munmap成功。
                self.scope
                    .candidate_cleanup_failed
                    .store(true, Ordering::Release);
            }
        }
        self.scope.prepared_commit_bytes.store(0, Ordering::Release);
        self.scope
            .prepared_commit_live
            .store(false, Ordering::Release);
    }
}

#[derive(Clone)]
/// 实际owner与映射清理收据；不保活草稿、原文、文档或文件，计数不是系统内存采样。
pub struct MarkdownCellDraftResourceObserver {
    pub(super) scope: Arc<Scope>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 分支delta、实际读取payload和唯一准入String容量；clone共享payload，不增加read_roots。
pub struct MarkdownCellDraftResourceSnapshot {
    pub branch_live: bool,
    pub read_roots: usize,
    pub peak_read_roots: usize,
    pub delta_bytes: usize,
    pub peak_delta_bytes: usize,
    pub cancelled: bool,
    pub first_source_errno: Option<i32>,
    pub first_raw_cleanup_errno: Option<i32>,
    pub first_candidate_cleanup_errno: Option<i32>,
    pub prepared_commit_live: bool,
    pub prepared_commit_bytes: usize,
    pub peak_prepared_commit_bytes: usize,
}

impl MarkdownCellDraftResourceObserver {
    #[must_use]
    /// 各项是原子观察值；释放结论要求真实分支和全部读取payload都已归还。
    pub fn snapshot(&self) -> MarkdownCellDraftResourceSnapshot {
        let errno = self.scope.first_source_errno.load(Ordering::Acquire);
        let raw_errno = self.scope.first_raw_cleanup_errno.load(Ordering::Acquire);
        let candidate_errno = self
            .scope
            .first_candidate_cleanup_errno
            .load(Ordering::Acquire);
        MarkdownCellDraftResourceSnapshot {
            branch_live: self.scope.branch_live.load(Ordering::Acquire),
            read_roots: self.scope.read_roots.load(Ordering::Acquire),
            peak_read_roots: self.scope.peak_read_roots.load(Ordering::Acquire),
            delta_bytes: self.scope.delta_bytes.load(Ordering::Acquire),
            peak_delta_bytes: self.scope.peak_delta_bytes.load(Ordering::Acquire),
            cancelled: self.scope.cancelled.load(Ordering::Acquire),
            first_source_errno: (errno != 0).then_some(errno),
            first_raw_cleanup_errno: (raw_errno != 0).then_some(raw_errno),
            first_candidate_cleanup_errno: (candidate_errno != 0).then_some(candidate_errno),
            prepared_commit_live: self.scope.prepared_commit_live.load(Ordering::Acquire),
            prepared_commit_bytes: self.scope.prepared_commit_bytes.load(Ordering::Acquire),
            peak_prepared_commit_bytes: self
                .scope
                .peak_prepared_commit_bytes
                .load(Ordering::Acquire),
        }
    }

    #[must_use]
    /// 树、Reader及准入owner退场后，还要求raw新文件与未转归候选真实清理成功。
    /// 继承的canonical Source及成功发布的Frozen由Doc实际backing票承担最终munmap义务。
    pub fn is_released(&self) -> bool {
        let state = self.snapshot();
        !state.branch_live
            && state.read_roots == 0
            && !state.prepared_commit_live
            && state.prepared_commit_bytes == 0
            && !self.scope.candidate_cleanup_failed.load(Ordering::Acquire)
            && self.scope.raw_backings_released()
    }
}

/// 弱nonce不保候选分支；丢弃编辑票只标取消，Doc仍须显式排空唯一slot。
pub struct MarkdownCellDraft {
    pub(super) scope: Weak<Scope>,
}
impl Drop for MarkdownCellDraft {
    fn drop(&mut self) {
        if let Some(scope) = self.scope.upgrade() {
            scope.cancelled.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests;
