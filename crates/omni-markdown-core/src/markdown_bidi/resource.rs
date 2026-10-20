//! UBA匿名文件和固定页的实际归还收据；观察者只持原子事实，不持正文、文件或缓存。

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering},
};

pub(super) struct ResourceState {
    file_created: AtomicBool,
    // 可见段最多保留前一段和当前段；每个真实 File acquire/close 都计入同一 scope。
    open_files: AtomicUsize,
    // 0为原FD仍未报告close；1只来自真实close成功；负值保留真实失败errno。
    close_status: AtomicI32,
    cache_bytes: AtomicUsize,
    peak_cache_bytes: AtomicUsize,
    page_loads: AtomicU64,
}

impl ResourceState {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            file_created: AtomicBool::new(false),
            open_files: AtomicUsize::new(0),
            close_status: AtomicI32::new(0),
            cache_bytes: AtomicUsize::new(0),
            peak_cache_bytes: AtomicUsize::new(0),
            page_loads: AtomicU64::new(0),
        })
    }

    pub(super) fn file_acquired(&self) {
        self.open_files.fetch_add(1, Ordering::AcqRel);
        self.file_created.store(true, Ordering::Release);
    }

    pub(super) fn observe(self: &Arc<Self>) -> MarkdownBidiResourceObserver {
        MarkdownBidiResourceObserver(Some(Arc::clone(self)))
    }

    pub(super) fn close_returned(&self, status: i32) {
        if status == 1 {
            self.open_files.fetch_sub(1, Ordering::AcqRel);
            // 后一文件的成功 close 不能覆盖前一文件的真实失败 errno。
            let _ = self
                .close_status
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        } else {
            let _ = self
                .close_status
                .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                    (old >= 0).then_some(status)
                });
        }
    }

    pub(super) fn page_allocated(&self, bytes: usize) {
        let live = self.cache_bytes.fetch_add(bytes, Ordering::AcqRel) + bytes;
        self.peak_cache_bytes.fetch_max(live, Ordering::AcqRel);
    }

    pub(super) fn page_deallocated(&self, bytes: usize) {
        self.cache_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }

    /// 每次成功的真实页读立即累计，Pending 子段或随后取消都不能把这次 IO 洗掉。
    pub(super) fn page_loaded(&self) {
        self.page_loads.fetch_add(1, Ordering::AcqRel);
    }

    /// 同一 scope 的两个共活子段与已经退役子段共享累计实际页读数。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn page_loads(&self) -> u64 {
        self.page_loads.load(Ordering::Acquire)
    }
}

/// Uniform证书没有创建文件或页；Paged收据与原真实File和Box归还事件共享固定原子状态。
#[derive(Clone)]
pub struct MarkdownBidiResourceObserver(Option<Arc<ResourceState>>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownBidiResourceSnapshot {
    pub file_created: bool,
    pub file_open: bool,
    pub close_succeeded: bool,
    pub close_errno: Option<i32>,
    pub cache_bytes: usize,
    pub peak_cache_bytes: usize,
}

impl MarkdownBidiResourceObserver {
    pub(super) fn unallocated() -> Self {
        Self(None)
    }

    #[must_use]
    pub fn snapshot(&self) -> MarkdownBidiResourceSnapshot {
        let Some(state) = &self.0 else {
            return MarkdownBidiResourceSnapshot {
                file_created: false,
                file_open: false,
                close_succeeded: false,
                close_errno: None,
                cache_bytes: 0,
                peak_cache_bytes: 0,
            };
        }; // 从未分配不冒称一次close成功，调用者可明确区分两种资源路径。
        let created = state.file_created.load(Ordering::Acquire);
        let status = state.close_status.load(Ordering::Acquire);
        let open_files = state.open_files.load(Ordering::Acquire);
        MarkdownBidiResourceSnapshot {
            file_created: created,
            file_open: open_files != 0,
            close_succeeded: status == 1 && open_files == 0,
            close_errno: (status < 0).then(|| -status),
            cache_bytes: state.cache_bytes.load(Ordering::Acquire),
            peak_cache_bytes: state.peak_cache_bytes.load(Ordering::Acquire),
        }
    }

    #[must_use]
    pub fn is_released(&self) -> bool {
        let actual = self.snapshot();
        !actual.file_open && actual.cache_bytes == 0 && actual.close_errno.is_none()
    }

    #[cfg(test)]
    pub(super) fn closed(&self) -> bool {
        self.snapshot().close_succeeded
    }
}
