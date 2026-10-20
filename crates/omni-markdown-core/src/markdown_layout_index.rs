//! 固定record的匿名分页IO；只持一份16KiB写buffer，不理解字素、断行或正文语义。
use std::{
    fs::File,
    os::fd::IntoRawFd,
    os::unix::fs::FileExt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// 每个独立算法索引仅有一个16KiB工作区；不创建全局缓存或跨算法共享文件。
pub const MARKDOWN_LAYOUT_INDEX_BYTES: usize = 16 * 1024;
const PAGE: usize = MARKDOWN_LAYOUT_INDEX_BYTES;
use std::io;

#[cfg(feature = "paged-line-compose")]
mod reader;
#[cfg(feature = "paged-line-compose")]
pub(crate) use reader::LayoutIndexReader;
#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use prefix::LayoutIndexSnapshotError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum LayoutIndexError {
    #[error("布局索引record或坐标无效")]
    InvalidRecord,
    #[error("布局索引坐标或计数溢出")]
    Overflow,
    #[error("布局索引已永久撤权")]
    Poisoned,
    #[error("布局索引IO失败")]
    Io(#[from] io::Error),
}
type Error = LayoutIndexError;
type Result<T> = std::result::Result<T, Error>;

#[derive(Default)]
struct ResourceState {
    #[cfg(feature = "paged-shaping-frontier")]
    producer_failed: AtomicBool,
    #[cfg(feature = "paged-shaping-frontier")]
    prefix_readers: AtomicUsize,
    file_open: AtomicBool,
    buffer: AtomicUsize,
    peak_buffer: AtomicUsize,
    #[cfg(any(feature = "paged-graphemes", feature = "paged-shaping"))]
    owner_closed: AtomicBool,
    // 真实写入或FD归还失败都不能被后来close洗掉；它不声称内核close必然失败。
    cleanup_failed: AtomicBool,
    #[cfg(feature = "paged-line-compose")]
    reader_failed: AtomicBool,
}

/// 原追加页只在RAM保留一份；flushed之前的字节已写入同inode且永不回写。
struct Page<const N: usize> {
    bytes: Option<Box<[u8; PAGE]>>,
    used: usize,
    flushed: usize,
    resources: Arc<ResourceState>,
}
impl<const N: usize> Page<N> {
    fn new(resources: Arc<ResourceState>) -> Self {
        let bytes = Box::new([0; PAGE]);
        resources.buffer.store(PAGE, Ordering::Release);
        resources.peak_buffer.fetch_max(PAGE, Ordering::AcqRel);
        Self {
            bytes: Some(bytes),
            used: 0,
            flushed: 0,
            resources,
        }
    }

    fn read(&self, offset: usize) -> Result<[u8; N]> {
        let end = offset
            .checked_add(N)
            .filter(|&end| end <= self.used)
            .ok_or(Error::InvalidRecord)?;
        let mut record = [0; N];
        record.copy_from_slice(&self.bytes.as_ref().ok_or(Error::Poisoned)?[offset..end]);
        Ok(record)
    }

    fn write(&mut self, offset: usize, record: [u8; N]) -> Result<()> {
        let end = offset
            .checked_add(N)
            .filter(|&end| end <= PAGE)
            .ok_or(Error::InvalidRecord)?;
        if offset != self.used {
            return Err(Error::InvalidRecord);
        }
        self.bytes.as_mut().ok_or(Error::Poisoned)?[offset..end].copy_from_slice(&record);
        self.used = end;
        Ok(())
    }

    /// 只写新增脏后缀；即使半页已发布，后继也不回写旧Reader可读的任何字节。
    fn flush_through(&mut self, file: &File, page_start: usize, through: usize) -> Result<()> {
        let through = through.min(self.used);
        if through <= self.flushed {
            return Ok(());
        }
        let offset = page_start
            .checked_add(self.flushed)
            .ok_or(Error::Overflow)?;
        let offset = u64::try_from(offset).map_err(|_| Error::Overflow)?;
        let bytes = self.bytes.as_ref().ok_or(Error::Poisoned)?;
        // write_all_at沿原有限区间处理短写/EINTR；部分成功后错误仍撤整个候选，不能补签prefix。
        if let Err(error) = file.write_all_at(&bytes[self.flushed..through], offset) {
            self.resources.cleanup_failed.store(true, Ordering::Release);
            return Err(error.into());
        }
        self.flushed = through;
        Ok(())
    }

    /// 换页只在旧页完整flush后复用原分配；旧内容不进入新的有效区间。
    fn reset(&mut self) {
        self.used = 0;
        self.flushed = 0;
    }
}
impl<const N: usize> Drop for Page<N> {
    fn drop(&mut self) {
        // 未发布尾部没有消费者，直接归还；先真实drop唯一buffer再发布资源归零。
        drop(self.bytes.take());
        self.resources.buffer.store(0, Ordering::Release);
    }
}

/// 读同一固定区间；构建器与只读Reader共用短读/EINTR协议，不额外建立映射或文件游标。
fn read_exact_at(
    output: &mut [u8],
    offset: u64,
    mut read: impl FnMut(&mut [u8], u64) -> io::Result<usize>,
) -> Result<()> {
    if output.len() > PAGE {
        return Err(Error::InvalidRecord);
    } // 辅助入口也保留真实16KiB上界，不能由后续调用者扩大为整索引读取。
    offset
        .checked_add(u64::try_from(output.len()).map_err(|_| Error::Overflow)?)
        .ok_or(Error::Overflow)?;
    let mut complete = 0usize;
    while complete < output.len() {
        let position = offset
            .checked_add(u64::try_from(complete).map_err(|_| Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        match read(&mut output[complete..], position) {
            Ok(0) => {
                // 已写入的栈前缀不代表完整record；真实EOF必须保留为本次失败。
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "layout index ended before the requested records",
                )
                .into());
            }
            Ok(bytes) => {
                complete = complete.checked_add(bytes).ok_or(Error::Overflow)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                // EINTR未完成这一有限剩余区间，只重试原坐标，不重建候选、页或权限。
            }
            Err(error) => return Err(error.into()), // 首个真实errno原样交给owner永久撤权。
        }
    }
    Ok(())
}

/// 原唯一FD的close义务共用同一出口，不为只读移交复制File或猜测重试时机。
fn close_file(file: File, resources: &ResourceState) -> Result<()> {
    let raw = file.into_raw_fd();
    // SAFETY: into_raw_fd转移唯一close义务；无第二File会重复关闭或复用此descriptor。
    if unsafe { libc::close(raw) } == 0 {
        resources.file_open.store(false, Ordering::Release);
        Ok(())
    } else {
        // close失败时不假设内核已归还，也不重试可能已被系统复用的descriptor。
        resources.cleanup_failed.store(true, Ordering::Release);
        Err(io::Error::last_os_error().into())
    }
}

pub(crate) struct PagedLayoutIndex<const N: usize> {
    page: Option<Page<N>>,
    file: Option<File>,
    number: usize,
    count: usize,
    loads: u64,
    sealed: bool,
    poisoned: bool,
    resources: Arc<ResourceState>,
}
impl<const N: usize> PagedLayoutIndex<N> {
    pub(crate) fn new() -> Result<Self> {
        // record不跨固定工作区边界；这里已无系统映射页对齐或磁盘预分配要求。
        if N == 0 || N > PAGE || !PAGE.is_multiple_of(N) {
            return Err(Error::InvalidRecord);
        }
        let resources = Arc::new(ResourceState::default());
        Ok(Self {
            page: None,
            file: None, // 私有单页始终由唯一buffer保存；没有实际File消费者就不创建匿名FD。
            number: 0,
            count: 0,
            loads: 0,
            sealed: false,
            poisoned: false,
            resources,
        })
    }
    #[cfg(all(test, any(feature = "paged-graphemes", feature = "paged-line-breaks")))]
    pub(crate) fn test_with_file(file: File) -> Result<Self> {
        Self::with_file(file)
    }
    #[cfg(test)]
    fn with_file(file: File) -> Result<Self> {
        let mut index = Self::new()?;
        // 真实只读、目录或可截短FD仍走同一实现；测试不替代后续写入、读取和close。
        index.file = Some(file);
        index.resources.file_open.store(true, Ordering::Release);
        Ok(index)
    }

    fn healthy(&self) -> Result<()> {
        if self.poisoned
            || self.resources.cleanup_failed.load(Ordering::Acquire)
            || self.producer_has_failed()
        {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    /// 只有换页回写或交付Reader才需要File；调用方的原失败出口负责永久撤权。
    fn ensure_file(&mut self) -> Result<()> {
        if self.file.is_none() {
            // 创建错误在首次真实IO边界原样返回；未取得FD不能伪造清理失败或重新建库兜底。
            let file = tempfile::tempfile()?;
            self.file = Some(file);
            self.resources.file_open.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn producer_has_failed(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.resources.producer_failed.load(Ordering::Acquire)
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }

    fn mark_failed(&mut self) {
        self.poisoned = true;
        #[cfg(feature = "paged-shaping-frontier")]
        self.resources
            .producer_failed
            .store(true, Ordering::Release);
    }

    fn page(&mut self, record: usize) -> Result<usize> {
        self.healthy()?;
        let result = (|| {
            let offset = record.checked_mul(N).ok_or(Error::Overflow)?;
            let number = offset / PAGE;
            if self.page.is_none() {
                self.page = Some(Page::new(Arc::clone(&self.resources)));
                self.number = number;
                self.loads = self.loads.checked_add(1).ok_or(Error::Overflow)?;
            } else if self.number != number {
                let start = self.number.checked_mul(PAGE).ok_or(Error::Overflow)?;
                self.ensure_file()?; // 原页即将离开唯一buffer，先取得其实际落盘owner。
                let file = self.file.as_ref().ok_or(Error::Poisoned)?;
                let page = self.page.as_mut().ok_or(Error::Poisoned)?;
                page.flush_through(file, start, PAGE)?;
                page.reset();
                self.number = number;
                self.loads = self.loads.checked_add(1).ok_or(Error::Overflow)?;
            }
            Ok(offset % PAGE)
        })();
        if result.is_err() {
            self.mark_failed();
        }
        result
    }

    /// 只有交付File读取权限或换页才flush；私有seal仍可读原已付费buffer。
    #[cfg(feature = "paged-line-compose")]
    fn flush_through(&mut self, records: usize) -> Result<()> {
        self.healthy()?;
        let result = (|| {
            let end = records.checked_mul(N).ok_or(Error::Overflow)?;
            if records > self.count {
                return Err(Error::InvalidRecord);
            }
            // 包括空into_reader：交付真实File前只创建一次，不能移交未分配的FD或第二buffer。
            self.ensure_file()?;
            if let Some(page) = self.page.as_mut() {
                let start = self.number.checked_mul(PAGE).ok_or(Error::Overflow)?;
                if end > start {
                    page.flush_through(
                        self.file.as_ref().ok_or(Error::Poisoned)?,
                        start,
                        end - start,
                    )?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.mark_failed();
        }
        result
    }

    pub(crate) fn append(&mut self, record: [u8; N]) -> Result<()> {
        self.healthy()?;
        if self.sealed {
            self.mark_failed();
            return Err(Error::Poisoned);
        }
        let result: Result<()> = (|| {
            let offset = self.page(self.count)?;
            self.page
                .as_mut()
                .ok_or(Error::InvalidRecord)?
                .write(offset, record)?;
            self.count = self.count.checked_add(1).ok_or(Error::Overflow)?;
            Ok(())
        })();
        if result.is_err() {
            self.mark_failed();
        }
        result
    }
    pub(crate) fn read(&mut self, record: usize) -> Result<[u8; N]> {
        self.healthy()?;
        if record >= self.count {
            self.mark_failed();
            return Err(Error::InvalidRecord);
        }
        let result = (|| {
            let offset = record.checked_mul(N).ok_or(Error::Overflow)?;
            if let Some(page) = self.page.as_ref()
                && self.number == offset / PAGE
            {
                // 命中原写页时直接读唯一buffer；seal不迫使已付费的记录落盘再读回。
                return page.read(offset % PAGE);
            }
            let file = self.file.as_ref().ok_or(Error::Poisoned)?;
            let offset = u64::try_from(offset).map_err(|_| Error::Overflow)?;
            let mut bytes = [0; N];
            // 异页只读取原FD上的固定栈record；旧页已在换页时flush，不预读或驱逐追加buffer。
            read_exact_at(&mut bytes, offset, |output, position| {
                file.read_at(output, position)
            })?;
            Ok(bytes)
        })();
        if result.is_err() {
            self.mark_failed();
        }
        result
    }

    /// 消费调用方已明确请求的一小批相邻record；不预读范围外字节，也不增加第二个页缓存。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn read_batch(
        &mut self,
        range: std::ops::Range<usize>,
        output: &mut [[u8; N]],
    ) -> Result<()> {
        self.healthy()?;
        let result = (|| {
            let records = range
                .end
                .checked_sub(range.start)
                .filter(|&records| records == output.len() && records <= 256)
                .ok_or(Error::InvalidRecord)?;
            let length = records.checked_mul(N).ok_or(Error::Overflow)?;
            if range.end > self.count || length > PAGE {
                return Err(Error::InvalidRecord);
            } // 范围及真实输出预算不匹配时整批拒绝，不能缩短后签发部分结果。
            if records == 0 {
                return Ok(()); // 空范围无需创建File或借用尚未分配的追加页。
            }
            let start = range.start.checked_mul(N).ok_or(Error::Overflow)?;
            let end = range.end.checked_mul(N).ok_or(Error::Overflow)?;
            let append_start = self.number.checked_mul(PAGE).ok_or(Error::Overflow)?;
            let output = output.as_flattened_mut();
            let file_end = end.min(append_start);
            let file_length = file_end.saturating_sub(start);
            if file_length != 0 {
                // 旧页已在换页时写出；把本次确定的相邻记录一次读回原输出，不读整页。
                let file = self.file.as_ref().ok_or(Error::Poisoned)?;
                let offset = u64::try_from(start).map_err(|_| Error::Overflow)?;
                read_exact_at(&mut output[..file_length], offset, |bytes, position| {
                    file.read_at(bytes, position)
                })?;
            }
            if file_length < length {
                // 跨界的尾部仍来自唯一追加buffer；其中尚未发布的记录不能从旧FD读成零。
                let page = self.page.as_ref().ok_or(Error::Poisoned)?;
                let local_start = start.max(append_start) - append_start;
                let local_end = end.checked_sub(append_start).ok_or(Error::InvalidRecord)?;
                if local_end > page.used {
                    return Err(Error::InvalidRecord);
                }
                let bytes = page.bytes.as_ref().ok_or(Error::Poisoned)?;
                output[file_length..].copy_from_slice(&bytes[local_start..local_end]);
            }
            Ok(())
        })();
        if result.is_err() {
            self.mark_failed(); // 半批IO或无效范围都撤销原owner，不发布已读前缀。
        }
        result
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }

    /// 只描述原唯一追加buffer已初始化的record；不加载旧页，也不创建另一份读取缓存。
    #[cfg(any(feature = "paged-graphemes", feature = "paged-line-breaks"))]
    pub(crate) fn append_page_records(&mut self) -> Result<Option<std::ops::Range<usize>>> {
        self.healthy()?;
        let result = (|| {
            let Some(page) = &self.page else {
                return Ok(None); // 尚无真实追加页，不能把未分配的零字节当作可读record。
            };
            let start = self.number.checked_mul(PAGE / N).ok_or(Error::Overflow)?;
            let end = start.checked_add(page.used / N).ok_or(Error::Overflow)?;
            // page.used只在完整record写入后增长；seal保留同一buffer及其真实有效范围。
            // 不把整页容量或已flush长度当作初始化长度，最后半页仍只授权实际写过的记录。
            Ok(Some(start..end))
        })();
        if result.is_err() {
            self.mark_failed(); // 坐标错误仍沿原owner撤权，不由查询优化改签成功或重新建页。
        }
        result
    }

    #[cfg(any(feature = "paged-graphemes", feature = "paged-line-breaks"))]
    pub(crate) fn page_loads(&self) -> u64 {
        self.loads
    }
    #[cfg(any(feature = "paged-graphemes", feature = "paged-line-breaks"))]
    pub(crate) fn poison(&mut self) {
        self.mark_failed();
    }
    pub(crate) fn resource_observer(&self) -> LayoutIndexObserver {
        LayoutIndexObserver {
            own: Arc::clone(&self.resources),
            #[cfg(feature = "paged-shaping-frontier")]
            upstream: None,
        }
    }
    pub(crate) fn seal(&mut self) -> Result<()> {
        self.healthy()?;
        // 这里只封闭私有追加权限；同owner查询继续使用原buffer，不为无File消费者强制写盘。
        self.sealed = true;
        Ok(())
    }

    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn into_reader(mut self) -> Result<LayoutIndexReader<N>> {
        // 真正交File前必须把完整逻辑记录写出；失败由原owner归还buffer/FD，绝不发布部分Reader。
        self.seal()?;
        self.flush_through(self.count)?;
        drop(self.page.take());
        self.count
            .checked_mul(N)
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(Error::Overflow)?;
        if self.resources.buffer.load(Ordering::Acquire) != 0 {
            // Reader只保原File与收据，不能把尚活的写buffer交给后台只读消费者。
            self.mark_failed();
            return Err(Error::Poisoned);
        }
        let file = self.file.take().ok_or(Error::Poisoned)?;
        // take只移交原唯一File；自身Drop已无FD/buffer可关，观察者仍描述同一个资源身份。
        Ok(LayoutIndexReader::from_sealed(
            file,
            self.count,
            Arc::clone(&self.resources),
        ))
    }

    pub(crate) fn close(&mut self) -> Result<()> {
        let mut first = None;
        // close不再交付新Reader；未发布的dirty尾部可丢弃，不为无消费者临时数据额外写盘。
        drop(self.page.take());
        if let Some(file) = self.file.take()
            && let Err(error) = close_file(file, &self.resources)
            && first.is_none()
        {
            first = Some(error);
        }
        self.poisoned = true; // 尚未分配FD与正常关闭都可能file=None；只有此终态撤销后续读写权。
        #[cfg(any(feature = "paged-graphemes", feature = "paged-shaping"))]
        self.resources.owner_closed.store(true, Ordering::Release); // 实际归还或移交之后才结束writer生命周期；Reader仍单独持其FD。
        if let Some(error) = first {
            #[cfg(feature = "paged-shaping-frontier")]
            self.resources
                .producer_failed
                .store(true, Ordering::Release);
            Err(error)
        } else if self.resources.cleanup_failed.load(Ordering::Acquire) {
            // 本次已把可归还资源真实排空，但原先的写入/归还失败不能被后来的close重签成功。
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
}
impl<const N: usize> Drop for PagedLayoutIndex<N> {
    fn drop(&mut self) {
        if self.close().is_err() {
            self.resources.cleanup_failed.store(true, Ordering::Release);
        }
    }
}

/// 观察者只有原子资源事实，不保留FD、mmap、源、算法或任何Native对象。
#[derive(Clone)]
pub(crate) struct LayoutIndexObserver {
    own: Arc<ResourceState>,
    #[cfg(feature = "paged-shaping-frontier")]
    upstream: Option<Arc<ResourceState>>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayoutIndexResources {
    pub(crate) file_open: bool,
    /// 原 writer 已交付而尚未真实 close 的前缀 FD 数，包含 close 结果未知的未归还名额。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) prefix_readers: usize,
    /// 原 inode 的真实写入、前缀读取或归还失败，后继前缀继续观察同一撤权事实。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) producer_failed: bool,
    /// Writer已实际close或完成File移交；不声称接收该File的Reader也已关闭。
    #[cfg(any(feature = "paged-graphemes", feature = "paged-shaping"))]
    pub(crate) owner_closed: bool,
    /// 此metadata后端从不映射；保留真实零值，与Source和shaper的映射收据分开。
    pub(crate) mapped_bytes: usize,
    pub(crate) peak_mapped_bytes: usize,
    /// 原唯一Box当前/历史实际分配；Reader不持工作buffer，prefix也不复制它。
    pub(crate) buffer_bytes: usize,
    pub(crate) peak_buffer_bytes: usize,
    pub(crate) cleanup_failed: bool,
    #[cfg(feature = "paged-line-compose")]
    pub(crate) reader_failed: bool,
}
impl LayoutIndexObserver {
    pub(crate) fn snapshot(&self) -> LayoutIndexResources {
        LayoutIndexResources {
            file_open: self.own.file_open.load(Ordering::Acquire),
            #[cfg(feature = "paged-shaping-frontier")]
            prefix_readers: self.own.prefix_readers.load(Ordering::Acquire),
            #[cfg(feature = "paged-shaping-frontier")]
            producer_failed: self.own.producer_failed.load(Ordering::Acquire),
            #[cfg(any(feature = "paged-graphemes", feature = "paged-shaping"))]
            owner_closed: self.own.owner_closed.load(Ordering::Acquire),
            mapped_bytes: 0,
            peak_mapped_bytes: 0,
            buffer_bytes: self.own.buffer.load(Ordering::Acquire),
            peak_buffer_bytes: self.own.peak_buffer.load(Ordering::Acquire),
            cleanup_failed: self.own.cleanup_failed.load(Ordering::Acquire),
            #[cfg(feature = "paged-line-compose")]
            reader_failed: self.own.reader_failed.load(Ordering::Acquire)
                || self.upstream_has_failed(),
        }
    }

    #[cfg(feature = "paged-line-compose")]
    fn upstream_has_failed(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.upstream.as_ref().is_some_and(|upstream| {
                upstream.producer_failed.load(Ordering::Acquire)
                    || upstream.cleanup_failed.load(Ordering::Acquire)
            })
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }
}

#[cfg(test)]
#[path = "markdown_layout_index/read_tests.rs"]
mod read_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_widths_round_trip_across_pages_reusing_one_actual_buffer() {
        fn check<const N: usize>() {
            let mut index = PagedLayoutIndex::<N>::new().unwrap();
            let observer = index.resource_observer();
            for row in 0usize..2049 {
                let mut bytes = [0; N];
                bytes[..8].copy_from_slice(&(row as u64).to_le_bytes());
                bytes[N - 1] = (row % 251) as u8;
                index.append(bytes).unwrap();
                assert_eq!(observer.snapshot().mapped_bytes, 0);
                assert_eq!(observer.snapshot().buffer_bytes, PAGE);
            }
            index.seal().unwrap();
            for row in [2048usize, 0, 513, 1024, 511, 2047] {
                let bytes = index.read(row).unwrap();
                assert_eq!(
                    u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                    row as u64
                );
                assert_eq!(bytes[N - 1], (row % 251) as u8);
                assert_eq!(observer.snapshot().mapped_bytes, 0);
                assert_eq!(observer.snapshot().buffer_bytes, PAGE);
            }
            index.close().unwrap();
            let state = observer.snapshot();
            assert!(
                !state.file_open
                    && state.mapped_bytes == 0
                    && state.buffer_bytes == 0
                    && !state.cleanup_failed
            );
            assert_eq!(state.peak_mapped_bytes, 0);
            assert_eq!(state.buffer_bytes, 0);
            assert_eq!(state.peak_buffer_bytes, PAGE);
        }
        check::<16>();
        check::<32>();
        check::<64>();
    }
    #[test]
    fn actual_readonly_write_failure_remains_negative_after_buffer_and_file_return() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly-index");
        std::fs::write(&path, b"unchanged").unwrap();
        let mut index = PagedLayoutIndex::<16>::with_file(File::open(&path).unwrap()).unwrap();
        let observer = index.resource_observer();
        // 小页尚未交付File消费者，先在唯一buffer追加；跨页才发生真正write_all_at。
        for _ in 0..PAGE / 16 {
            index.append([3; 16]).unwrap();
        }
        assert_eq!(index.read(0).unwrap(), [3; 16]);
        assert!(
            matches!(index.append([4; 16]), Err(Error::Io(error)) if error.raw_os_error() == Some(libc::EBADF))
        );
        assert!(matches!(index.read(0), Err(Error::Poisoned)));
        assert!(matches!(index.close(), Err(Error::Poisoned)));
        let state = observer.snapshot();
        assert!(
            !state.file_open
                && state.mapped_bytes == 0
                && state.buffer_bytes == 0
                && state.cleanup_failed
        );
        assert_eq!(state.peak_buffer_bytes, PAGE);
        assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
        assert!(matches!(index.close(), Err(Error::Poisoned)));
        drop(index);
        assert!(observer.snapshot().cleanup_failed);
    }
    #[test]
    fn invalid_width_and_post_seal_write_never_revive_cached_records() {
        assert!(matches!(
            PagedLayoutIndex::<0>::new(),
            Err(LayoutIndexError::InvalidRecord)
        ));
        assert!(matches!(
            PagedLayoutIndex::<3>::new(),
            Err(LayoutIndexError::InvalidRecord)
        ));
        assert!(matches!(
            PagedLayoutIndex::<32768>::new(),
            Err(LayoutIndexError::InvalidRecord)
        ));
        let mut index = PagedLayoutIndex::<32>::new().unwrap();
        let observer = index.resource_observer();
        index.append([7; 32]).unwrap();
        index.seal().unwrap();
        assert_eq!(index.read(0).unwrap(), [7; 32]);
        assert!(matches!(
            index.append([9; 32]),
            Err(LayoutIndexError::Poisoned)
        ));
        assert!(matches!(index.read(0), Err(LayoutIndexError::Poisoned)));
        drop(index);
        let state = observer.snapshot();
        assert!(
            !state.file_open
                && state.mapped_bytes == 0
                && state.buffer_bytes == 0
                && !state.cleanup_failed
        );
    }
}
