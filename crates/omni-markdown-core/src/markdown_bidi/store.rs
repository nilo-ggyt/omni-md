//! 匿名元数据文件与全表共享的固定页缓存；正文从不进入此文件。

use std::fs::File;
use std::os::fd::IntoRawFd;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use super::resource::ResourceState;
use super::{BidiError, BidiMemoryBudget, MarkdownBidiCancellation, MarkdownBidiResourceObserver};

pub(super) const PAGE_BYTES: usize = 16 * 1024;
pub(super) const RECORD_BYTES: u64 = 32;
pub(super) const NONE: u32 = u32::MAX;

#[derive(Clone, Copy)]
pub(super) enum Table {
    Text = 0,
    Run = 1,
    Sequence = 2,
    Stack = 3,
    Paragraph = 4,
}

struct Page {
    number: u64,
    bytes: PageBytes,
    dirty: bool,
    used: u64,
}

/// 先真实drop固定Box，再更新收据；Store析构顺序不会把仍存活的页报成已释放。
struct PageBytes {
    bytes: Option<Box<[u8; PAGE_BYTES]>>,
    resources: Arc<ResourceState>,
}
impl PageBytes {
    fn new(resources: &Arc<ResourceState>) -> Self {
        let bytes = Box::new([0; PAGE_BYTES]);
        resources.page_allocated(PAGE_BYTES);
        Self {
            bytes: Some(bytes),
            resources: Arc::clone(resources),
        }
    }
}
impl std::ops::Deref for PageBytes {
    type Target = [u8; PAGE_BYTES];
    fn deref(&self) -> &Self::Target {
        self.bytes
            .as_deref()
            .expect("存活的双向文本页必须拥有原缓存")
    }
}
impl std::ops::DerefMut for PageBytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.bytes
            .as_deref_mut()
            .expect("存活的双向文本页必须拥有原缓存")
    }
}
impl Drop for PageBytes {
    fn drop(&mut self) {
        if let Some(bytes) = self.bytes.take() {
            drop(bytes);
            self.resources.page_deallocated(PAGE_BYTES);
        }
    }
}

/// 正常路径只转发公开positioned IO；测试故障也先执行真实前缀IO，绝不替算法计算结果。
struct Io {
    file: Option<File>,
    resources: Arc<ResourceState>,
    #[cfg(test)]
    fault: Option<IoFault>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum IoFault {
    PartialRead(usize),
    PartialWrite(usize),
    SetLength,
}

impl Io {
    #[cfg(test)]
    fn new(file: File) -> Self {
        Self::with_resources(file, ResourceState::new())
    }

    fn with_resources(file: File, resources: Arc<ResourceState>) -> Self {
        resources.file_acquired(); // 只有调用方已真正取得File所有权，才把原scope记为存在FD。
        Self {
            file: Some(file),
            resources,
            #[cfg(test)]
            fault: None,
        }
    }

    fn file(&self) -> &File {
        self.file.as_ref().expect("live metadata IO owns one file")
    }

    #[cfg(test)]
    fn observer(&self) -> FileObserver {
        self.resources.observe()
    }

    fn set_len(&mut self, length: u64) -> std::io::Result<()> {
        self.file().set_len(length)?;
        #[cfg(test)]
        if matches!(self.fault, Some(IoFault::SetLength)) {
            self.fault = None;
            return Err(std::io::Error::other("synthetic metadata set_len failure"));
        }
        Ok(())
    }

    fn read_page(&mut self, destination: &mut [u8], offset: u64) -> std::io::Result<()> {
        #[cfg(test)]
        if let Some(IoFault::PartialRead(length)) = self.fault {
            self.fault = None;
            let length = length.min(destination.len());
            self.file()
                .read_exact_at(&mut destination[..length], offset)?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "synthetic metadata partial read",
            ));
        }
        self.file().read_exact_at(destination, offset)
    }

    fn write_page(&mut self, source: &[u8], offset: u64) -> std::io::Result<()> {
        #[cfg(test)]
        if let Some(IoFault::PartialWrite(length)) = self.fault {
            self.fault = None;
            self.file()
                .write_all_at(&source[..length.min(source.len())], offset)?;
            return Err(std::io::Error::other("synthetic metadata partial write"));
        }
        self.file().write_all_at(source, offset)
    }
}

impl Drop for Io {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let fd = file.into_raw_fd();
            // SAFETY: 此处移出唯一匿名FD所有者，只close一次；没有映射或外部借用依赖该descriptor。
            let result = unsafe { libc::close(fd) };
            let status = if result == 0 {
                1
            } else {
                // 紧邻真实close读取errno，任何其他系统调用都不能覆盖它；失败不猜FD已经关闭。
                -std::io::Error::last_os_error()
                    .raw_os_error()
                    .filter(|errno| *errno > 0)
                    .unwrap_or(libc::EIO)
            };
            self.resources.close_returned(status);
        }
    }
}

#[cfg(test)]
pub(super) type FileObserver = MarkdownBidiResourceObserver;

/// 所有表共用同一组页，不因阶段或表数量放大预算。文件只存类别、层级与坐标。
pub(super) struct Store {
    io: Io,
    pages: Vec<Page>,
    bases: [u64; 5],
    capacity: u32,
    clock: u64,
    pub(super) page_loads: u64,
    poisoned: bool,
    cancellation: MarkdownBidiCancellation,
}

impl Store {
    #[cfg(test)]
    pub(super) fn new(capacity: u32, budget: BidiMemoryBudget) -> Result<Self, BidiError> {
        Self::new_cancellable(capacity, budget, MarkdownBidiCancellation::new())
    }

    #[cfg(test)]
    pub(super) fn new_cancellable(
        capacity: u32,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        Self::new_cancellable_observed(capacity, budget, cancellation, None)
    }

    #[cfg(test)]
    pub(super) fn new_cancellable_observed(
        capacity: u32,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: Option<&Arc<ResourceState>>,
    ) -> Result<Self, BidiError> {
        cancellation.check()?; // 在建立匿名FD之前拒绝已经取消的任务。
        let file = tempfile::tempfile()?;
        let io = match resources {
            Some(resources) => Io::with_resources(file, Arc::clone(resources)),
            None => Io::new(file), // 旧入口仍仅在确实创建Paged存储时分配收据，Uniform没有额外分配。
        };
        Self::with_io_cancellable(capacity, budget, io, cancellation)
    }

    #[cfg(test)]
    fn with_io(capacity: u32, budget: BidiMemoryBudget, io: Io) -> Result<Self, BidiError> {
        Self::with_io_cancellable(capacity, budget, io, MarkdownBidiCancellation::new())
    }

    #[cfg(test)]
    fn with_io_cancellable(
        capacity: u32,
        budget: BidiMemoryBudget,
        io: Io,
        cancellation: MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        let mut store = Self::with_io_empty(capacity, budget, io, cancellation)?;
        for _ in 0..budget.pages {
            store.allocate_prepared_page()?;
        }
        Ok(store)
    }

    /// 只有Prepare内部持有未配满固定页的候选；在全部页初始化前没有对外Store或Ready。
    pub(super) fn new_prepared_empty(
        capacity: u32,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: &Arc<ResourceState>,
    ) -> Result<Self, BidiError> {
        cancellation.check()?;
        let file = tempfile::tempfile()?;
        Self::with_io_empty(
            capacity,
            budget,
            Io::with_resources(file, Arc::clone(resources)),
            cancellation,
        )
    }

    pub(super) fn allocate_prepared_page(&mut self) -> Result<(), BidiError> {
        self.cancellation.check()?;
        self.pages.push(Page {
            number: u64::MAX,
            bytes: PageBytes::new(&self.io.resources),
            dirty: false,
            used: 0,
        });
        #[cfg(test)]
        if self.pages.len() == 1 {
            self.cancellation.first_page_allocated(self.io.observer());
        }
        self.cancellation.check() // 原真实页分配后的取消点仍在，失败析构不发布候选。
    }

    pub(super) fn prepared_page_count(&self) -> usize {
        self.pages.len()
    }

    pub(super) fn prepared_page_dirty(&self, index: usize) -> bool {
        self.pages.get(index).is_some_and(|page| page.dirty)
    }

    pub(super) fn flush_prepared_page(&mut self, index: usize) -> Result<(), BidiError> {
        self.healthy()?;
        let page = self.pages.get_mut(index).ok_or(BidiError::InvalidBudget)?;
        if page.dirty
            && let Err(error) = self
                .io
                .write_page(page.bytes.as_slice(), page.number * PAGE_BYTES as u64)
        {
            self.poisoned = true;
            return Err(error.into());
        }
        page.dirty = false;
        self.healthy()
    }

    fn with_io_empty(
        capacity: u32,
        budget: BidiMemoryBudget,
        mut io: Io,
        cancellation: MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        cancellation.check()?; // FD创建期间收到取消时，局部Io在返回前真实释放。
        let mut bases = [0; 5];
        let extent = u64::from(capacity)
            .checked_add(2)
            .and_then(|value| value.checked_mul(RECORD_BYTES))
            .ok_or(BidiError::CoordinateOverflow)?;
        // 表之间留整页边界；set_len只建立稀疏文件长度，不分配正文或全表内存。
        let stride = extent
            .checked_add(PAGE_BYTES as u64 - 1)
            .map(|value| value / PAGE_BYTES as u64 * PAGE_BYTES as u64)
            .ok_or(BidiError::CoordinateOverflow)?;
        for (index, base) in bases.iter_mut().enumerate() {
            *base = stride
                .checked_mul(index as u64)
                .ok_or(BidiError::CoordinateOverflow)?;
        }
        // 构造失败前尚未存在可查询Store；IO owner在?失败时直接归还匿名fd，不发布部分对象。
        io.set_len(
            stride
                .checked_mul(bases.len() as u64)
                .ok_or(BidiError::CoordinateOverflow)?,
        )?;
        cancellation.check()?; // set_len错误先传播；只有真实IO成功后才检查随后取消。
        let pages = Vec::with_capacity(usize::from(budget.pages));
        // 页数由构造时已经验证的硬预算决定；后续miss只能复用这些真实buffer。

        Ok(Self {
            io,
            pages,
            bases,
            capacity,
            clock: 0,
            page_loads: 0,
            poisoned: false,
            cancellation,
        })
    }

    pub(super) fn healthy(&self) -> Result<(), BidiError> {
        if self.poisoned {
            Err(BidiError::PoisonedMetadata)
        } else {
            self.cancellation.check() // 既存metadata I/O poison保首因；取消不允许把任何cache命中重新授权。
        }
    }

    fn offset(&self, table: Table, id: u32) -> Result<u64, BidiError> {
        if id > self.capacity {
            return Err(BidiError::CorruptMetadata); // 错误ID不能跨越表边界访问另一个有效record。
        }
        self.bases[table as usize]
            .checked_add(u64::from(id) * RECORD_BYTES)
            .ok_or(BidiError::CoordinateOverflow)
    }

    fn page(&mut self, number: u64) -> Result<usize, BidiError> {
        self.healthy()?; // poison连cache hit也拒绝，不能通过仍在RAM的片段绕过全表撤权。
        self.clock = self
            .clock
            .checked_add(1)
            .ok_or(BidiError::CoordinateOverflow)?;
        if let Some(index) = self.pages.iter().position(|page| page.number == number) {
            self.pages[index].used = self.clock;
            self.healthy()?;
            return Ok(index); // hit不读文件，也不创建第二份页。
        }
        let index = self
            .pages
            .iter()
            .enumerate()
            .min_by_key(|(_, page)| page.used)
            .map(|(index, _)| index)
            .ok_or(BidiError::InvalidBudget)?;
        let page = &mut self.pages[index];
        if page.dirty
            && let Err(error) = self
                .io
                .write_page(page.bytes.as_slice(), page.number * PAGE_BYTES as u64)
        {
            self.poisoned = true;
            return Err(error.into());
        }
        self.cancellation.check()?;
        // 开始覆盖buffer之前先撤销旧身份；即使短读只改了前缀，也不再有可命中的旧页。
        page.number = u64::MAX;
        page.dirty = false;
        // 匿名文件已设真实逻辑长度；未写稀疏页也必须成功读满，短读不能被当成零元数据。
        if let Err(error) = self
            .io
            .read_page(page.bytes.as_mut_slice(), number * PAGE_BYTES as u64)
        {
            self.poisoned = true;
            return Err(error.into());
        }
        self.io.resources.page_loaded();
        self.cancellation.check()?; // 已完成真实IO，但取消后仍不发布新的页身份。
        page.number = number;
        page.used = self.clock;
        page.dirty = false;
        self.page_loads += 1;
        Ok(index)
    }

    pub(super) fn read(&mut self, table: Table, id: u32) -> Result<[u8; 32], BidiError> {
        self.healthy()?;
        let offset = self.offset(table, id)?;
        let slot = self.page(offset / PAGE_BYTES as u64)?;
        let from = (offset % PAGE_BYTES as u64) as usize;
        let mut value = [0; 32];
        // record和页长整除，不跨页；只复制32B元数据，借用在返回前结束。
        value.copy_from_slice(&self.pages[slot].bytes[from..from + 32]);
        self.healthy()?;
        Ok(value)
    }

    pub(super) fn write(
        &mut self,
        table: Table,
        id: u32,
        value: [u8; 32],
    ) -> Result<(), BidiError> {
        self.healthy()?;
        let offset = self.offset(table, id)?;
        let slot = self.page(offset / PAGE_BYTES as u64)?;
        let from = (offset % PAGE_BYTES as u64) as usize;
        self.pages[slot].bytes[from..from + 32].copy_from_slice(&value);
        self.pages[slot].dirty = true;
        self.healthy()
    }

    #[cfg(test)]
    pub(super) fn flush(&mut self) -> Result<(), BidiError> {
        self.healthy()?;
        // 同一个页写入实现；Prepare另在每个dirty页之前消费真实资源操作预算。
        for index in 0..self.pages.len() {
            self.flush_prepared_page(index)?;
        }
        self.healthy()
    }

    pub(super) fn cache_bytes(&self) -> usize {
        // 这是已分配页payload的真实大小，不包含小型页目录、Rust栈或进程总footprint。
        self.pages.len() * PAGE_BYTES
    }

    pub(super) fn resource_observer(&self) -> MarkdownBidiResourceObserver {
        self.io.resources.observe()
    }

    #[cfg(test)]
    pub(super) fn fail_next_read_after_prefix(&mut self, bytes: usize) {
        self.io.fault = Some(IoFault::PartialRead(bytes));
    }

    #[cfg(all(test, feature = "paged-layout-breaks"))]
    pub(super) fn fail_next_write_after_prefix(&mut self, bytes: usize) {
        self.io.fault = Some(IoFault::PartialWrite(bytes));
    }

    #[cfg(all(test, feature = "paged-layout-breaks"))]
    pub(super) fn io_observer_for_test(&self) -> FileObserver {
        self.io.observer()
    }

    #[cfg(test)]
    pub(super) fn raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.io.file().as_raw_fd()
    }
}

pub(super) fn read_u32(bytes: &[u8; 32], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("固定32B元数据字段"),
    )
}

pub(super) fn write_u32(bytes: &mut [u8; 32], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// 初始FSI与IRS暂停栈可深于显式level上限；其条目同样分页，不能误用125项截断。
pub(super) struct DiskStack {
    depth: u32,
}

impl DiskStack {
    pub(super) fn new() -> Self {
        Self { depth: 0 }
    }

    pub(super) fn len(&self) -> u32 {
        self.depth
    }

    pub(super) fn push(
        &mut self,
        store: &mut Store,
        first: u32,
        last: u32,
    ) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        write_u32(&mut bytes, 0, first);
        write_u32(&mut bytes, 4, last);
        store.write(Table::Stack, self.depth, bytes)?;
        self.depth = self
            .depth
            .checked_add(1)
            .ok_or(BidiError::CoordinateOverflow)?;
        Ok(())
    }

    pub(super) fn top(&self, store: &mut Store) -> Result<Option<(u32, u32)>, BidiError> {
        let Some(index) = self.depth.checked_sub(1) else {
            return Ok(None); // 空栈是合法段首/未匹配PDI，不读取不存在的record。
        };
        let bytes = store.read(Table::Stack, index)?;
        Ok(Some((read_u32(&bytes, 0), read_u32(&bytes, 4))))
    }

    pub(super) fn pop(&mut self, store: &mut Store) -> Result<Option<(u32, u32)>, BidiError> {
        let result = self.top(store)?;
        if result.is_some() {
            self.depth -= 1;
        }
        Ok(result)
    }

    pub(super) fn clear(&mut self) {
        self.depth = 0; // 旧槽只含元数据，后继push覆盖；没有堆容量或源owner留在栈中。
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_store() -> Store {
        Store::new(4096, BidiMemoryBudget::new(4 * PAGE_BYTES).unwrap()).unwrap()
    }

    #[test]
    fn production_receipt_waits_for_the_actual_fd_and_each_fixed_page_to_be_released() {
        let mut store = small_store();
        let observer = store.resource_observer();
        let live = observer.snapshot();
        assert!(live.file_created && live.file_open && !live.close_succeeded);
        assert_eq!((live.cache_bytes, live.peak_cache_bytes), (65536, 65536));
        store.write(Table::Text, 0, [7; 32]).unwrap();
        store.cancellation.cancel();
        assert!(!observer.is_released()); // 取消只撤权限，活的原文件和页不能被报为已经归还。
        drop(store);
        let actual = observer.snapshot();
        assert!(actual.file_created && !actual.file_open && actual.close_succeeded);
        assert_eq!(actual.close_errno, None);
        assert_eq!((actual.cache_bytes, actual.peak_cache_bytes), (0, 65536));
        assert!(observer.is_released());
    }

    fn dirty_four_pages(store: &mut Store) {
        // 各record相隔一个真实页；第一次miss后的四个页都含可辨认数据，下一miss必然淘汰旧页。
        for page in 0..4 {
            store
                .write(Table::Text, page * 512, [0xc0 + page as u8; 32])
                .unwrap();
        }
    }

    #[test]
    fn cancellation_rejects_every_cached_operation_without_hiding_old_io_poison() {
        let mut store = small_store();
        store.write(Table::Text, 0, [7; 32]).unwrap();
        let observer = store.io.observer();
        store.cancellation.cancel();
        assert!(matches!(store.healthy(), Err(BidiError::Cancelled)));
        assert!(matches!(
            store.read(Table::Text, 0),
            Err(BidiError::Cancelled)
        ));
        assert!(matches!(
            store.write(Table::Text, 0, [9; 32]),
            Err(BidiError::Cancelled)
        ));
        assert!(matches!(store.flush(), Err(BidiError::Cancelled)));
        assert!(!observer.closed()); // 撤权不等于资源析构，原Store仍活着必须保留其真实FD。
        drop(store);
        assert!(observer.closed());

        let mut store = small_store();
        store.write(Table::Text, 0, [7; 32]).unwrap();
        store.io.fault = Some(IoFault::PartialWrite(1));
        assert!(matches!(store.flush(), Err(BidiError::Storage(_))));
        store.cancellation.cancel();
        assert!(matches!(store.healthy(), Err(BidiError::PoisonedMetadata)));
    }

    #[test]
    fn partial_read_revokes_old_page_and_poison_rejects_even_untouched_cache_hits() {
        let mut store = small_store();
        dirty_four_pages(&mut store);
        store.flush().unwrap();
        store.io.fault = Some(IoFault::PartialRead(8));
        assert!(matches!(
            store.read(Table::Text, 2048),
            Err(BidiError::Storage(_))
        ));
        let invalid = store
            .pages
            .iter()
            .find(|page| page.number == u64::MAX)
            .unwrap();
        assert_eq!(&invalid.bytes[..8], &[0; 8]);
        assert_eq!(&invalid.bytes[8..32], &[0xc0; 24]); // 真正覆盖了前缀，而非只返回一个合成Err。
        assert!(matches!(
            store.read(Table::Text, 0),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(
            store.read(Table::Text, 512),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(
            store.write(Table::Text, 512, [0; 32]),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(store.flush(), Err(BidiError::PoisonedMetadata)));
    }

    #[test]
    fn partial_eviction_write_poison_rejects_cached_data_and_preserves_real_io_evidence() {
        let mut store = small_store();
        dirty_four_pages(&mut store);
        store.io.fault = Some(IoFault::PartialWrite(8));
        assert!(matches!(
            store.read(Table::Text, 2048),
            Err(BidiError::Storage(_))
        ));
        let mut actual = [0; 32];
        store.io.file().read_exact_at(&mut actual, 0).unwrap();
        assert_eq!(&actual[..8], &[0xc0; 8]);
        assert_eq!(&actual[8..], &[0; 24]); // 仅诊断私有元数据的真实部分落盘，不读任何源正文。
        assert!(matches!(
            store.read(Table::Text, 512),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(store.flush(), Err(BidiError::PoisonedMetadata)));
    }

    #[test]
    fn failed_final_flush_permanently_revokes_every_following_access() {
        let mut store = small_store();
        store.write(Table::Text, 0, [0x77; 32]).unwrap();
        store.io.fault = Some(IoFault::PartialWrite(4));
        assert!(matches!(store.flush(), Err(BidiError::Storage(_))));
        assert!(matches!(
            store.read(Table::Text, 0),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(
            store.write(Table::Text, 0, [0; 32]),
            Err(BidiError::PoisonedMetadata)
        ));
    }

    #[test]
    fn successful_eviction_never_grows_the_real_page_payload_or_page_directory() {
        let mut store = small_store();
        let directory = store.pages.as_ptr();
        let capacity = store.pages.capacity();
        for index in 0..=4096 {
            store.write(Table::Text, index, [index as u8; 32]).unwrap();
        }
        store.flush().unwrap();
        assert!(store.page_loads > 4);
        assert_eq!(store.pages.as_ptr(), directory);
        assert_eq!(store.pages.capacity(), capacity);
        assert_eq!(store.pages.len(), 4);
        assert_eq!(store.cache_bytes(), 65536);
    }

    #[test]
    fn failed_length_initialization_returns_no_store_and_closes_its_actual_descriptor() {
        const CHILD: &str = "OMNI_BIDI_INIT_IO_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "markdown_bidi::store::tests::failed_length_initialization_returns_no_store_and_closes_its_actual_descriptor", "--nocapture"])
                .env(CHILD, "1")
                .status().unwrap();
            assert!(status.success());
            return;
        }
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().unwrap();
        let fd = file.as_raw_fd();
        let mut io = Io::new(file);
        let resources = io.resources.observe();
        io.fault = Some(IoFault::SetLength);
        assert!(matches!(
            Store::with_io(8, BidiMemoryBudget::default(), io),
            Err(BidiError::Storage(_))
        ));
        // SAFETY: 子进程只有此测试，错误构造后没有新建fd；F_GETFD安全观察实际关闭，不访问已析构Rust对象。
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
        let actual = resources.snapshot();
        assert!(actual.file_created && actual.close_succeeded && resources.is_released());
        assert_eq!((actual.cache_bytes, actual.peak_cache_bytes), (0, 0));
    }
}
