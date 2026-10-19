//! 单个排版任务的固定资源收据；观察者只持原子标量，不保活文件、映射、字体或正文。

use std::{
    fmt,
    fs::File,
    io,
    ops::Deref,
    os::fd::{AsRawFd, IntoRawFd, RawFd},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};

/// 失败发生的公开文件/映射操作；不包含路径或正文。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PagingOperation {
    /// 创建匿名工作文件失败。
    CreateFile = 1,
    /// 复制原文件描述符失败。
    CloneFile = 2,
    /// 真实磁盘预分配失败。
    Preallocate = 3,
    /// 扩长原匿名文件失败。
    Resize = 4,
    /// 建立共享映射失败。
    Map = 5,
    /// 回写或撤驻留失败。
    Sync = 6,
    /// 归还实际映射失败。
    Unmap = 7,
    /// 关闭唯一文件描述符失败。
    Close = 8,
}

/// 同一任务的首个实际IO错误；后继错误不能覆盖它。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagingIoError {
    /// 实际失败的操作。
    pub operation: PagingOperation,
    /// 原始系统errno；无原始码的Rust IO错误保留EIO。
    pub errno: i32,
}
impl fmt::Display for PagingIoError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            output,
            "分页存储操作 {:?} 失败（errno={}）",
            self.operation, self.errno
        )
    }
}
impl std::error::Error for PagingIoError {}

/// 任务自己的资源事实；mapped_bytes是虚拟映射extent，不能替代RSS/footprint实测。
/// 发生close/munmap失败时，未取得成功归还收据的数量继续保留，不能假清零。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagingResourceSnapshot {
    /// 尚由本任务拥有的固定工作块分配字节；不是操作系统resident或footprint采样。
    pub work_bytes: usize,
    /// 本任务实际同时拥有的固定工作块字节峰值；不累加先后已经释放的工作块。
    pub peak_work_bytes: usize,
    /// 尚未取得成功close收据的FD数量，包括真实dup的FD。
    pub files_open: usize,
    /// 尚未取得成功munmap收据的映射数量。
    pub mappings_open: usize,
    /// 所有尚未归还映射的extent字节总数。
    pub mapped_bytes: usize,
    /// 本任务真实同时存在的映射extent峰值，包含增长期间的新旧映射。
    pub peak_mapped_bytes: usize,
    /// 累计实际创建/dup成功的FD数量。
    pub files_created: u64,
    /// 累计close实际成功的FD数量。
    pub files_closed: u64,
    /// 累计mmap实际成功的次数。
    pub mappings_created: u64,
    /// 累计munmap实际成功的次数。
    pub mappings_released: u64,
    /// 原任务永久保留的首个IO失败。
    pub first_error: Option<PagingIoError>,
    /// 任一次归还失败都永久为true；不能用最终数量零覆盖失败历史。
    pub cleanup_failed: bool,
    /// 实际访问路径保持有界，且平台已具备驻留对照资格；不是整产品内存通过结论。
    pub bounded_access_certified: bool,
}

struct State {
    work_bytes: AtomicUsize,
    peak_work_bytes: AtomicUsize,
    files_open: AtomicUsize,
    mappings_open: AtomicUsize,
    mapped_bytes: AtomicUsize,
    peak_mapped_bytes: AtomicUsize,
    files_created: AtomicU64,
    files_closed: AtomicU64,
    mappings_created: AtomicU64,
    mappings_released: AtomicU64,
    first_error: AtomicU64,
    cleanup_failed: AtomicBool,
    bounded: AtomicBool,
    #[cfg(test)]
    fail_close_after_real: AtomicBool,
}
impl State {
    fn new() -> Self {
        Self {
            work_bytes: AtomicUsize::new(0),
            peak_work_bytes: AtomicUsize::new(0),
            files_open: AtomicUsize::new(0),
            mappings_open: AtomicUsize::new(0),
            mapped_bytes: AtomicUsize::new(0),
            peak_mapped_bytes: AtomicUsize::new(0),
            files_created: AtomicU64::new(0),
            files_closed: AtomicU64::new(0),
            mappings_created: AtomicU64::new(0),
            mappings_released: AtomicU64::new(0),
            first_error: AtomicU64::new(0),
            cleanup_failed: AtomicBool::new(false),
            bounded: AtomicBool::new(true),
            #[cfg(test)]
            fail_close_after_real: AtomicBool::new(false),
        }
    }
    fn error(&self) -> Option<PagingIoError> {
        let encoded = self.first_error.load(Ordering::Acquire);
        if encoded == 0 {
            return None;
        }
        let operation = match (encoded >> 32) as u32 {
            1 => PagingOperation::CreateFile,
            2 => PagingOperation::CloneFile,
            3 => PagingOperation::Preallocate,
            4 => PagingOperation::Resize,
            5 => PagingOperation::Map,
            6 => PagingOperation::Sync,
            7 => PagingOperation::Unmap,
            8 => PagingOperation::Close,
            _ => unreachable!("首错误只由本模块的封闭enum编码"),
        };
        Some(PagingIoError {
            operation,
            errno: encoded as u32 as i32,
        })
    }
    fn record_error(&self, operation: PagingOperation, error: &io::Error) {
        let errno = error.raw_os_error().unwrap_or(libc::EIO);
        let encoded = (u64::from(operation as u32) << 32) | u64::from(errno as u32);
        // 每个任务只有一份固定状态；原子首写保留实际最先观察的错误，不建全局日志。
        let _ = self
            .first_error
            .compare_exchange(0, encoded, Ordering::AcqRel, Ordering::Acquire);
        self.bounded.store(false, Ordering::Release);
        if matches!(operation, PagingOperation::Unmap | PagingOperation::Close) {
            self.cleanup_failed.store(true, Ordering::Release);
        }
    }
    fn snapshot(&self) -> PagingResourceSnapshot {
        PagingResourceSnapshot {
            work_bytes: self.work_bytes.load(Ordering::Acquire),
            peak_work_bytes: self.peak_work_bytes.load(Ordering::Acquire),
            files_open: self.files_open.load(Ordering::Acquire),
            mappings_open: self.mappings_open.load(Ordering::Acquire),
            mapped_bytes: self.mapped_bytes.load(Ordering::Acquire),
            peak_mapped_bytes: self.peak_mapped_bytes.load(Ordering::Acquire),
            files_created: self.files_created.load(Ordering::Acquire),
            files_closed: self.files_closed.load(Ordering::Acquire),
            mappings_created: self.mappings_created.load(Ordering::Acquire),
            mappings_released: self.mappings_released.load(Ordering::Acquire),
            first_error: self.error(),
            cleanup_failed: self.cleanup_failed.load(Ordering::Acquire),
            bounded_access_certified: cfg!(target_os = "macos")
                && self.bounded.load(Ordering::Acquire),
        }
    }
}

/// 不保活任何业务资源的固定观察者；应在owner实际结束之后核最终快照。
/// 运行中多项原子字段不是事务快照，不能用一次中途采样证明任务已完全归还。
#[derive(Clone)]
pub struct PagingResourceObserver(Arc<State>);
impl PagingResourceObserver {
    /// 只读原scope首次真实错误，协作检查无需为此加载全部资源计数。
    pub fn first_error(&self) -> Option<PagingIoError> {
        self.0.error()
    }
    /// 读取同一个任务的累计真实资源收据，不查询或重建任何文件/映射。
    pub fn snapshot(&self) -> PagingResourceSnapshot {
        self.0.snapshot()
    }
}

#[derive(Clone)]
pub(crate) struct PagingScope(Arc<State>);
impl PagingScope {
    /// 只在实际分配成功后计入同一scope；没有FD或mmap的工作块不冒记分页I/O。
    pub(crate) fn allocated_work(&self, bytes: usize) {
        let current = self.0.work_bytes.fetch_add(bytes, Ordering::AcqRel) + bytes;
        self.0.peak_work_bytes.fetch_max(current, Ordering::AcqRel);
    }
    /// 调用者已经按原Layout实际dealloc；只有这个事实之后才能归零工作块收据。
    pub(crate) fn released_work(&self, bytes: usize) {
        self.0.work_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }
    pub(crate) fn new() -> Self {
        Self(Arc::new(State::new()))
    }
    pub(crate) fn observer(&self) -> PagingResourceObserver {
        PagingResourceObserver(self.0.clone())
    }
    pub(crate) fn error(&self) -> Option<PagingIoError> {
        self.0.error()
    }
    pub(crate) fn record_error(&self, operation: PagingOperation, error: &io::Error) {
        self.0.record_error(operation, error);
    }
    pub(crate) fn unbounded(&self) {
        self.0.bounded.store(false, Ordering::Release);
    }
    pub(crate) fn bounded(&self) -> bool {
        self.0.bounded.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(crate) fn test_fail_next_close_after_real(&self) {
        self.0.fail_close_after_real.store(true, Ordering::Release);
    }
    pub(crate) fn mapped(&self, bytes: usize) {
        self.0.mappings_open.fetch_add(1, Ordering::AcqRel);
        self.0.mappings_created.fetch_add(1, Ordering::AcqRel);
        let current = self.0.mapped_bytes.fetch_add(bytes, Ordering::AcqRel) + bytes;
        self.0
            .peak_mapped_bytes
            .fetch_max(current, Ordering::AcqRel);
    }
    pub(crate) fn unmapped(&self, status: i32, bytes: usize) {
        if status == 0 {
            self.0.mappings_open.fetch_sub(1, Ordering::AcqRel);
            self.0.mapped_bytes.fetch_sub(bytes, Ordering::AcqRel);
            self.0.mappings_released.fetch_add(1, Ordering::AcqRel);
        } else {
            // errno必须在另一个系统调用之前捕获，失败记录保留实际未归还的数量。
            self.record_error(PagingOperation::Unmap, &io::Error::last_os_error());
        }
    }
}

/// 真实FD的唯一close owner；每个成功create/dup都先纳入原scope，所有提前失败也能配平。
pub(crate) struct TrackedFile {
    file: Option<File>,
    scope: PagingScope,
}
impl TrackedFile {
    pub(crate) fn create(scope: PagingScope) -> io::Result<Self> {
        match tempfile::tempfile() {
            Ok(file) => Ok(Self::from_file(file, scope)),
            Err(error) => {
                scope.record_error(PagingOperation::CreateFile, &error);
                Err(error)
            }
        }
    }
    fn from_file(file: File, scope: PagingScope) -> Self {
        scope.0.files_open.fetch_add(1, Ordering::AcqRel);
        scope.0.files_created.fetch_add(1, Ordering::AcqRel);
        Self {
            file: Some(file),
            scope,
        }
    }
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        match self.deref().try_clone() {
            Ok(file) => Ok(Self::from_file(file, self.scope.clone())),
            Err(error) => {
                self.scope.record_error(PagingOperation::CloneFile, &error);
                Err(error)
            }
        }
    }
}
impl Deref for TrackedFile {
    type Target = File;
    fn deref(&self) -> &File {
        self.file.as_ref().expect("FD只在唯一Drop中take")
    }
}
impl AsRawFd for TrackedFile {
    fn as_raw_fd(&self) -> RawFd {
        self.deref().as_raw_fd()
    }
}
impl Drop for TrackedFile {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        let fd = file.into_raw_fd();
        // SAFETY: into_raw_fd转移此唯一FD的close义务；没有第二File能重复关闭该整数。
        // 失败不盲目重试，以免平台已关闭并复用了同一FD而误关无关资源。
        if unsafe { libc::close(fd) } == 0 {
            self.scope.0.files_open.fetch_sub(1, Ordering::AcqRel);
            self.scope.0.files_closed.fetch_add(1, Ordering::AcqRel);
            #[cfg(test)]
            if self
                .scope
                .0
                .fail_close_after_real
                .swap(false, Ordering::AcqRel)
            {
                // 原FD已经实际close成功后才注入负面收据；只验证失败历史不能被归零掩盖，
                // 不把测试注入声称为内核真实close失败，也不保留/重试已归还的FD。
                self.scope.record_error(
                    PagingOperation::Close,
                    &io::Error::from_raw_os_error(libc::EIO),
                );
            }
        } else {
            self.scope
                .record_error(PagingOperation::Close, &io::Error::last_os_error());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_descriptor_failure_stays_first_after_owner_drop() {
        let scope = PagingScope::new();
        let observer = scope.observer();
        // 只读null设备不能提供可写文件预分配；公开平台API必须返回真实错误，不能造成功。
        let file = TrackedFile::from_file(File::open("/dev/null").unwrap(), scope.clone());
        assert_eq!(observer.snapshot().files_open, 1);
        let actual_error = super::super::platform::preallocate(&file, 0, 4096).unwrap_err();
        scope.record_error(PagingOperation::Preallocate, &actual_error);
        let first = observer.snapshot().first_error.unwrap();
        assert_eq!(first.errno, actual_error.raw_os_error().unwrap());
        // 后续故障不能覆盖原错误；释放真实FD不恢复已经撤销的任务权限。
        scope.record_error(
            PagingOperation::Map,
            &io::Error::from_raw_os_error(libc::ENOMEM),
        );
        drop(file);
        let ended = observer.snapshot();
        assert_eq!(ended.files_open, 0);
        assert_eq!(ended.files_created, ended.files_closed);
        assert_eq!(ended.first_error, Some(first));
        assert!(!ended.bounded_access_certified);
        assert!(!ended.cleanup_failed); // 故障发生于工作操作；实际close仍成功。
    }
}
