//! canonical匿名快照的有限只读借用；完整虚拟地址不授予完整正文slice权限。

use std::fs::File;
use std::ops::{Deref, Range};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// base映射错误不依赖可选Doc/Unicode功能；默认Vault同样能真实拒绝I/O失败。
#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum OriginalBackingError {
    #[error("原始映射读取范围无效")]
    RangeOutsideMapping,
    #[error("原始映射不可用（系统错误 {0}）")]
    Unavailable(i32),
    #[error("同时读取的原始窗口过多")]
    LeaseLimitExceeded,
}

pub(crate) const ORIGINAL_READ_BYTES: usize = 4096;
// 并发及递归借用也必须有真实驻留上界；不持锁调用业务回调，避免同线程递归死锁。
const MAX_ACTIVE_LEASES: usize = 32;

mod window;
use window::{LeasePins, MANAGED_WINDOW_BYTES, WindowState};

#[derive(Default)]
struct BackingFacts {
    mapped_bytes: AtomicUsize,
    active_leases: AtomicUsize,
    poison: AtomicI32,
    managed_bytes: AtomicUsize,
    peak_managed_bytes: AtomicUsize,
    reclaim_attempts: AtomicUsize,
    reclaim_successes: AtomicUsize,
    unmap_attempted: AtomicBool,
    unmap_succeeded: AtomicBool,
    #[cfg(any(test, feature = "original-backing-reclaim-probe"))]
    fail_next_reclaim: AtomicBool,
}

#[derive(Clone)]
/// 仅观察实际VM清理的固定标量，不保活映射、文件、正文、文档或历史。
pub struct MarkdownOriginalBackingObserver {
    facts: Arc<BackingFacts>,
}

impl MarkdownOriginalBackingObserver {
    #[must_use]
    /// 只有最后映射owner真实执行munmap且系统返回成功才为真；普通Drop到达不等同成功。
    pub fn unmap_succeeded(&self) -> bool {
        self.facts.unmap_succeeded.load(Ordering::Acquire)
    }

    #[must_use]
    /// 返回仍由本owner负责的映射extent；失败时保留负面事实，不伪装成零。
    pub fn mapped_bytes(&self) -> usize {
        self.facts.mapped_bytes.load(Ordering::Acquire)
    }

    #[must_use]
    /// 必要页驱逐或最终unmap失败均永久记录为负面事实，归还pin不冒充物理回收。
    pub fn failed(&self) -> bool {
        self.facts.poison.load(Ordering::Acquire) != 0
    }

    /// 本owner当前管理的系统页预算；不是内核RSS或整产品物理内存证明。
    #[must_use]
    pub fn managed_resident_bytes(&self) -> usize {
        self.facts.managed_bytes.load(Ordering::Acquire)
    }
    /// 真实准入过的槽峰值，始终不超过64KiB；实际RSS另由进程控制验证。
    #[must_use]
    pub fn peak_managed_resident_bytes(&self) -> usize {
        self.facts.peak_managed_bytes.load(Ordering::Acquire)
    }
    /// 实际进入msync系统调用的次数，不把lease结束计成物理回收。
    #[must_use]
    pub fn reclaim_attempts(&self) -> usize {
        self.facts.reclaim_attempts.load(Ordering::Acquire)
    }
    /// 只有真实msync返回零才增加；故障注入后的永久错误另由first_errno记录。
    #[must_use]
    pub fn reclaim_successes(&self) -> usize {
        self.facts.reclaim_successes.load(Ordering::Acquire)
    }
    /// 请求在下一次真实`MS_SYNC | MS_INVALIDATE`成功后，注入一次合成`EIO`。
    ///
    /// 仅测试或显式诊断feature提供此入口；这不是内核返回的错误，也不会篡改线程errno。
    /// 设置标志本身不驱逐页、不撤销活lease或关闭FD；缓存命中不会消费请求。原owner只有
    /// 在实际msync返回成功后才消费一次并永久记失败，真实内核错误仍保留原先首因。
    /// 此观察者仅持固定原子事实，不保活Source、映射或文件；owner已释放时标志没有后继作用。
    /// 最终真实munmap成功与注入sticky失败分别观察，不能以其中一项替代另一项。
    #[cfg(any(test, feature = "original-backing-reclaim-probe"))]
    pub fn fail_next_reclaim_for_test(&self) {
        self.facts.fail_next_reclaim.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn first_errno(&self) -> Option<i32> {
        match self.facts.poison.load(Ordering::Acquire) {
            0 => None,
            error => Some(error),
        }
    }
}

/// 此类型没有Deref或whole-slice入口；只有持有效lease才能观察最多4KiB原字节。
pub(crate) struct OriginalBacking {
    address: NonNull<u8>,
    length: usize,
    page_size: usize,
    facts: Arc<BackingFacts>,
    mapped: bool,
    // 同一冻结inode的所有Doc/Snapshot共享此固定目录；锁不跨业务回调。
    windows: Mutex<WindowState>,
    window_slots: usize,
}

// SAFETY: inode已独立冻结且无写者；映射地址在最后owner析构前稳定，只提供不可变有限借用。
// msync不改变字节或取消地址；活lease借用owner，Rust阻止它与最后munmap并发。
unsafe impl Send for OriginalBacking {}
// SAFETY: 目录由Mutex串行化，观察事实为原子；只读字节不变，活pin槽不会被本owner驱逐。
unsafe impl Sync for OriginalBacking {}

impl OriginalBacking {
    /// 建立有界读权限所用的完整虚拟映射，不授予完整slice。
    ///
    /// # Safety
    /// file必须是factory已经验证长度的独立匿名inode，所有写者已结束且没有其他写/截断句柄。
    /// 此义务由immutable_snapshot工厂在冻结独立源副本或消费独占匿名输出时证明，不能传用户可变文件。
    pub(crate) unsafe fn map(file: File, length: usize) -> Result<Self, OriginalBackingError> {
        if length == 0 {
            // 空值不使用mmap；调用者应以None表达没有原文件资源。
            return Err(OriginalBackingError::RangeOutsideMapping);
        }
        // SAFETY: sysconf不借用或转移任何对象；仅查询系统页对齐所需的正整数。
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page_size = usize::try_from(page_size)
            .ok()
            .filter(|size| {
                size.is_power_of_two()
                    && (ORIGINAL_READ_BYTES..=MANAGED_WINDOW_BYTES).contains(size)
            })
            .ok_or(OriginalBackingError::Unavailable(libc::EINVAL))?;
        // SAFETY: file来自专用匿名冻结factory，extent已核验且无写者；只读共享映射由本类型独占VM析构。
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            // 保留真实errno；没有取得映射时不创建成功清理收据。
            return Err(OriginalBackingError::Unavailable(last_errno()));
        }
        let Some(address) = NonNull::new(raw.cast::<u8>()) else {
            // Rust slice不能使用零指针；即使系统给出零地址也必须补偿真实映射。
            // SAFETY: 此分支已经取得且尚未发布length字节的映射，没有任何活借用。
            if unsafe { libc::munmap(raw, length) } != 0 {
                // 补偿也失败时报告真实VM清理错误；不能仅返回地址错误而暗示映射已归还。
                return Err(OriginalBackingError::Unavailable(last_errno()));
            }
            return Err(OriginalBackingError::Unavailable(libc::EINVAL));
        };
        // 映射本身保留匿名inode；原只读descriptor在这里实际释放，不成为idle FD。
        let fd = file.into_raw_fd();
        // SAFETY: into_raw_fd转移唯一descriptor，close只调用一次；映射已有独立VM引用。
        if unsafe { libc::close(fd) } != 0 {
            let errno = last_errno();
            // SAFETY: 映射未发布、没有活lease，失败获取必须补偿已取得的VM资源。
            if unsafe { libc::munmap(address.as_ptr().cast(), length) } != 0 {
                // descriptor获取失败后的VM补偿仍是必要资源操作，其失败不能被最初close错误吞掉。
                return Err(OriginalBackingError::Unavailable(last_errno()));
            }
            return Err(OriginalBackingError::Unavailable(errno));
        }
        let facts = Arc::new(BackingFacts::default());
        facts.mapped_bytes.store(length, Ordering::Release);
        Ok(Self {
            address,
            length,
            page_size,
            facts,
            mapped: true,
            windows: Mutex::new(WindowState::default()),
            window_slots: MANAGED_WINDOW_BYTES / page_size,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.length
    }

    /// 原生文稿和真实释放测试借用同一原子观察，不延长文件或映射本身的寿命。
    #[cfg(any(feature = "native-markdown-candidate", test))]
    pub(crate) fn observer(&self) -> MarkdownOriginalBackingObserver {
        MarkdownOriginalBackingObserver {
            facts: Arc::clone(&self.facts),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_reclaim_for_test(&self) {
        self.observer().fail_next_reclaim_for_test(); // 旧单测沿同一个诊断位，不另设故障算法。
    }

    pub(crate) fn health(&self) -> Result<(), OriginalBackingError> {
        match self.facts.poison.load(Ordering::Acquire) {
            0 => Ok(()),
            // 首次失败永久撤销后继读取，不能靠新的metadata或cache命中重新签发权限。
            errno => Err(OriginalBackingError::Unavailable(errno)),
        }
    }

    fn fail(&self, errno: i32) -> OriginalBackingError {
        let errno = if errno == 0 { libc::EIO } else { errno };
        let _ = self
            .facts
            .poison
            .compare_exchange(0, errno, Ordering::AcqRel, Ordering::Acquire);
        OriginalBackingError::Unavailable(self.facts.poison.load(Ordering::Acquire))
    }

    fn lock_windows(&self) -> Result<MutexGuard<'_, WindowState>, OriginalBackingError> {
        self.windows.lock().map_err(|_| self.fail(libc::EIO))
    }

    fn record_managed(&self, windows: &WindowState) {
        let bytes = windows.managed_bytes(self.window_slots, self.page_size);
        self.facts.managed_bytes.store(bytes, Ordering::Release);
        self.facts
            .peak_managed_bytes
            .fetch_max(bytes, Ordering::AcqRel);
    }

    pub(crate) fn lease(
        &self,
        range: Range<usize>,
    ) -> Result<OriginalReadLease<'_>, OriginalBackingError> {
        self.health()?;
        if range.start > range.end || range.end > self.length || range.len() > ORIGINAL_READ_BYTES {
            return Err(OriginalBackingError::RangeOutsideMapping); // 任何字节借用前核完整原范围。
        }
        let pins = {
            let mut windows = self.lock_windows()?;
            self.health()?;
            if self.facts.active_leases.load(Ordering::Acquire) >= MAX_ACTIVE_LEASES {
                return Err(OriginalBackingError::LeaseLimitExceeded);
            }
            let mut pages = [0; 2];
            let count = if range.is_empty() {
                0 // 空读取仍有活lease与健康门，但不取得正文页。
            } else {
                pages[0] = range.start / self.page_size * self.page_size;
                pages[1] = (range.end - 1) / self.page_size * self.page_size;
                if pages[0] == pages[1] { 1 } else { 2 }
            };
            let pins = windows
                .plan(&pages[..count], self.window_slots)
                .ok_or(OriginalBackingError::LeaseLimitExceeded)?;
            // 计划完整成功才驱逐；所有候选均未pin且不属于本请求，失败不会扩大预算或等待其他lease。
            for at in 0..pins.count {
                let id = pins.slots[at];
                if windows.slots[id].page != Some(pins.pages[at])
                    && let Some(old) = windows.slots[id].page
                {
                    self.reclaim_page(old)?;
                    windows.slots[id].page = None;
                    self.record_managed(&windows);
                }
            }
            windows.pin(pins);
            self.record_managed(&windows);
            self.facts.active_leases.fetch_add(1, Ordering::AcqRel);
            pins
        };
        // SAFETY: 原完整范围在固定匿名映射内，活pin与owner借用覆盖返回slice；不向外给whole映射。
        let bytes = unsafe {
            std::slice::from_raw_parts(self.address.as_ptr().add(range.start), range.len())
        };
        let lease = OriginalReadLease {
            owner: self,
            bytes,
            pins,
            finished: false,
        };
        // 其他线程刚发生的错误不能因已取得缓存槽而被忽略；失败时局部lease真实归还pin。
        self.health()?;
        Ok(lease)
    }

    pub(crate) fn with_bytes<R>(
        &self,
        range: Range<usize>,
        visit: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, OriginalBackingError> {
        let lease = self.lease(range)?;
        let result = visit(&lease); // 无目录锁跨业务回调，允许有限递归和并发读取。
        lease.finish()?; // 先归还本次pin并核真实必要驱逐错误，再允许发布业务值。
        Ok(result)
    }

    fn reclaim_page(&self, start: usize) -> Result<(), OriginalBackingError> {
        let length = self.page_size.min(self.length - start);
        bump(&self.facts.reclaim_attempts);
        // SAFETY: 起点是本映射中完整系统页，尾页length不超过真实extent；只有未pin页或独占最终清理调用。
        let result = unsafe {
            libc::msync(
                self.address.as_ptr().add(start).cast(),
                length,
                libc::MS_SYNC | libc::MS_INVALIDATE,
            )
        };
        if result != 0 {
            return Err(self.fail(last_errno()));
        }
        bump(&self.facts.reclaim_successes);
        #[cfg(any(test, feature = "original-backing-reclaim-probe"))]
        if self.facts.fail_next_reclaim.swap(false, Ordering::AcqRel) {
            return Err(self.fail(libc::EIO)); // 只在真实驱逐msync成功后注入，不能伪造物理操作。
        }
        Ok(())
    }

    fn unpin(&self, pins: LeasePins) -> Result<(), OriginalBackingError> {
        let mut windows = match self.windows.lock() {
            Ok(windows) => windows,
            Err(poisoned) => {
                self.fail(libc::EIO);
                poisoned.into_inner() // 仅为归还原claim取目录，绝不恢复读取或清除poison。
            }
        };
        for at in 0..pins.count {
            let slot = &mut windows.slots[pins.slots[at]];
            if slot.page == Some(pins.pages[at]) && slot.pins != 0 {
                slot.pins -= 1;
            } else {
                self.fail(libc::EINVAL); // 内部claim不配平不能继续签发成功，但仍补偿其他原pin。
            }
        }
        if self
            .facts
            .active_leases
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .is_err()
        {
            self.fail(libc::EINVAL);
        }
        self.health()
    }

    /// 仅文稿功能的测试显式驱逐未pin页，以验证真实回收失败和后继读取的首错。
    #[cfg(all(test, feature = "native-markdown-candidate"))]
    pub(crate) fn reclaim_unpinned_for_test(&self) -> Result<(), OriginalBackingError> {
        let mut windows = self.lock_windows()?;
        self.health()?;
        for at in 0..self.window_slots {
            if windows.slots[at].pins == 0
                && let Some(page) = windows.slots[at].page
            {
                self.reclaim_page(page)?;
                windows.slots[at].page = None;
                self.record_managed(&windows);
            }
        }
        self.record_managed(&windows);
        self.health()
    }

    /// 独占消费者最终归还驻留槽和整个VM；共享Doc/Frozen只由最后Arc走同一路径。
    pub(crate) fn close(mut self) -> Result<(), OriginalBackingError> {
        self.release()
    }

    fn release(&mut self) -> Result<(), OriginalBackingError> {
        if !self.mapped {
            return self.health();
        } // 旧地址的VM释放只尝试一次，不对复用地址重试。
        if self.facts.active_leases.load(Ordering::Acquire) != 0 {
            self.fail(libc::EBUSY);
        }
        {
            let mut windows = match self.windows.lock() {
                Ok(windows) => windows,
                Err(poisoned) => {
                    self.fail(libc::EIO);
                    poisoned.into_inner()
                }
            };
            for at in 0..self.window_slots {
                // 独占最终清理即使源已poison也继续真实尝试全部槽；首因不被后续成功重写。
                if let Some(page) = windows.slots[at].page
                    && self.reclaim_page(page).is_ok()
                {
                    windows.slots[at].page = None;
                }
            }
            self.record_managed(&windows);
        }
        self.mapped = false;
        self.facts.unmap_attempted.store(true, Ordering::Release);
        // SAFETY: &mut self/最后owner确保没有活Rust借用，mapped标记禁止对原地址再次munmap。
        let result = unsafe { libc::munmap(self.address.as_ptr().cast(), self.length) };
        if result == 0 {
            self.facts.mapped_bytes.store(0, Ordering::Release);
            self.facts.managed_bytes.store(0, Ordering::Release);
            self.facts.unmap_succeeded.store(true, Ordering::Release);
            self.health()
        } else {
            Err(self.fail(last_errno()))
        }
    }
}

/// 原slice只由活lease授权；结束pin不销毁地址，也不保留活动文档或独立正文副本。
pub(crate) struct OriginalReadLease<'a> {
    owner: &'a OriginalBacking,
    bytes: &'a [u8],
    pins: LeasePins,
    finished: bool,
}
impl Deref for OriginalReadLease<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.bytes
    }
}
impl OriginalReadLease<'_> {
    pub(crate) fn finish(mut self) -> Result<(), OriginalBackingError> {
        self.finished = true; // 原claim只归还一次；失败也不能由Drop重复扣减。
        self.owner.unpin(self.pins)
    }
}
impl Drop for OriginalReadLease<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.owner.unpin(self.pins);
        }
    }
}

fn bump(value: &AtomicUsize) {
    let _ = value.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_add(1))
    });
}

impl Drop for OriginalBacking {
    fn drop(&mut self) {
        // Drop补偿不能返回错误；只由固定observer保存真实系统结果，绝不把到达Drop当作释放成功。
        let _ = self.release();
    }
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(bytes: &[u8]) -> OriginalBacking {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        // SAFETY: 此独占匿名测试inode在本调用后没有写者或外部句柄，extent与初始内容完全匹配。
        unsafe { OriginalBacking::map(file, bytes.len()) }.unwrap()
    }

    #[test]
    fn actual_finite_lease_close_and_observer_release() {
        let source = vec![0x71; ORIGINAL_READ_BYTES * 3 + 1];
        let backing = fixture(&source);
        let observer = backing.observer();
        assert_eq!(observer.mapped_bytes(), source.len());
        for start in (0..source.len()).step_by(ORIGINAL_READ_BYTES) {
            let end = (start + ORIGINAL_READ_BYTES).min(source.len());
            backing
                .with_bytes(start..end, |bytes| assert_eq!(bytes, &source[start..end]))
                .unwrap();
        }
        assert_eq!(backing.facts.active_leases.load(Ordering::Acquire), 0);
        assert!(!observer.unmap_succeeded());
        backing.close().unwrap();
        assert!(observer.unmap_succeeded());
        assert_eq!(observer.mapped_bytes(), 0);
        assert!(!observer.failed());
    }

    #[test]
    fn callback_failure_never_masks_reclaim_failure_or_restores_health() {
        let backing = fixture(&vec![b'x'; MANAGED_WINDOW_BYTES * 2]);
        let observer = backing.observer();
        // 先装满真实驻留目录；回调中的新页请求才会进入真实驱逐msync。
        for at in 0..backing.window_slots {
            backing
                .with_bytes(at * backing.page_size..at * backing.page_size + 1, |_| ())
                .unwrap();
        }
        backing.fail_next_reclaim_for_test();
        let held = if backing.window_slots > 1 { 0..1 } else { 0..0 };
        let result = backing.with_bytes(held, |_| {
            assert_eq!(
                backing.with_bytes(MANAGED_WINDOW_BYTES..MANAGED_WINDOW_BYTES + 1, |_| ()),
                Err(OriginalBackingError::Unavailable(libc::EIO))
            );
            Err::<(), _>("callback stopped")
        });
        assert_eq!(result, Err(OriginalBackingError::Unavailable(libc::EIO)));
        assert_eq!(
            backing.with_bytes(0..1, |_| ()),
            Err(OriginalBackingError::Unavailable(libc::EIO))
        );
        assert_eq!(
            backing.close(),
            Err(OriginalBackingError::Unavailable(libc::EIO))
        );
        // 实际VM已释放和本次操作已经失败是两个事实，不能因close成功改写旧错误。
        assert!(observer.unmap_succeeded());
        assert!(observer.failed());
        assert_eq!(observer.mapped_bytes(), 0);
    }

    #[test]
    fn nested_window_limit_rejects_before_read_and_releases_every_true_claim() {
        let backing = fixture(b"x");
        let mut leases = Vec::new();
        for _ in 0..MAX_ACTIVE_LEASES {
            leases.push(backing.lease(0..1).unwrap());
        }
        assert!(matches!(
            backing.lease(0..1),
            Err(OriginalBackingError::LeaseLimitExceeded)
        ));
        drop(leases);
        assert_eq!(backing.facts.active_leases.load(Ordering::Acquire), 0);
        backing
            .with_bytes(0..1, |bytes| assert_eq!(bytes, b"x"))
            .unwrap();
        backing.close().unwrap();
    }

    #[test]
    fn invalid_extent_is_rejected_without_touch_and_panicking_visitor_unpins_before_close() {
        let backing = fixture(&vec![0; ORIGINAL_READ_BYTES + 1]);
        assert!(matches!(
            backing.lease(0..ORIGINAL_READ_BYTES + 1),
            Err(OriginalBackingError::RangeOutsideMapping)
        ));
        assert!(matches!(
            backing.lease(std::ops::Range { start: 1, end: 0 }),
            Err(OriginalBackingError::RangeOutsideMapping)
        ));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = backing.with_bytes(0..1, |_| panic!("synthetic callback"));
        }));
        assert!(result.is_err());
        assert_eq!(backing.facts.active_leases.load(Ordering::Acquire), 0);
        let observer = backing.observer();
        assert_eq!(observer.reclaim_attempts(), 0); // panic只结束原pin，真正回收发生于最终close。
        backing.close().unwrap();
        assert_eq!(observer.reclaim_attempts(), 1);
        assert!(observer.unmap_succeeded());
    }
}

#[cfg(test)]
#[path = "original_backing/window_tests.rs"]
mod window_tests;
