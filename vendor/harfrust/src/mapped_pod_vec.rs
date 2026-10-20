//! 字形元数据先用固定小工作块，真实溢出才文件分页；原字形算法与输入语义不变。
//!
//! 工作块和映射都只保存算法元数据，不建立原文镜像。逐项访问保留固定窗口；调用者拿到
//! 整片借用时，必须在真实循环内使用配套checkpoint，不能把阶段结束当峰值证明。

#![deny(unsafe_op_in_unsafe_fn)]

use bytemuck::Pod;
use core::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    ops::{
        Bound, Deref, DerefMut, Index, IndexMut, Range, RangeBounds, RangeFrom, RangeFull,
        RangeInclusive, RangeTo, RangeToInclusive,
    },
    ptr::NonNull,
};
use std::{
    alloc::{alloc_zeroed, dealloc, Layout},
    os::fd::AsRawFd,
    rc::{Rc, Weak},
};

const BUCKET_BYTES: usize = 64 * 1024;
// 很短的原整形项只分配实际小工作区；64KiB仍是转文件的唯一上界，不设池或跨任务库存。
const MIN_WORK_BYTES: usize = 4 * 1024;
const WINDOWS: usize = 8;
mod platform;
mod resources;
pub use resources::{
    PagingIoError, PagingOperation, PagingResourceObserver, PagingResourceSnapshot,
};
use resources::{PagingScope, TrackedFile};
use std::io;

#[derive(Clone, Copy)]
struct Window {
    bucket: usize,
    stamp: u64,
}

// 内容证明只保存范围和弱身份；绝不保活映射、文件、字体或正文。
struct SpanTicket {
    owner: Weak<MutationDomain>,
    scope: Weak<()>,
    bytes: Cell<(usize, usize)>,
    stride: usize,
    valid: Cell<bool>,
}
#[derive(Clone)]
pub(crate) struct ReadSpanWitness {
    ticket: Rc<SpanTicket>,
}

struct MutationDomain {
    observers: RefCell<[Weak<SpanTicket>; WINDOWS]>,
    max_end: Cell<usize>,
    next_slot: Cell<usize>,
}
impl MutationDomain {
    fn new() -> Self {
        Self {
            observers: RefCell::new(core::array::from_fn(|_| Weak::new())),
            max_end: Cell::new(0),
            next_slot: Cell::new(0),
        }
    }
    #[inline]
    fn invalidate(&self, bytes: Range<usize>) {
        // 半开区间的尾部append不与旧证明相交；常见路径不借用观察表或增加Rc计数。
        if bytes.is_empty() || bytes.start >= self.max_end.get() {
            return;
        }
        let mut maximum = 0;
        for weak in self.observers.borrow().iter() {
            if let Some(ticket) = weak.upgrade() {
                let (start, end) = ticket.bytes.get();
                if ticket.valid.get() && bytes.start < end && start < bytes.end {
                    ticket.valid.set(false);
                }
                if ticket.valid.get() {
                    maximum = maximum.max(end);
                }
            }
        }
        self.max_end.set(maximum);
    }
    fn invalidate_all(&self) {
        if self.max_end.replace(0) == 0 {
            return;
        }
        for weak in self.observers.borrow_mut().iter_mut() {
            if let Some(ticket) = weak.upgrade() {
                ticket.valid.set(false);
            }
            *weak = Weak::new();
        }
    }
    fn register(
        self: &Rc<Self>,
        scope: &Rc<()>,
        bytes: Range<usize>,
        stride: usize,
    ) -> ReadSpanWitness {
        let ticket = Rc::new(SpanTicket {
            owner: Rc::downgrade(self),
            scope: Rc::downgrade(scope),
            bytes: Cell::new((bytes.start, bytes.end)),
            stride,
            valid: Cell::new(true),
        });
        let mut observers = self.observers.borrow_mut();
        let slot = observers
            .iter()
            .position(|weak| weak.upgrade().is_none_or(|old| !old.valid.get()))
            .unwrap_or_else(|| {
                // 固定表满时先撤旧证明，再复用弱槽；旧缓存只能回到真实读取，不得继续跳过。
                let slot = self.next_slot.get();
                self.next_slot.set((slot + 1) % WINDOWS);
                if let Some(old) = observers[slot].upgrade() {
                    old.valid.set(false);
                }
                slot
            });
        observers[slot] = Rc::downgrade(&ticket);
        self.max_end.set(self.max_end.get().max(bytes.end));
        ReadSpanWitness { ticket }
    }
}

#[derive(Clone, Copy)]
struct ResidentSpan {
    start: usize,
    end: usize,
    mapping: usize,
    epoch: u64,
}
impl ResidentSpan {
    const EMPTY: Self = Self {
        start: 0,
        end: 0,
        mapping: 0,
        epoch: 0,
    };
}

struct PageState {
    base: NonNull<u8>,
    bytes: usize,
    // None是固定工作块，不具备mmap页对齐或回写义务；Some只来自真实文件映射。
    page: Option<usize>,
    poison: Cell<Option<i32>>,
    scope: PagingScope,
    mutation: Rc<MutationDomain>,
    epoch: Cell<u64>,
    fast_span: Cell<ResidentSpan>,
    bounded_access: Cell<bool>,
    windows: Cell<[Option<Window>; WINDOWS]>,
    clock: Cell<u64>,
    last_bucket: Cell<usize>,
}

impl PageState {
    #[inline(always)]
    fn fast_contains(&self, byte: usize, size: usize) -> bool {
        let span = self.fast_span.get();
        // 这里只缓存已经登记的地址范围，不缓存字形值；失败、撤驻留或映射身份变化
        // 都必须重新走慢路径，不能用旧窗口掩盖永久IO撤权。
        self.poison.get().is_none()
            && span.mapping == self.base.as_ptr().addr()
            && span.epoch == self.epoch.get()
            && byte >= span.start
            && byte + size <= span.end
    }
    #[inline(always)]
    fn revoke_fast(&self) {
        self.fast_span.set(ResidentSpan::EMPTY);
        self.epoch.set(self.epoch.get().wrapping_add(1));
    }
    fn poison(&self, code: i32) -> bool {
        // 第一处IO故障永久保留；之后即使数据仍可读，也不能提交形状结果。
        self.revoke_fast();
        self.scope.unbounded();
        self.mutation.invalidate_all();
        if self.poison.get().is_none() {
            self.poison.set(Some(code));
        }
        false
    }

    fn flush_bytes(&self, start: usize, end: usize) -> bool {
        if self.poison.get().is_some() {
            return false;
        }
        if start >= end {
            return true;
        }
        if end > self.bytes {
            return self.poison(libc::EINVAL);
        }
        let Some(page) = self.page else {
            self.revoke_fast();
            return true; // 固定工作块只保实际内存与内容权限，不伪造msync成功或内核I/O错误。
        };
        let begin = start / page * page;
        let Some(rounded) = end.checked_add(page - 1).map(|n| n / page * page) else {
            return self.poison(libc::EOVERFLOW);
        };
        let limit = rounded.min(self.bytes);
        // 公共同步/撤驻留调用前先撤销所有借用句柄共享的快路票。
        self.revoke_fast();
        // SAFETY: 地址与长度均属于本对象仍活着的页对齐MAP_SHARED映射；此调用
        // 只回写并撤驻留，不改地址/逻辑字节，也不解除任何Rust借用。
        let status = unsafe { platform::flush(self.base.as_ptr().add(begin), limit - begin) };
        if let Err(error) = status {
            self.scope.record_error(PagingOperation::Sync, &error);
            return self.poison(error.raw_os_error().unwrap_or(libc::EIO));
        }
        true
    }

    #[inline(never)]
    fn touch(&self, byte: usize, size: usize) {
        if self.poison.get().is_some() {
            return;
        }
        let Some(end) = byte.checked_add(size) else {
            self.poison(libc::EOVERFLOW);
            return;
        };
        if end > self.bytes {
            self.poison(libc::EINVAL);
            return;
        }
        let first = byte / BUCKET_BYTES;
        let last = end.saturating_sub(1) / BUCKET_BYTES;
        for bucket in first..=last {
            if self.last_bucket.get() == bucket {
                continue;
            }
            let mut windows = self.windows.get();
            let stamp = self.clock.get().wrapping_add(1);
            self.clock.set(stamp);
            let slot = if let Some(hit) = windows
                .iter()
                .position(|w| w.is_some_and(|w| w.bucket == bucket))
            {
                hit
            } else if let Some(empty) = windows.iter().position(Option::is_none) {
                empty
            } else {
                // 淘汰LRU只改变物理驻留；即使调用者保存较早的引用，其值仍有效。
                let victim = windows
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, w)| w.map_or(0, |w| w.stamp))
                    .map_or(0, |(i, _)| i);
                let old = windows[victim].expect("filled window");
                if !self.flush_bytes(
                    old.bucket * BUCKET_BYTES,
                    ((old.bucket + 1) * BUCKET_BYTES).min(self.bytes),
                ) {
                    return;
                }
                victim
            };
            windows[slot] = Some(Window { bucket, stamp });
            self.windows.set(windows);
            self.last_bucket.set(bucket);
        }
        // 一个实际访问最多跨两个64KiB桶；只有所有桶真正登记成功才发布快路。
        // 原映射和epoch同票保存，后续纯读取不再重复复制LRU表或更新时钟。
        if self.poison.get().is_none() {
            self.fast_span.set(ResidentSpan {
                start: first * BUCKET_BYTES,
                end: ((last + 1) * BUCKET_BYTES).min(self.bytes),
                mapping: self.base.as_ptr().addr(),
                epoch: self.epoch.get(),
            });
        }
    }

    fn checkpoint(&self, bytes: Range<usize>) -> bool {
        if !self.flush_bytes(bytes.start, bytes.end) {
            return false;
        }
        // 只撤本次真实访问范围，不能遗忘其它仍驻留的LRU桶。保留它们的账，
        // 避免小片checkpoint反复清空索引表却遗留物理页，造成隐形累积。
        self.revoke_fast();
        self.last_bucket.set(usize::MAX);
        true
    }
}

/// Layout随真正的分配owner转移；同尺寸cast也不能改掉dealloc所需的原对齐。
enum Backing {
    Work(Layout),
    File(TrackedFile),
}
struct Mapping {
    backing: Backing,
    state: PageState,
}
impl Mapping {
    /// 工作区只按本次实际所需字节增长，仍不超过原64KiB；不按Source总长或调用预算预留。
    fn workspace<T: Pod>(scope: PagingScope, bounded: bool, bytes: usize) -> Option<Self> {
        let layout = Layout::from_size_align(bytes, align_of::<T>())
            .expect("非零且不超过64KiB的真实Pod必须具有有效分配Layout");
        // SAFETY: Layout满足T的原对齐，大小来自已核的有限工作区；全零是Pod的有效表示。
        // 原Layout连同owner保存，任何cast或溢出转换都不会改用mmap的释放方法。
        let base = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        scope.allocated_work(layout.size());
        Some(Self {
            backing: Backing::Work(layout),
            state: PageState {
                base,
                bytes: layout.size(),
                page: None,
                poison: Cell::new(None),
                scope,
                mutation: Rc::new(MutationDomain::new()),
                epoch: Cell::new(0),
                fast_span: Cell::new(ResidentSpan::EMPTY),
                bounded_access: Cell::new(bounded),
                windows: Cell::new([None; WINDOWS]),
                clock: Cell::new(0),
                last_bucket: Cell::new(usize::MAX),
            },
        })
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        match &self.backing {
            Backing::Work(layout) => {
                // SAFETY: 此地址仍是alloc_zeroed返回的唯一活分配，所有借用已结束。
                // Layout是原创建值，连同cast一起移交；先真实释放，再报告工作块为零。
                unsafe { dealloc(self.state.base.as_ptr(), *layout) };
                self.state.scope.released_work(layout.size());
            }
            Backing::File(_) => {
                // SAFETY: 所有字形借用已经结束；只有真实mmap才交给唯一munmap owner。
                // 先登记实际munmap，随后TrackedFile字段单独核真实close，失败不假清零。
                let status =
                    unsafe { libc::munmap(self.state.base.as_ptr().cast(), self.state.bytes) };
                self.state.scope.unmapped(status, self.state.bytes);
            }
        }
    }
}

/// 配套借用的撤驻留句柄；不保活正文，不允许remap或修改任何字节。
#[derive(Clone, Copy)]
pub(crate) struct CheckpointHandle<'a> {
    state: &'a PageState,
    stride: usize,
    len: usize,
}
impl CheckpointHandle<'_> {
    pub(crate) fn checkpoint_range(&self, range: Range<usize>) -> bool {
        if range.start > range.end || range.end > self.len {
            return self.state.poison(libc::EINVAL);
        }
        let Some(start) = range.start.checked_mul(self.stride) else {
            return self.state.poison(libc::EOVERFLOW);
        };
        let Some(end) = range.end.checked_mul(self.stride) else {
            return self.state.poison(libc::EOVERFLOW);
        };
        self.state.checkpoint(start..end)
    }
    pub(crate) fn observe_raw_span(&self, range: Range<usize>) -> bool {
        self.state.mutation.invalidate_all();
        if range.start > range.end || range.end > self.len {
            return self.state.poison(libc::EINVAL);
        }
        let Some(bytes) = range.len().checked_mul(self.stride) else {
            return self.state.poison(libc::EOVERFLOW);
        };
        // 原外部callback仍收到原指针和完整数量；不能暗中拆分或截断来隐藏无界访问。
        if bytes > BUCKET_BYTES {
            self.state.bounded_access.set(false);
            self.state.scope.unbounded();
        } else if !range.is_empty() {
            // 小raw批次只有有限公开地址范围；调用前把完整跨度纳入同一LRU账，
            // 回调内不再假设有逐项Rust迭代器。此操作不读取或预造任何字形值。
            let Some(start) = range.start.checked_mul(self.stride) else {
                return self.state.poison(libc::EOVERFLOW);
            };
            self.state.touch(start, bytes);
        }
        self.state.bounded_access.get()
    }
    pub(crate) fn finish_callback(&self) -> bool {
        // 正常迭代器已逐项touch，有限raw也已声明完整地址窗口；回调返回不必
        // 对每个短词重复msync同一页，真正LRU淘汰仍执行原回写+撤驻留。
        self.state.poison.get().is_none()
    }
    #[inline(always)]
    pub(crate) fn touch_index(&self, index: usize) -> bool {
        if index >= self.len {
            return self.state.poison(libc::EINVAL);
        }
        let byte = index * self.stride;
        if !self.state.fast_contains(byte, self.stride) {
            self.state.touch(byte, self.stride);
        }
        self.state.poison.get().is_none()
    }
    #[cfg(test)]
    pub(crate) fn checkpoint_all(&self) -> bool {
        self.checkpoint_range(0..self.len)
    }
}

pub(crate) struct MappedPodVec<T: Pod> {
    scope: PagingScope,
    map: Option<Mapping>,
    len: usize,
    initialized: usize,
    failed: bool,
    marker: PhantomData<T>,
}
impl<T: Pod> Default for MappedPodVec<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T: Pod> MappedPodVec<T> {
    pub(crate) fn new() -> Self {
        Self::new_in(PagingScope::new())
    }
    pub(crate) fn new_in(scope: PagingScope) -> Self {
        // 实际记录是20或64字节；不接受单条自身就超过有限窗口的未知Pod类型。
        assert!(size_of::<T>() != 0 && size_of::<T>() <= BUCKET_BYTES);
        Self {
            scope,
            map: None,
            len: 0,
            initialized: 0,
            failed: false,
            marker: PhantomData,
        }
    }
    pub(crate) fn paging_scope(&self) -> PagingScope {
        self.scope.clone()
    }
    pub(crate) fn resource_observer(&self) -> PagingResourceObserver {
        self.scope.observer()
    }
    pub(crate) fn paging_error(&self) -> Option<PagingIoError> {
        self.scope.error()
    }
    pub(crate) fn len(&self) -> usize {
        self.len
    }
    pub(crate) fn bounded_access_certified(&self) -> bool {
        self.scope.bounded()
            && self
                .map
                .as_ref()
                .is_none_or(|m| m.state.bounded_access.get())
    }
    pub(crate) fn healthy(&self) -> bool {
        !self.failed
            && self.scope.error().is_none()
            && self
                .map
                .as_ref()
                .is_none_or(|m| m.state.poison.get().is_none())
    }
    fn invalidate_all_writes(&self) {
        if let Some(mapping) = &self.map {
            mapping.state.mutation.invalidate_all();
        }
    }
    #[inline]
    fn authorize_write(&self, range: Range<usize>) {
        if let Some(mapping) = &self.map {
            mapping
                .state
                .mutation
                .invalidate(range.start * size_of::<T>()..range.end * size_of::<T>());
        }
    }
    pub(crate) fn read_witness(
        &self,
        scope: &Rc<()>,
        range: Range<usize>,
    ) -> Option<ReadSpanWitness> {
        // 调用方仅能在同一只读借用刚证明整个范围后登记；本层再核真实owner和范围。
        if !self.healthy() || range.start >= range.end || range.end > self.len {
            return None;
        }
        let mapping = self.map.as_ref()?;
        Some(mapping.state.mutation.register(
            scope,
            range.start * size_of::<T>()..range.end * size_of::<T>(),
            size_of::<T>(),
        ))
    }
    pub(crate) fn witness_range(
        &self,
        scope: &Rc<()>,
        witness: &ReadSpanWitness,
    ) -> Option<Range<usize>> {
        if !self.healthy() || !witness.ticket.valid.get() || witness.ticket.stride != size_of::<T>()
        {
            return None;
        }
        let mapping = self.map.as_ref()?;
        let owner = witness.ticket.owner.upgrade()?;
        let old_scope = witness.ticket.scope.upgrade()?;
        if !Rc::ptr_eq(&owner, &mapping.state.mutation) || !Rc::ptr_eq(&old_scope, scope) {
            return None;
        }
        let (start, end) = witness.ticket.bytes.get();
        if start >= end
            || start % size_of::<T>() != 0
            || end % size_of::<T>() != 0
            || end / size_of::<T>() > self.len
        {
            return None;
        }
        Some(start / size_of::<T>()..end / size_of::<T>())
    }
    pub(crate) fn extend_read_witness(
        &self,
        scope: &Rc<()>,
        witness: &ReadSpanWitness,
        range: Range<usize>,
    ) -> bool {
        let Some(old) = self.witness_range(scope, witness) else {
            return false;
        };
        if range.start >= range.end
            || range.end > self.len
            || range.start > old.end
            || old.start > range.end
        {
            return false;
        }
        let start = old.start.min(range.start) * size_of::<T>();
        let end = old.end.max(range.end) * size_of::<T>();
        witness.ticket.bytes.set((start, end));
        self.map.as_ref().unwrap().state.mutation.max_end.set(
            self.map
                .as_ref()
                .unwrap()
                .state
                .mutation
                .max_end
                .get()
                .max(end),
        );
        true
    }

    fn fail(&mut self) -> bool {
        self.invalidate_all_writes();
        self.failed = true;
        false
    }
    fn fail_io(&mut self, operation: PagingOperation, error: &io::Error) -> bool {
        self.scope.record_error(operation, error);
        self.fail()
    }
    fn capacity(&self) -> usize {
        self.map
            .as_ref()
            .map_or(0, |m| m.state.bytes / size_of::<T>())
    }

    fn reserve(&mut self, required: usize) -> bool {
        if !self.healthy() {
            return false;
        }
        if required <= self.capacity() {
            return true;
        }
        self.invalidate_all_writes();
        let Some(required_bytes) = required.checked_mul(size_of::<T>()) else {
            return self.fail();
        };
        let candidate = if required_bytes <= BUCKET_BYTES
            && self
                .map
                .as_ref()
                .is_none_or(|mapping| matches!(mapping.backing, Backing::Work(_)))
        {
            // 已进入File的owner永不回heap。小工作区按实际容量向上取幂，增长仍留在同一原槽，
            // 不能只缩小首次分配、却让下一次合法的小项增长提前创建FD。
            let Some(bytes) = required_bytes.checked_next_power_of_two() else {
                return self.fail();
            };
            let bytes = bytes.max(MIN_WORK_BYTES);
            if crate::cooperative::poll_boundary().is_err() {
                return self.fail();
            }
            let Some(candidate) =
                Mapping::workspace::<T>(self.scope.clone(), self.bounded_access_certified(), bytes)
            else {
                return self.fail();
            };
            if crate::cooperative::poll_boundary().is_err() {
                return self.fail(); // 新块实际归还，旧块和initialized仍由原owner保留，失败不得换备用路径。
            }
            candidate
        } else {
            // 仅独占借用可改变映射地址，和原Vec增长的引用失效边界一致。
            let wanted = required.max(self.capacity().saturating_mul(2)).max(256);
            let page = self
                .map
                .as_ref()
                .and_then(|m| m.state.page)
                .unwrap_or_else(|| {
                    // SAFETY: sysconf只查询系统页大小，不触及业务内存。
                    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
                    usize::try_from(value).unwrap_or(0)
                });
            if page == 0 {
                return self.fail();
            }
            let Some(bytes) = wanted
                .checked_mul(size_of::<T>())
                .and_then(|n| n.checked_add(page - 1))
                .map(|n| n / page * page)
            else {
                return self.fail();
            };
            let Ok(bytes_off) = i64::try_from(bytes) else {
                return self.fail();
            };
            let file = match self.map.as_ref().map(|m| &m.backing) {
                Some(Backing::File(file)) => match file.try_clone() {
                    Ok(file) => file,
                    Err(error) => return self.fail_io(PagingOperation::CloneFile, &error),
                },
                Some(Backing::Work(_)) | None => match TrackedFile::create(self.scope.clone()) {
                    Ok(file) => file,
                    Err(error) => return self.fail_io(PagingOperation::CreateFile, &error),
                },
            };
            // 当前capacity必须先真实预留磁盘块；普通磁盘不足在公开错误边界失败。
            // 平台适配不回退heap数组，也不把失败延后到已发布引用的SIGBUS。
            let old_bytes = self.map.as_ref().map_or(0, |m| match m.backing {
                Backing::File(_) => m.state.bytes,
                Backing::Work(_) => 0, // 工作块从未占有这个新文件，预分配必须覆盖整个新文件区间。
            });
            if let Err(error) = platform::preallocate(&file, old_bytes, bytes_off) {
                return self.fail_io(PagingOperation::Preallocate, &error);
            }
            if let Err(error) = file.set_len(bytes as u64) {
                return self.fail_io(PagingOperation::Resize, &error);
            }
            // SAFETY: 文件先完成真实预分配/扩长；len为页倍数，PROT读写+SHARED不建立副本。
            let address = unsafe {
                libc::mmap(
                    core::ptr::null_mut(),
                    bytes,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                )
            };
            if address == libc::MAP_FAILED {
                return self.fail_io(PagingOperation::Map, &io::Error::last_os_error());
            }
            // mmap已实际成功就立即计费，连随后NonNull/对齐补偿失败也不能漏报资源。
            self.scope.mapped(bytes);
            let Some(base) = NonNull::new(address.cast::<u8>()) else {
                // SAFETY: 极端系统配置若把有效映射放到零地址，不能构造NonNull；
                // 新映射尚未发布，失败分支必须先归还它，不能泄漏文件映射。
                let status = unsafe { libc::munmap(address, bytes) };
                self.scope.unmapped(status, bytes);
                return self.fail();
            };
            if base.as_ptr().align_offset(align_of::<T>()) != 0 {
                // SAFETY: 此时尚未发布任何新引用，失败分支归还刚建立的映射。
                let status = unsafe { libc::munmap(address, bytes) };
                self.scope.unmapped(status, bytes);
                return self.fail();
            }
            let was_bounded = self.bounded_access_certified();
            Mapping {
                backing: Backing::File(file),
                state: PageState {
                    base,
                    bytes,
                    page: Some(page),
                    poison: Cell::new(None),
                    scope: self.scope.clone(),
                    mutation: Rc::new(MutationDomain::new()),
                    epoch: Cell::new(0),
                    fast_span: Cell::new(ResidentSpan::EMPTY),
                    bounded_access: Cell::new(was_bounded),
                    windows: Cell::new([None; WINDOWS]),
                    clock: Cell::new(0),
                    last_bucket: Cell::new(usize::MAX),
                },
            }
        };
        if let Some(old) = self
            .map
            .as_ref()
            .filter(|old| matches!(old.backing, Backing::Work(_)))
        {
            // 尚未commit或clear后仍保留的initialized字节也属于原有效状态，不能只迁移len。
            let count = self.initialized * size_of::<T>();
            if crate::cooperative::poll_boundary().is_err() {
                return self.fail(); // candidate先实际dealloc或unmap/close；旧工作块仍归原owner。
            }
            if count != 0 {
                candidate.state.touch(0, count);
                // SAFETY: initialized不超过原工作区；新工作块已真实zero，或目标文件已预分配/映射。
                // 两个活分配不重叠，只有独占self可迁移；一次复制最多64KiB，无Source读取。
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        old.state.base.as_ptr(),
                        candidate.state.base.as_ptr(),
                        count,
                    )
                };
            }
            if crate::cooperative::poll_boundary().is_err() {
                return self.fail(); // 不发布半迁移owner；scope保留实际取消/IO事实，绝不换备用实现。
            }
        }
        let old = self.map.replace(candidate);
        // 小块增长和最终转File共用同一份initialized迁移与发布。新旧块同时存活的真实峰值
        // 已由同scope计入，不能把32KiB+64KiB伪报成一块；旧Layout随原owner实际dealloc。
        // File增长仍引用同一inode；旧owner真实unmap/close失败时，不能继续发布新长度。
        drop(old);
        self.healthy()
    }

    pub(crate) fn prepare_resize(&mut self, new_len: usize, value: T) -> bool {
        if !self.reserve(new_len) {
            return false;
        }
        if new_len <= self.len {
            return true;
        }
        // 新工作块由alloc_zeroed、文件新区域由OS保证为零；重用旧区间或非零初值仍须实际写入。
        let zero = bytemuck::bytes_of(&value).iter().all(|b| *b == 0);
        let write_end = if zero {
            new_len.min(self.initialized)
        } else {
            new_len
        };
        if write_end > self.len && !self.fill_unpublished(self.len..write_end, value) {
            return false;
        }
        self.initialized = self.initialized.max(new_len);
        true
    }
    fn fill_unpublished(&mut self, range: Range<usize>, value: T) -> bool {
        self.authorize_write(range.clone());
        let Some(m) = self.map.as_ref() else {
            return range.is_empty();
        };
        let chunk = (BUCKET_BYTES / size_of::<T>()).max(1);
        let mut start = range.start;
        while start < range.end {
            if crate::cooperative::poll_loop().is_err() {
                return false;
            }
            let end = (start + chunk).min(range.end);
            // SAFETY: prepare_resize先完成容量；此区间为Pod有效字节，独占借用且不越界。
            unsafe {
                core::slice::from_raw_parts_mut(
                    m.state.base.as_ptr().cast::<T>().add(start),
                    end - start,
                )
            }
            .fill(value);
            if !m
                .state
                .checkpoint(start * size_of::<T>()..end * size_of::<T>())
            {
                return false;
            }
            start = end;
        }
        true
    }
    pub(crate) fn commit_len(&mut self, len: usize) {
        assert!(len <= self.initialized && len <= self.capacity());
        if len < self.len {
            self.authorize_write(len..self.len);
        }
        self.len = len;
    }
    pub(crate) fn clear(&mut self) {
        self.invalidate_all_writes();
        self.checkpoint_all();
        self.len = 0;
    }
    /// 公有slice离开本层后不能再观察调用方逐项访问，因此按真实整个跨度认证。
    /// 此只读观察不撤内容证明；真正可变授权仍由原IndexMut/as_mut_slice先撤权。
    pub(crate) fn observe_shared_span(&self, range: Range<usize>) {
        assert!(range.start <= range.end && range.end <= self.len);
        if range.is_empty() {
            return;
        }
        let mapping = self.map.as_ref().expect("非空真实映射");
        let bytes = range.len() * size_of::<T>();
        if bytes > BUCKET_BYTES {
            // 不截断或替换返回值；只是永久拒绝把不可观察的大slice当作有限驻留证据。
            mapping.state.bounded_access.set(false);
            mapping.state.scope.unbounded();
        } else {
            // 有限公开跨度在逃逸前真实登记；地址/记录值/owner身份均不改变。
            mapping.state.touch(range.start * size_of::<T>(), bytes);
        }
    }
    pub(crate) fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            return &[];
        }
        let m = self.map.as_ref().expect("nonempty mapping");
        // SAFETY: 只暴露已初始化Pod元素；共享借用期间不允许reserve或drop映射。
        unsafe { core::slice::from_raw_parts(m.state.base.as_ptr().cast(), self.len) }
    }
    fn normalized_range<R: RangeBounds<usize>>(&self, range: &R) -> Range<usize> {
        let start = match range.start_bound() {
            Bound::Included(i) => *i,
            Bound::Excluded(i) => i.checked_add(1).expect("range start overflow"),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(i) => i.checked_add(1).expect("range end overflow"),
            Bound::Excluded(i) => *i,
            Bound::Unbounded => self.len,
        };
        assert!(start <= end && end <= self.len);
        start..end
    }
    fn mutable_range(&mut self, range: Range<usize>) -> &mut [T] {
        self.authorize_write(range.clone());
        if range.is_empty() {
            return &mut [];
        }
        let mapping = self.map.as_ref().expect("mutable range map");
        // SAFETY: range由标准RangeBounds核过已初始化边界；独占owner借用保持原映射。
        // 只授权实际子范围，避免先构造全片可变引用而错误撤销无关append证明。
        unsafe {
            core::slice::from_raw_parts_mut(
                mapping.state.base.as_ptr().cast::<T>().add(range.start),
                range.len(),
            )
        }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [T] {
        self.invalidate_all_writes();
        if self.len == 0 {
            return &mut [];
        }
        let m = self.map.as_mut().expect("nonempty mapping");
        // SAFETY: 独占容器借用确保此Pod范围只有一个可变切片，长度仅包含已初始化元素。
        unsafe { core::slice::from_raw_parts_mut(m.state.base.as_ptr().cast(), self.len) }
    }
    pub(crate) fn as_mut_slice_with_checkpoint(&mut self) -> (&mut [T], CheckpointHandle<'_>) {
        self.invalidate_all_writes();
        let m = self.map.as_ref().expect("nonempty mapping");
        let handle = CheckpointHandle {
            state: &m.state,
            stride: size_of::<T>(),
            len: self.len,
        };
        // SAFETY: handle仅引用独立页状态，不读写T；两者借用共同阻止映射增长/销毁。
        let slice =
            unsafe { core::slice::from_raw_parts_mut(m.state.base.as_ptr().cast(), self.len) };
        (slice, handle)
    }
    pub(crate) fn checkpoint_range(&self, range: Range<usize>) -> bool {
        if range.start > range.end || range.end > self.len {
            return false;
        }
        self.map.as_ref().is_none_or(|m| {
            CheckpointHandle {
                state: &m.state,
                stride: size_of::<T>(),
                len: self.len,
            }
            .checkpoint_range(range)
        }) && self.healthy()
    }
    pub(crate) fn checkpoint_all(&self) -> bool {
        self.checkpoint_range(0..self.len)
    }
    pub(crate) fn for_each_chunk_mut(
        &mut self,
        range: Range<usize>,
        mut visit: impl FnMut(&mut [T]),
    ) -> bool {
        if range.start > range.end || range.end > self.len || !self.healthy() {
            return false;
        }
        let chunk = (BUCKET_BYTES / size_of::<T>()).max(1);
        let mut start = range.start;
        while start < range.end {
            if crate::cooperative::poll_loop().is_err() {
                return false;
            }
            let end = (start + chunk).min(range.end);
            visit(self.mutable_range(start..end));
            if crate::cooperative::poll_boundary().is_err() {
                return false;
            }
            if !self.checkpoint_range(start..end) {
                return false;
            }
            start = end;
        }
        self.healthy()
    }
    pub(crate) const fn finite_chunk_capacity() -> usize {
        BUCKET_BYTES / size_of::<T>()
    }

    pub(crate) fn with_finite_chunk<R>(
        &self,
        range: Range<usize>,
        visit: impl FnOnce(&[T]) -> R,
    ) -> Option<R> {
        if range.start > range.end
            || range.end > self.len
            || range.len() > Self::finite_chunk_capacity()
            || !self.healthy()
        {
            return None;
        }
        if range.is_empty() {
            return Some(visit(&[]));
        }
        let mapping = self.map.as_ref()?;
        mapping
            .state
            .touch(range.start * size_of::<T>(), range.len() * size_of::<T>());
        if !self.healthy() {
            return None;
        }
        // SAFETY: 共享借用禁止修改/增长/释放owner；只暴露已核的有限真实范围。
        let chunk = unsafe {
            core::slice::from_raw_parts(
                mapping.state.base.as_ptr().cast::<T>().add(range.start),
                range.len(),
            )
        };
        let result = visit(chunk);
        self.healthy().then_some(result)
    }
    pub(crate) fn with_finite_cast_chunk<U: Pod, R>(
        &self,
        range: Range<usize>,
        visit: impl FnOnce(&[U]) -> R,
    ) -> Option<R> {
        assert_eq!(size_of::<T>(), size_of::<U>());
        assert_eq!(align_of::<T>(), align_of::<U>());
        self.with_finite_chunk(range, |chunk| visit(bytemuck::cast_slice(chunk)))
    }

    #[cfg(test)]
    pub(crate) fn with_finite_chunk_mut<R>(
        &mut self,
        range: Range<usize>,
        visit: impl FnOnce(&mut [T]) -> R,
    ) -> Option<R> {
        // 此上限只约束一次借用的元数据字节，不截断文本、cluster或匹配语义。
        if range.start > range.end
            || range.end > self.len
            || range.len() > Self::finite_chunk_capacity()
            || !self.healthy()
        {
            self.fail();
            return None;
        }
        if range.is_empty() {
            return Some(visit(&mut []));
        }
        let Some(start) = range.start.checked_mul(size_of::<T>()) else {
            self.fail();
            return None;
        };
        let Some(bytes) = range.len().checked_mul(size_of::<T>()) else {
            self.fail();
            return None;
        };
        self.authorize_write(range.clone());
        let result = {
            let Some(mapping) = self.map.as_ref() else {
                self.fail();
                return None;
            };
            // 完整真实跨度在借用开始前登记一次；闭包内无需逐项重验同一地址。
            // 所有可能淘汰旧页的IO发生在构造本次有限切片之前。
            mapping.state.touch(start, bytes);
            if mapping.state.poison.get().is_some() {
                return None;
            }
            // SAFETY: 入口已核已初始化范围和最多64KiB，起点严格在同一活映射中。
            // 独占容器借用禁止期间grow/drop；闭包的短借用不能通过R逃逸。
            // PageState只涉及独立页元数据，不形成第二个字形数据引用。
            let chunk = unsafe {
                core::slice::from_raw_parts_mut(
                    mapping.state.base.as_ptr().cast::<T>().add(range.start),
                    range.len(),
                )
            };
            visit(chunk)
        };
        // 原属性/coverage回调可以写本次真实记录；不缓存其结果或覆盖先前写入。
        // 回调结束只检查永久失败，不强制把每个短匹配再次回写到磁盘。
        self.healthy().then_some(result)
    }

    #[cfg(test)]
    pub(crate) fn with_finite_cast_chunk_mut<U: Pod, R>(
        &mut self,
        range: Range<usize>,
        visit: impl FnOnce(&mut [U]) -> R,
    ) -> Option<R> {
        assert_eq!(size_of::<T>(), size_of::<U>());
        assert_eq!(align_of::<T>(), align_of::<U>());
        // 两种20字节Pod使用同一真实backing和同一短借用，绝不转移owner或复制记录。
        self.with_finite_chunk_mut(range, |chunk| visit(bytemuck::cast_slice_mut(chunk)))
    }

    pub(crate) fn cast<U: Pod>(self) -> MappedPodVec<U> {
        self.invalidate_all_writes();
        assert_eq!(size_of::<T>(), size_of::<U>());
        assert_eq!(align_of::<T>(), align_of::<U>());
        // Pod无析构，尺寸/对齐相同；只转移owner，绝不能把映射伪装成标准Vec分配。
        MappedPodVec {
            scope: self.scope,
            map: self.map,
            len: self.len,
            initialized: self.initialized,
            failed: self.failed,
            marker: PhantomData,
        }
    }
    pub(crate) fn swap(&mut self, a: usize, b: usize) {
        let value = self[a];
        self[a] = self[b];
        self[b] = value;
    }
    pub(crate) fn copy_within(&mut self, source: Range<usize>, destination: usize) {
        assert!(
            source.start <= source.end
                && source.end <= self.len
                && destination <= self.len - (source.end - source.start)
        );
        self.authorize_write(destination..destination + source.len());
        // 和memmove保持相同的重叠方向；逐项索引使距离很远的源/目标仍进入有限LRU。
        if destination <= source.start {
            for i in 0..source.len() {
                shape_cooperative_abort!();
                self[destination + i] = self[source.start + i];
            }
        } else {
            for i in (0..source.len()).rev() {
                shape_cooperative_abort!();
                self[destination + i] = self[source.start + i];
            }
        }
    }
    pub(crate) fn iter(&self) -> Iter<'_, T> {
        Iter {
            vector: self,
            offset: 0,
        }
    }
}

pub(crate) struct Iter<'a, T: Pod> {
    vector: &'a MappedPodVec<T>,
    offset: usize,
}
impl<'a, T: Pod> Iterator for Iter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        if crate::cooperative::poll_loop().is_err() {
            self.offset = self.vector.len; // 终态迭代器真实耗尽；不伪造仍可用的ExactSize尾部。
            return None;
        }
        if self.offset == self.vector.len {
            return None;
        }
        let i = self.offset;
        self.offset += 1;
        Some(&self.vector[i])
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.vector.len - self.offset;
        (remaining, Some(remaining))
    }
}
impl<T: Pod> ExactSizeIterator for Iter<'_, T> {}
impl<'a, T: Pod> IntoIterator for &'a MappedPodVec<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<T: Pod> Index<usize> for MappedPodVec<T> {
    type Output = T;
    #[inline(always)]
    fn index(&self, index: usize) -> &T {
        assert!(index < self.len);
        let m = self.map.as_ref().expect("mapped index");
        let byte = index * size_of::<T>();
        if self.failed || !m.state.fast_contains(byte, size_of::<T>()) {
            m.state.touch(byte, size_of::<T>());
        }
        // SAFETY: index先核已初始化len；len不超过真实映射容量，基址已核对齐。
        // 共享借用禁止增长/drop；快路只省去LRU登记，不改变原地址或字形值。
        unsafe { &*m.state.base.as_ptr().cast::<T>().add(index) }
    }
}
impl<T: Pod> IndexMut<usize> for MappedPodVec<T> {
    #[inline(always)]
    fn index_mut(&mut self, index: usize) -> &mut T {
        assert!(index < self.len);
        self.authorize_write(index..index + 1);
        let m = self.map.as_ref().expect("mapped index");
        let byte = index * size_of::<T>();
        if self.failed || !m.state.fast_contains(byte, size_of::<T>()) {
            m.state.touch(byte, size_of::<T>());
        }
        // SAFETY: 独占容器借用提供此元素的唯一可变引用；其余已核界限/对齐与
        // 共享索引相同。同步仅改物理驻留，绝不在引用存活期remap。
        unsafe { &mut *m.state.base.as_ptr().cast::<T>().add(index) }
    }
}
impl<T: Pod> Deref for MappedPodVec<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}
impl<T: Pod> DerefMut for MappedPodVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}
macro_rules! range_index {
    ($($range:ty),*) => {$(
        impl<T: Pod> Index<$range> for MappedPodVec<T> { type Output = [T]; fn index(&self, range: $range) -> &[T] { &self.as_slice()[range] } }
        impl<T: Pod> IndexMut<$range> for MappedPodVec<T> { fn index_mut(&mut self, range: $range) -> &mut [T] { let actual = self.normalized_range(&range); self.mutable_range(actual) } }
    )*};
}
range_index!(
    Range<usize>,
    RangeFrom<usize>,
    RangeFull,
    RangeInclusive<usize>,
    RangeTo<usize>,
    RangeToInclusive<usize>
);

// 以下仅属于固定控制入口，使用SDK mach/mach_vm.h公开ABI查询本进程映射区间。
#[cfg(all(test, target_os = "macos"))]
unsafe extern "C" {
    static mach_task_self_: libc::mach_port_t;
    fn mach_vm_region(
        task: libc::mach_port_t,
        address: *mut u64,
        size: *mut u64,
        flavor: i32,
        information: *mut i32,
        count: *mut u32,
        object: *mut libc::mach_port_t,
    ) -> i32;
    fn mach_port_deallocate(task: libc::mach_port_t, name: libc::mach_port_t) -> i32;
}

#[cfg(all(test, target_os = "macos"))]
fn address_is_unmapped(pointer: NonNull<u8>) -> Option<bool> {
    let requested = pointer.as_ptr().addr() as u64;
    let mut address = requested;
    let mut size = 0u64;
    let mut information = [0i32; 32];
    let mut count = information.len() as u32;
    let mut object = 0;
    // SAFETY: 只查询本进程公开Mach task port；输出均是独立固定容量栈变量。
    // VM_REGION_BASIC_INFO_64=9，数组容量大于SDK公开结构，内核按count返实际长度。
    let task = unsafe { mach_task_self_ };
    // SAFETY: SDK公开mach_vm_region签名逐项对应；不解引用被查询的原映射地址。
    let status = unsafe {
        mach_vm_region(
            task,
            &mut address,
            &mut size,
            9,
            information.as_mut_ptr(),
            &mut count,
            &mut object,
        )
    };
    if object != 0 {
        // SAFETY: 只归还此公开查询新得到的send right，绝不改目标task port本身。
        if unsafe { mach_port_deallocate(task, object) } != libc::KERN_SUCCESS {
            return None;
        }
    }
    if status == libc::KERN_INVALID_ADDRESS {
        return Some(true);
    }
    if status != libc::KERN_SUCCESS || count == 0 || count as usize > information.len() || size == 0
    {
        return None;
    }
    // 公开查询返回包含该地址或其后的第一个region；后移说明目标已是实际空洞。
    Some(address > requested)
}

// 单进程控制直接核原匿名FD和原映射地址：不通过Rust owner计数猜测OS归还。
#[cfg(all(test, target_os = "macos"))]
fn mapped_owner_contract() -> bool {
    let mut values = MappedPodVec::<[u32; 5]>::new();
    if !values.prepare_resize(5000, [0; 5]) {
        return false;
    }
    values.commit_len(5000);
    values[0] = [42; 5];
    let first = values.map.as_ref().expect("first map");
    let Backing::File(file) = &first.backing else {
        return false;
    };
    let old_fd = file.as_raw_fd();
    let old_address = first.state.base;
    if address_is_unmapped(old_address) != Some(false) {
        return false;
    }
    if !values.prepare_resize(100000, [0; 5]) {
        return false;
    }
    values.commit_len(100000);
    // SAFETY: F_GETFD只查询原整数FD状态，不读写可能已归还的文件内容。
    let old_closed = unsafe { libc::fcntl(old_fd, libc::F_GETFD) } == -1
        && io::Error::last_os_error().raw_os_error() == Some(libc::EBADF);
    let old_unmapped = address_is_unmapped(old_address) == Some(true);
    let values = values.cast::<[i32; 5]>();
    if values[0] != [42; 5] || !values.healthy() {
        return false;
    }
    let current = values.map.as_ref().expect("current map");
    let Backing::File(file) = &current.backing else {
        return false;
    };
    let final_fd = file.as_raw_fd();
    let final_address = current.state.base;
    let scope = Rc::new(());
    let proof = values.read_witness(&scope, 1..100).unwrap();
    if address_is_unmapped(final_address) != Some(false) {
        return false;
    }
    drop(values); // 类型转换后的唯一owner先munmap，再关闭匿名文件。
                  // SAFETY: 和上面的原FD查询相同；期间没有其它创建FD的动作。
    let final_closed = unsafe { libc::fcntl(final_fd, libc::F_GETFD) } == -1
        && io::Error::last_os_error().raw_os_error() == Some(libc::EBADF);
    let final_unmapped = address_is_unmapped(final_address) == Some(true);
    let weak_released = proof.ticket.owner.upgrade().is_none();
    eprintln!(
        "mapped_owner_contract old_closed={old_closed} old_unmapped={old_unmapped} final_closed={final_closed} final_unmapped={final_unmapped}"
    );
    old_closed && old_unmapped && final_closed && final_unmapped && weak_released
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
    struct Record {
        words: [u32; 5],
    }

    fn resized(len: usize) -> MappedPodVec<Record> {
        let mut values = MappedPodVec::new();
        assert!(values.prepare_resize(len, Record::default()));
        values.commit_len(len);
        values
    }

    #[test]
    fn workspace_cast_and_spill_keep_original_layout_initialized_tail_and_weak_permissions() {
        // u128保证真实16字节对齐，覆盖GPOS记录的要求；分配不是align1的byte Box。
        let mut values = MappedPodVec::<[u128; 4]>::new();
        let observer = values.resource_observer();
        assert_eq!(observer.snapshot().work_bytes, 0); // 空对象不能预留闲置工作区。
        assert!(values.prepare_resize(3, [1; 4]));
        values.commit_len(3);
        values[0] = [u128::MAX; 4];
        assert!(values.prepare_resize(7, [9; 4]));
        let active = observer.snapshot();
        assert!(active.work_bytes > 0 && active.work_bytes <= BUCKET_BYTES);
        assert_eq!(
            (active.files_open, active.mappings_open, active.mapped_bytes),
            (0, 0, 0)
        );
        assert_eq!(
            values.map.as_ref().unwrap().state.base.as_ptr().addr() % align_of::<u128>(),
            0
        );
        let scope = Rc::new(());
        let before_cast = values.read_witness(&scope, 0..3).unwrap();
        let mut values = values.cast::<[i128; 4]>();
        assert!(values.witness_range(&scope, &before_cast).is_none());
        assert_eq!(observer.snapshot().work_bytes, active.work_bytes);
        assert_eq!(values[0], [-1; 4]);
        let before_spill = values.read_witness(&scope, 0..3).unwrap();
        assert!(values.reserve(BUCKET_BYTES / size_of::<[i128; 4]>() + 1));
        assert!(values.witness_range(&scope, &before_spill).is_none());
        // reserve不重新初始化：只有保留原initialized而非len，尚未commit的四条才能保持。
        values.commit_len(7);
        assert_eq!(values[0], [-1; 4]);
        for index in 3..7 {
            assert_eq!(values[index], [9; 4]);
        }
        let spilled = observer.snapshot();
        assert_eq!(spilled.work_bytes, 0);
        assert_eq!(spilled.peak_work_bytes, active.work_bytes);
        assert_eq!((spilled.files_open, spilled.mappings_open), (1, 1));
        assert!(spilled.mapped_bytes > BUCKET_BYTES);
        let proof = values.read_witness(&scope, 0..3).unwrap();
        drop(values);
        assert!(proof.ticket.owner.upgrade().is_none());
        let ended = observer.snapshot();
        assert_eq!(
            (
                ended.work_bytes,
                ended.files_open,
                ended.mappings_open,
                ended.mapped_bytes
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(ended.files_created, ended.files_closed);
        assert_eq!(ended.mappings_created, ended.mappings_released);
        assert!(!ended.cleanup_failed && ended.first_error.is_none());
    }

    #[test]
    fn workspace_zero_reuse_and_drop_have_real_work_receipts_without_file_operations() {
        let mut values = resized(2000);
        let observer = values.resource_observer();
        values[0].words = [u32::MAX; 5];
        values.clear();
        assert!(values.prepare_resize(2100, Record::default()));
        values.commit_len(2100);
        assert!(values.iter().all(|value| *value == Record::default()));
        assert!(values.checkpoint_all());
        let active = observer.snapshot();
        assert_eq!(active.work_bytes, BUCKET_BYTES);
        assert_eq!((active.files_created, active.mappings_created), (0, 0));
        drop(values);
        let ended = observer.snapshot();
        assert_eq!(ended.work_bytes, 0);
        assert_eq!(ended.peak_work_bytes, BUCKET_BYTES);
        assert!(!ended.cleanup_failed && ended.first_error.is_none());
    }

    #[test]
    fn cancellation_after_actual_spill_copy_discards_candidate_without_losing_work_owner() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let mut values = resized(100);
        let observer = values.resource_observer();
        let original_work = observer.snapshot().work_bytes;
        let token = crate::CancellationToken::new();
        let requested = token.clone();
        let watched = observer.clone();
        let saw_mapping = AtomicBool::new(false);
        let health: crate::CooperativeSourceHealth = Arc::new(move |_| {
            if watched.snapshot().mappings_open != 0 && saw_mapping.swap(true, Ordering::AcqRel) {
                requested.request(); // 第一次见真实候选时保留，复制后的原边界才实际取消。
            }
            None
        });
        let result = crate::with_cooperative_control(
            crate::CooperativeControl::new(token, observer.clone(), Some(health)),
            || {
                assert!(!values.reserve(BUCKET_BYTES / size_of::<Record>() + 1));
                Ok(())
            },
        );
        assert_eq!(result, Err(crate::ShapeError::Cancelled));
        let failed = observer.snapshot();
        assert_eq!(failed.work_bytes, original_work);
        assert!(failed.mappings_created > 0 && failed.files_created > 0);
        assert_eq!((failed.files_open, failed.mappings_open), (0, 0));
        assert!(!values.healthy());
        drop(values);
        let ended = observer.snapshot();
        assert_eq!(ended.work_bytes, 0);
        assert_eq!(ended.files_created, ended.files_closed);
        assert_eq!(ended.mappings_created, ended.mappings_released);
        assert!(!ended.cleanup_failed && ended.first_error.is_none());
    }

    #[test]
    #[cfg(unix)]
    fn workspace_spill_descriptor_failure_preserves_real_errno_and_releases_work() {
        // FD限额只在原测试可执行文件的独立子进程改动，不污染并行shape或文件测试。
        const CHILD: &str = "HARFRUST_WORKSPACE_SPILL_FAILURE_CHILD";
        if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "mapped_pod_vec::tests::workspace_spill_descriptor_failure_preserves_real_errno_and_releases_work",
                    "--test-threads=1", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "真实spill失败控制未通过：{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let mut values = resized(100);
        let observer = values.resource_observer();
        let original_work = observer.snapshot().work_bytes;
        let scope = Rc::new(());
        let witness = values.read_witness(&scope, 0..100).unwrap();
        let mut old = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: 指针是完整可写rlimit，查询本独立进程限额，不修改已有FD或业务数据。
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut old) }, 0);
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: old.rlim_max,
        };
        // SAFETY: 仅降低本独立进程软限额；保留硬限额及原值，原有stdout/stderr仍可使用。
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        let prepared =
            values.prepare_resize(BUCKET_BYTES / size_of::<Record>() + 1, Record::default());
        let actual = observer.snapshot();
        // SAFETY: 原限额来自上面的真实getrlimit，软限额不超过未变更的硬限额。
        let restored = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &old) };
        assert_eq!(restored, 0);
        assert!(!prepared && !values.healthy());
        assert!(values.witness_range(&scope, &witness).is_none());
        assert_eq!(
            actual.first_error,
            Some(PagingIoError {
                operation: PagingOperation::CreateFile,
                errno: libc::EMFILE
            })
        );
        assert_eq!(actual.work_bytes, original_work); // 迁移失败不伪造旧块已归还，也不返回新heap兜底。
        assert_eq!((actual.files_created, actual.mappings_created), (0, 0));
        drop(values);
        let ended = observer.snapshot();
        assert_eq!(
            (ended.work_bytes, ended.files_open, ended.mappings_open),
            (0, 0, 0)
        );
        assert_eq!(ended.first_error, actual.first_error);
        assert!(!ended.cleanup_failed);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn anonymous_mapping_and_fd_are_actually_released() {
        // 地址归还控制必须隔离其他并行测试：旧地址被另一个真实mmap重用，并不能证明
        // 本owner没有munmap。单次exact子进程仍核全部FD/region/Weak，不豁免任何存活资源。
        const CHILD: &str = "HARFRUST_MAPPING_RELEASE_CONTROL_CHILD";
        if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
            assert!(mapped_owner_contract());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().expect("原测试可执行文件"))
            .args([
                "--exact",
                "mapped_pod_vec::tests::anonymous_mapping_and_fd_are_actually_released",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("隔离地址归还控制必须实际启动");
        // 只有真实子控制正常结束才签通过；故障输出只包含固定合成资源收据。
        assert!(
            output.status.success(),
            "地址归还控制失败：{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn grows_without_copy_and_preserves_every_word() {
        let mut values = resized(5000);
        for i in 0..values.len() {
            values[i].words = [i as u32; 5];
        }
        assert!(values.checkpoint_all());
        assert!(values.prepare_resize(70000, Record::default()));
        values.commit_len(70000);
        for i in 0..5000 {
            assert_eq!(values[i].words, [i as u32; 5]);
        }
        for i in 5000..70000 {
            assert_eq!(values[i], Record::default());
        }
        assert!(values.healthy());
    }

    #[test]
    fn clear_then_resize_zeros_reused_bytes_and_preserves_capacity_semantics() {
        let mut values = resized(10000);
        for i in 0..values.len() {
            values[i].words = [u32::MAX; 5];
        }
        values.clear();
        assert!(values.prepare_resize(15000, Record::default()));
        values.commit_len(15000);
        for i in 0..values.len() {
            assert_eq!(values[i], Record::default());
        }
    }

    #[test]
    fn typed_owner_transfer_preserves_bytes_and_overlap_direction() {
        let mut values = resized(30000);
        for i in 0..values.len() {
            values[i].words[0] = i as u32;
        }
        values.copy_within(0..18000, 6000);
        for i in 0..18000 {
            assert_eq!(values[i + 6000].words[0], i as u32);
        }
        let mut words = values.cast::<[u32; 5]>();
        words.copy_within(6000..24000, 0);
        for i in 0..18000 {
            assert_eq!(words[i][0], i as u32);
        }
        assert!(words.checkpoint_all());
        assert!(words.healthy());
    }

    #[test]
    fn raw_callback_observation_is_sticky_and_independent_of_io_success() {
        let mut values = resized(70000);
        let (slice, handle) = values.as_mut_slice_with_checkpoint();
        assert!(!handle.observe_raw_span(0..slice.len()));
        for i in 0..slice.len() {
            assert!(handle.touch_index(i));
            slice[i].words[0] = i as u32;
        }
        assert!(handle.checkpoint_all());
        assert!(!values.bounded_access_certified());
        assert!(values.healthy());
        assert!(values.prepare_resize(140000, Record::default()));
        values.commit_len(140000);
        assert!(!values.bounded_access_certified());
    }

    #[test]
    fn finite_raw_callbacks_register_real_spans_without_changing_values() {
        let mut values = resized(50000);
        let (slice, handle) = values.as_mut_slice_with_checkpoint();
        let mut start = 0;
        while start < slice.len() {
            let end = (start + 3000).min(slice.len());
            assert!(handle.observe_raw_span(start..end));
            // 模拟原公开raw callback直接写其完整地址范围；没有偷偷补逐项touch。
            for (offset, value) in slice[start..end].iter_mut().enumerate() {
                value.words[0] = (start + offset) as u32;
            }
            assert!(handle.finish_callback());
            start = end;
        }
        assert!(values.bounded_access_certified());
        assert!(values.healthy());
        for i in 0..values.len() {
            assert_eq!(values[i].words[0], i as u32);
        }
    }

    #[test]
    fn cached_span_is_revoked_by_shared_checkpoint_and_permanent_poison() {
        let mut values = resized(5000);
        values[0].words[0] = 42;
        let state = &values.map.as_ref().unwrap().state;
        assert!(state.fast_contains(0, size_of::<Record>()));
        let epoch = state.epoch.get();
        let clock = state.clock.get();
        for _ in 0..10000 {
            assert_eq!(values[0].words[0], 42);
        }
        assert_eq!(state.clock.get(), clock);
        assert!(values.checkpoint_range(0..1));
        assert_ne!(state.epoch.get(), epoch);
        assert!(!state.fast_contains(0, size_of::<Record>()));
        assert_eq!(values[0].words[0], 42);
        assert!(state.fast_contains(0, size_of::<Record>()));
        assert!(!state.poison(libc::EIO));
        assert!(!state.fast_contains(0, size_of::<Record>()));
        assert_eq!(values[0].words[0], 42); // 仍可读旧值不等于恢复了提交权限。
        assert!(!state.fast_contains(0, size_of::<Record>()));
        assert!(!values.healthy());
    }

    #[test]
    fn finite_borrow_keeps_reverse_callback_order_mutations_and_early_stop() {
        let mut values = resized(50000);
        let capacity = MappedPodVec::<Record>::finite_chunk_capacity();
        let stop = 12345;
        let mut expected = values.len();
        let mut end = values.len();
        let mut stopped = false;
        while end > 0 {
            let start = end.saturating_sub(capacity);
            let result = values
                .with_finite_chunk_mut(start..end, |chunk| {
                    for (offset, value) in chunk.iter_mut().enumerate().rev() {
                        expected -= 1;
                        assert_eq!(start + offset, expected);
                        value.words[0] += 1;
                        if expected == stop {
                            return true;
                        }
                    }
                    false
                })
                .unwrap();
            if result {
                stopped = true;
                break;
            }
            end = start;
        }
        assert!(stopped);
        assert_eq!(expected, stop);
        for index in 0..values.len() {
            assert_eq!(values[index].words[0], u32::from(index >= stop));
        }
        assert!(values.healthy());
        assert!(values.bounded_access_certified());
    }

    #[test]
    fn invalid_or_poisoned_finite_borrow_never_calls_callback_or_recovers() {
        let mut oversized = resized(5000);
        let mut called = false;
        assert!(oversized
            .with_finite_chunk_mut(0..5000, |_| {
                called = true;
            })
            .is_none());
        assert!(!called);
        assert!(!oversized.healthy());
        assert!(oversized
            .with_finite_chunk_mut(0..1, |_| {
                called = true;
            })
            .is_none());
        let mut poisoned = resized(1);
        poisoned.map.as_ref().unwrap().state.poison(libc::EIO);
        assert!(poisoned
            .with_finite_chunk_mut(0..1, |_| {
                called = true;
            })
            .is_none());
        assert!(!called);
    }

    #[test]
    fn public_read_span_observes_real_extent_without_revoking_content_proof() {
        let values = resized(5000);
        let scope = Rc::new(());
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        values.observe_shared_span(0..1000);
        assert!(values.bounded_access_certified());
        assert_eq!(values.witness_range(&scope, &witness), Some(100..1000));
        // 真实大slice会永久撤有限访问认证，但既没有写权限也没有字节改变。
        values.observe_shared_span(0..5000);
        assert!(!values.bounded_access_certified());
        assert_eq!(values.witness_range(&scope, &witness), Some(100..1000));
        assert!(values.checkpoint_all());
        assert!(!values.bounded_access_certified());
    }

    #[test]
    fn read_witness_survives_reads_pageout_and_disjoint_append_only() {
        let mut values = resized(2000);
        let scope = Rc::new(());
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        assert_eq!(values.witness_range(&scope, &witness), Some(100..1000));
        assert!(values.witness_range(&Rc::new(()), &witness).is_none());
        let _ = values[500];
        assert!(values
            .with_finite_chunk(100..1000, |slice| slice.len())
            .is_some());
        assert!(values.checkpoint_all());
        assert!(values.prepare_resize(2100, Record::default()));
        values.commit_len(2100);
        values[2050].words[0] = 42;
        assert_eq!(values.witness_range(&scope, &witness), Some(100..1000));
        assert!(values.extend_read_witness(&scope, &witness, 1000..1100));
        assert_eq!(values.witness_range(&scope, &witness), Some(100..1100));
        values[500].words[0] = 1;
        assert!(values.witness_range(&scope, &witness).is_none());
        assert!(values.healthy());
    }

    #[test]
    fn every_mutable_range_form_and_whole_escape_revokes_before_authorization() {
        let producers: [fn(&mut MappedPodVec<Record>); 10] = [
            |v| {
                let _ = &mut v[500..600];
            },
            |v| {
                let _ = &mut v[500..=600];
            },
            |v| {
                let _ = &mut v[..600];
            },
            |v| {
                let _ = &mut v[..=600];
            },
            |v| {
                let _ = &mut v[500..];
            },
            |v| {
                let _ = &mut v[..];
            },
            |v| {
                let _ = v.as_mut_slice();
            },
            |v| {
                let _ = v.as_mut_slice_with_checkpoint();
            },
            |v| {
                let _ = v.get_mut(500);
            },
            |v| {
                v.copy_within(1200..1300, 500);
            },
        ];
        for producer in producers {
            let mut values = resized(2000);
            let scope = Rc::new(());
            let witness = values.read_witness(&scope, 100..1000).unwrap();
            producer(&mut values);
            assert!(values.witness_range(&scope, &witness).is_none());
        }
        let mut values = resized(2000);
        let scope = Rc::new(());
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        values
            .with_finite_chunk_mut(500..501, |_| assert!(!witness.ticket.valid.get()))
            .unwrap();
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        assert!(values.for_each_chunk_mut(500..600, |_| assert!(!witness.ticket.valid.get())));
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        values[1500..1600].fill(Record::default());
        assert_eq!(values.witness_range(&scope, &witness), Some(100..1000));
    }

    #[test]
    fn raw_reset_cast_growth_poison_and_owner_drop_revoke_read_witnesses() {
        let mut values = resized(2000);
        let scope = Rc::new(());
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        let mapping = values.map.as_ref().unwrap();
        let handle = CheckpointHandle {
            state: &mapping.state,
            stride: size_of::<Record>(),
            len: values.len(),
        };
        handle.observe_raw_span(1500..1600);
        assert!(values.witness_range(&scope, &witness).is_none());
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        assert!(values.prepare_resize(100000, Record::default()));
        values.commit_len(100000);
        assert!(values.witness_range(&scope, &witness).is_none());
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        let values = values.cast::<[u32; 5]>();
        assert!(values.witness_range(&scope, &witness).is_none());
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        drop(values);
        assert!(witness.ticket.owner.upgrade().is_none());
        let mut values = resized(2000);
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        values.clear();
        assert!(!witness.ticket.valid.get());
        let mut values = resized(2000);
        let witness = values.read_witness(&scope, 100..1000).unwrap();
        values.map.as_ref().unwrap().state.poison(libc::EIO);
        assert!(!witness.ticket.valid.get());
        assert!(values.witness_range(&scope, &witness).is_none());
        assert!(!values.prepare_resize(usize::MAX, Record::default()));
    }

    #[test]
    fn fixed_witness_slots_evict_safely_and_do_not_keep_scope_alive() {
        let values = resized(2000);
        let scope = Rc::new(());
        let first = values.read_witness(&scope, 100..200).unwrap();
        let mut retained = Vec::new();
        for index in 0..WINDOWS {
            retained.push(
                values
                    .read_witness(&scope, 300 + index..400 + index)
                    .unwrap(),
            );
        }
        assert!(values.witness_range(&scope, &first).is_none());
        assert_eq!(retained.len(), WINDOWS);
        let last = retained.pop().unwrap();
        drop(scope);
        assert!(last.ticket.scope.upgrade().is_none());
    }

    #[test]
    fn failed_old_owner_cleanup_cannot_publish_successful_growth() {
        // 实际超过工作块才有FD；故障必须发生于原文件增长后的真实close。
        let mut values = resized(5000);
        let scope = values.paging_scope();
        let observer = values.resource_observer();
        scope.test_fail_next_close_after_real();
        assert!(!values.prepare_resize(100000, Record::default()));
        assert_eq!(values.len(), 5000); // 容量已实际映射，逻辑长度仍不能假提交。
        assert!(!values.healthy());
        drop(values);
        let ended = observer.snapshot();
        assert_eq!(
            (ended.files_open, ended.mappings_open, ended.mapped_bytes),
            (0, 0, 0)
        );
        assert!(ended.cleanup_failed);
        assert_eq!(
            ended.first_error,
            Some(PagingIoError {
                operation: PagingOperation::Close,
                errno: libc::EIO
            })
        );
    }

    #[test]
    fn a_temporary_scope_io_error_permanently_revokes_the_original_owner() {
        // 本负控验证真实文件分页首错，保持实际溢出而不是给小工作块伪造FD。
        let mut values = resized(5000);
        let scope = values.paging_scope();
        let observer = values.resource_observer();
        // 仅构造错误调用的页状态，不把非法地址登记为一个成功mmap，也不解引用它。
        let bad = PageState {
            base: NonNull::dangling(),
            bytes: 16384,
            page: Some(16384),
            poison: Cell::new(None),
            scope,
            mutation: Rc::new(MutationDomain::new()),
            epoch: Cell::new(0),
            fast_span: Cell::new(ResidentSpan::EMPTY),
            bounded_access: Cell::new(true),
            windows: Cell::new([None; WINDOWS]),
            clock: Cell::new(0),
            last_bucket: Cell::new(usize::MAX),
        };
        assert!(!bad.checkpoint(0..1)); // 实际msync因非页对齐地址失败。
        drop(bad); // 临时页结束后，原owner不能丢失同scope首错误再继续发布结果。
        assert!(!values.healthy());
        assert!(!values.prepare_resize(1000, Record::default()));
        assert_eq!(observer.snapshot().files_open, 1);
        drop(values);
        let ended = observer.snapshot();
        assert_eq!(
            (ended.files_open, ended.mappings_open, ended.mapped_bytes),
            (0, 0, 0)
        );
        assert_eq!(
            ended.first_error,
            Some(PagingIoError {
                operation: PagingOperation::Sync,
                errno: libc::EINVAL
            })
        );
        assert!(!ended.cleanup_failed); // 工作IO失败不会伪造实际unmap/close也失败。
    }

    #[test]
    fn actual_msync_error_permanently_poison_even_future_empty_checkpoint() {
        // 只向公开msync提交非页对齐的非法地址；内核必须EINVAL，绝不解引用该地址。
        let state = PageState {
            base: NonNull::dangling(),
            bytes: 16384,
            page: Some(16384),
            poison: Cell::new(None),
            scope: PagingScope::new(),
            mutation: Rc::new(MutationDomain::new()),
            epoch: Cell::new(0),
            fast_span: Cell::new(ResidentSpan::EMPTY),
            bounded_access: Cell::new(true),
            windows: Cell::new([None; WINDOWS]),
            clock: Cell::new(0),
            last_bucket: Cell::new(usize::MAX),
        };
        assert!(!state.checkpoint(0..1));
        assert_eq!(state.poison.get(), Some(libc::EINVAL));
        assert_eq!(
            state.scope.error(),
            Some(PagingIoError {
                operation: PagingOperation::Sync,
                errno: libc::EINVAL
            })
        );
        assert!(!state.checkpoint(0..0));
        state.touch(0, 1);
        assert_eq!(state.poison.get(), Some(libc::EINVAL));
    }

    #[test]
    fn capacity_overflow_cannot_publish_prepared_length_or_recover_after_clear() {
        let mut values = resized(1);
        values[0].words[0] = 42;
        assert!(!values.prepare_resize(usize::MAX, Record::default()));
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].words[0], 42);
        assert!(!values.healthy());
        values.clear();
        assert!(!values.prepare_resize(1, Record::default()));
    }
}
