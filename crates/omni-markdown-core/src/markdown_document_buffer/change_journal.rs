//! 文档自有的真实变化日志；常见两项以内无I/O，多项仅用32KiB页缓冲和有限页身份表。
//!
//! 文件只存旧坐标及删除/插入长度，不含正文。所有页在提交前均属于本次候选；失败可释放，
//! 成功后历史仅保小型页票据。原root/正文仍由现有piece tree和session source拥有。

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};

use sha2::{Digest, Sha256};

const PAGE_BYTES: usize = 32 * 1024;
const MAX_JOURNAL_BYTES: usize = 256 * 1024 * 1024;
const PAGE_COUNT: usize = MAX_JOURNAL_BYTES / PAGE_BYTES;
const HEADER_BYTES: usize = 32;
const RECORD_BYTES: usize = 24;
const RECORDS_PER_PAGE: usize = (PAGE_BYTES - HEADER_BYTES) / RECORD_BYTES;
const NO_PAGE: u32 = u32::MAX;
const MAGIC: &[u8; 8] = b"OMNICHG1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只保留有限系统分类与数值码；不保存系统英文、路径、正文或分配式io::Error。
pub struct MarkdownChangeJournalIoFailure {
    pub kind: io::ErrorKind,
    pub raw_os_error: Option<i32>,
}

impl From<io::Error> for MarkdownChangeJournalIoFailure {
    fn from(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// 匿名日志错误不携带正文、路径或系统英文；调用方不得把I/O失败解释成成功提交。
pub enum MarkdownChangeJournalError {
    #[error("变化日志状态无效")]
    InvalidState,
    #[error("变化日志坐标无效")]
    InvalidChange,
    #[error("变化日志达到256MiB会话上限")]
    BudgetExceeded,
    #[error("无法创建私有匿名变化日志")]
    CreationUnavailable(MarkdownChangeJournalIoFailure),
    #[error("匿名变化日志的私有身份校验失败")]
    AnonymousIdentityRejected,
    #[error("无法完整写入变化日志")]
    WriteUnavailable(MarkdownChangeJournalIoFailure),
    #[error("无法完整读取变化日志")]
    ReadUnavailable(MarkdownChangeJournalIoFailure),
    #[error("变化日志内容校验失败")]
    Corrupt,
    #[error("变化日志失败后的截尾清理未确认")]
    TruncateUnavailable(MarkdownChangeJournalIoFailure),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Record {
    pub(super) start: usize,
    pub(super) removed: usize,
    pub(super) inserted: usize,
}

/// 票据只在本document内部移动；不能Clone后让同一页被两个history重复释放。
pub(super) struct Entry {
    old_length: usize,
    new_length: usize,
    payload: Payload,
}

impl Entry {
    /// 同一组合的净替换只有一条实际变化；直接保inline记录，不为合并重新借正文或建立日志FD。
    pub(super) fn single_replacement(
        old_length: usize,
        record: Record,
    ) -> Result<Self, MarkdownChangeJournalError> {
        let (old_end, new_end) = advance_coordinates(old_length, 0, 0, record)?;
        let new_length = new_end
            .checked_add(old_length - old_end)
            .ok_or(MarkdownChangeJournalError::InvalidChange)?;
        Ok(Self {
            old_length,
            new_length,
            payload: Payload::Inline {
                records: [record, Record::default()],
                count: usize::from(record.removed != 0 || record.inserted != 0),
            },
        })
    }
}

enum Payload {
    Inline {
        records: [Record; 2],
        count: usize,
    },
    Pages {
        id: u64,
        first: u32,
        pages: usize,
        records: usize,
        digest: [u8; 32],
    },
}

struct Pending {
    old_length: usize,
    old_cursor: usize,
    new_cursor: usize,
    inline: [Record; 2],
    records: usize,
    id: Option<u64>,
    first: u32,
    current: u32,
    pages: usize,
    page_records: usize,
    scratch: Option<Box<[u8; PAGE_BYTES]>>,
    hash: Sha256,
    file_length_before: u64,
    failure: Option<MarkdownChangeJournalError>,
}

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Fault {
    Create,
    Write,
    PartialWrite,
    Read,
    Truncate,
}

/// 唯一FD只属于文档；固定64KiB页身份表与匹配数量无关，首次第三项变化才取得它们。
pub(super) struct ChangeJournal {
    file: Option<File>,
    owners: Option<Box<[u64; PAGE_COUNT]>>,
    // 写入尝试可触及的extent上界；普通inline事务不为checkpoint执行stat或其他I/O。
    possible_extent: u64,
    next_id: u64,
    pending: Option<Pending>,
    page_limit: usize,
    #[cfg(test)]
    fault: std::cell::Cell<Option<Fault>>,
}

impl Default for ChangeJournal {
    fn default() -> Self {
        Self {
            file: None,
            owners: None,
            possible_extent: 0,
            next_id: 1,
            pending: None,
            page_limit: PAGE_COUNT,
            #[cfg(test)]
            fault: std::cell::Cell::new(None),
        }
    }
}

impl ChangeJournal {
    #[cfg(test)]
    pub(super) fn fail_next(&self, fault: Fault) {
        self.fault.set(Some(fault)); // 测试只注入一次，生产没有此入口或额外分支。
    }

    pub(super) fn begin(&mut self, old_length: usize) -> Result<(), MarkdownChangeJournalError> {
        if self.pending.is_some() {
            return Err(MarkdownChangeJournalError::InvalidState);
        }
        let file_length_before = self.possible_extent;
        self.pending = Some(Pending {
            old_length,
            old_cursor: 0,
            new_cursor: 0,
            inline: [Record::default(); 2],
            records: 0,
            id: None,
            first: NO_PAGE,
            current: NO_PAGE,
            pages: 0,
            page_records: 0,
            scratch: None,
            hash: Sha256::new(),
            file_length_before,
            failure: None,
        });
        Ok(())
    }

    pub(super) fn push(&mut self, record: Record) -> Result<(), MarkdownChangeJournalError> {
        if let Some(error) = self.pending.as_ref().and_then(|pending| pending.failure) {
            return Err(error); // 错误后的输入不能修补成另一个可提交journal。
        }
        let result = self.push_inner(record);
        if let Err(error) = result
            && let Some(pending) = &mut self.pending
        {
            pending.failure = Some(error);
        }
        result
    }

    fn push_inner(&mut self, record: Record) -> Result<(), MarkdownChangeJournalError> {
        if let Some(pending) = self.pending.as_mut() {
            let (old_end, new_end) = advance_coordinates(
                pending.old_length,
                pending.old_cursor,
                pending.new_cursor,
                record,
            )?;
            let buffered = if pending.id.is_none() && pending.records < 2 {
                pending.inline[pending.records] = record;
                true
            } else if pending.id.is_some() && pending.page_records < RECORDS_PER_PAGE {
                Self::buffer_record(pending, record)?;
                true
            } else {
                false // 只有升级或页满需要取得journal文件/页目录的独占访问。
            };
            if buffered {
                // 高频记录只改当前字段，不逐match搬移Sha256状态、整份Pending或任何正文。
                pending.records = pending
                    .records
                    .checked_add(1)
                    .ok_or(MarkdownChangeJournalError::BudgetExceeded)?;
                pending.old_cursor = old_end;
                pending.new_cursor = new_end;
                return Ok(());
            }
        }
        // 暂取候选结束字段借用，再操作唯一文件；任何失败仍把候选放回供统一abort释放。
        let mut pending = self
            .pending
            .take()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        let result = self.push_pending(&mut pending, record);
        self.pending = Some(pending);
        result
    }

    fn push_pending(
        &mut self,
        pending: &mut Pending,
        record: Record,
    ) -> Result<(), MarkdownChangeJournalError> {
        let (old_end, new_end) = advance_coordinates(
            pending.old_length,
            pending.old_cursor,
            pending.new_cursor,
            record,
        )?;
        if pending.records < 2 && pending.id.is_none() {
            pending.inline[pending.records] = record;
        } else {
            if pending.id.is_none() {
                // 升级发生在第三项之前；普通键入/两侧Surround不创建FD或64KiB页目录。
                self.ensure_file()?;
                let id = self.next_id;
                self.next_id = id
                    .checked_add(1)
                    .ok_or(MarkdownChangeJournalError::BudgetExceeded)?;
                pending.id = Some(id);
                pending.scratch = Some(Box::new([0; PAGE_BYTES]));
                let first = self.allocate_page(id)?;
                pending.first = first;
                pending.current = first;
                pending.pages = 1;
                for previous in pending.inline {
                    self.append_record(pending, previous)?;
                }
            }
            self.append_record(pending, record)?;
        }
        pending.records = pending
            .records
            .checked_add(1)
            .ok_or(MarkdownChangeJournalError::BudgetExceeded)?;
        pending.old_cursor = old_end;
        pending.new_cursor = new_end;
        Ok(())
    }

    fn ensure_file(&mut self) -> Result<(), MarkdownChangeJournalError> {
        if self.file.is_some() {
            return Ok(());
        }
        #[cfg(test)]
        self.check_fault(Fault::Create)
            .map_err(|error| MarkdownChangeJournalError::CreationUnavailable(error.into()))?;
        let file = tempfile::tempfile()
            .map_err(|error| MarkdownChangeJournalError::CreationUnavailable(error.into()))?;
        let metadata = file
            .metadata()
            .map_err(|error| MarkdownChangeJournalError::CreationUnavailable(error.into()))?;
        if metadata.nlink() != 0
            || metadata.permissions().mode() & 0o777 != 0o600
            || !metadata.is_file()
        {
            return Err(MarkdownChangeJournalError::AnonymousIdentityRejected);
        }
        // 私有匿名身份核准后才分配页目录；失败局部File自然close，没有可留下的命名文件。
        self.owners = Some(Box::new([0; PAGE_COUNT]));
        self.file = Some(file);
        Ok(())
    }

    fn allocate_page(&mut self, id: u64) -> Result<u32, MarkdownChangeJournalError> {
        if self.page_limit > PAGE_COUNT {
            return Err(MarkdownChangeJournalError::BudgetExceeded); // 私有测试缩小容量不能扩大真实硬边界。
        }
        let owners = self
            .owners
            .as_mut()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        let page = owners[..self.page_limit]
            .iter()
            .position(|owner| *owner == 0)
            .ok_or(MarkdownChangeJournalError::BudgetExceeded)?;
        // 分配前已检查固定页上限；候选绝不覆盖当前undo/redo仍拥有的页。
        owners[page] = id;
        Ok(page as u32)
    }

    fn append_record(
        &mut self,
        pending: &mut Pending,
        record: Record,
    ) -> Result<(), MarkdownChangeJournalError> {
        if pending.page_records == RECORDS_PER_PAGE {
            let next =
                self.allocate_page(pending.id.ok_or(MarkdownChangeJournalError::InvalidState)?)?;
            self.flush_page(pending, next)?;
            pending.current = next;
            pending.pages += 1;
            pending.page_records = 0;
        }
        Self::buffer_record(pending, record)
    }

    fn buffer_record(
        pending: &mut Pending,
        record: Record,
    ) -> Result<(), MarkdownChangeJournalError> {
        let at = HEADER_BYTES + pending.page_records * RECORD_BYTES;
        let scratch = pending
            .scratch
            .as_mut()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        for (index, value) in [record.start, record.removed, record.inserted]
            .into_iter()
            .enumerate()
        {
            let value =
                u64::try_from(value).map_err(|_| MarkdownChangeJournalError::InvalidChange)?;
            scratch[at + index * 8..at + (index + 1) * 8].copy_from_slice(&value.to_le_bytes());
        }
        pending.page_records += 1;
        Ok(())
    }

    fn flush_page(
        &mut self,
        pending: &mut Pending,
        next: u32,
    ) -> Result<(), MarkdownChangeJournalError> {
        let scratch = pending
            .scratch
            .as_mut()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        scratch[..8].copy_from_slice(MAGIC);
        scratch[8..16].copy_from_slice(
            &pending
                .id
                .ok_or(MarkdownChangeJournalError::InvalidState)?
                .to_le_bytes(),
        );
        scratch[16..20].copy_from_slice(&next.to_le_bytes());
        scratch[20..24].copy_from_slice(&(pending.page_records as u32).to_le_bytes());
        scratch[24..28].copy_from_slice(&((pending.pages - 1) as u32).to_le_bytes());
        scratch[28..32].fill(0);
        #[cfg(test)]
        self.check_fault(Fault::Write)
            .map_err(|error| MarkdownChangeJournalError::WriteUnavailable(error.into()))?;
        // 页号只能来自固定owner目录；整个pwrite范围先于I/O验证，文件物理extent不会越过硬预算。
        let offset = u64::from(pending.current) * PAGE_BYTES as u64;
        if offset + PAGE_BYTES as u64 > MAX_JOURNAL_BYTES as u64 {
            return Err(MarkdownChangeJournalError::BudgetExceeded);
        }
        // 即便write_all_at部分写入后失败，abort也知道必须截回哪个上界；不把失败当成零写入。
        self.possible_extent = self.possible_extent.max(offset + PAGE_BYTES as u64);
        #[cfg(test)]
        if self.fault.get() == Some(Fault::PartialWrite) {
            // 真实pwrite部分页后报错，检验abort并非假定失败等于零字节写入；仅测试可达。
            self.fault.set(None);
            self.file
                .as_ref()
                .ok_or(MarkdownChangeJournalError::InvalidState)?
                .write_all_at(&scratch[..HEADER_BYTES + RECORD_BYTES], offset)
                .map_err(|error| MarkdownChangeJournalError::WriteUnavailable(error.into()))?;
            return Err(MarkdownChangeJournalError::WriteUnavailable(
                io::Error::other("synthetic partial write").into(),
            ));
        }
        self.file
            .as_ref()
            .ok_or(MarkdownChangeJournalError::InvalidState)?
            .write_all_at(scratch.as_slice(), offset)
            .map_err(|error| MarkdownChangeJournalError::WriteUnavailable(error.into()))?;
        pending
            .hash
            .update(&scratch[HEADER_BYTES..HEADER_BYTES + pending.page_records * RECORD_BYTES]);
        Ok(())
    }

    pub(super) fn finish(
        &mut self,
        new_length: usize,
    ) -> Result<Entry, MarkdownChangeJournalError> {
        let mut pending = self
            .pending
            .take()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        let result = (|| {
            if let Some(error) = pending.failure {
                return Err(error); // 即便调用方误忽略push错误，最终完成也绝不发布部分记录。
            }
            let expected = pending
                .new_cursor
                .checked_add(pending.old_length - pending.old_cursor)
                .ok_or(MarkdownChangeJournalError::InvalidChange)?;
            if expected != new_length {
                return Err(MarkdownChangeJournalError::InvalidChange);
            }
            let payload = if let Some(id) = pending.id {
                self.flush_page(&mut pending, NO_PAGE)?;
                Payload::Pages {
                    id,
                    first: pending.first,
                    pages: pending.pages,
                    records: pending.records,
                    digest: pending.hash.clone().finalize().into(),
                }
            } else {
                Payload::Inline {
                    records: pending.inline,
                    count: pending.records,
                }
            };
            Ok(Entry {
                old_length: pending.old_length,
                new_length,
                payload,
            })
        })();
        if result.is_err() {
            self.pending = Some(pending); // 最后一页失败仍必须由原checkpoint统一回收。
        }
        result
    }

    pub(super) fn abort(&mut self) -> Result<(), MarkdownChangeJournalError> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if let Some(id) = pending.id {
            self.free_id(id);
        }
        if self.close_if_empty() {
            return Ok(()); // 最后一个paged owner已消失，直接close匿名FD，无需可能失败的truncate。
        }
        if let Some(file) = &self.file
            && self.possible_extent > pending.file_length_before
        {
            #[cfg(test)]
            self.check_fault(Fault::Truncate)
                .map_err(|error| MarkdownChangeJournalError::TruncateUnavailable(error.into()))?;
            file.set_len(pending.file_length_before)
                .map_err(|error| MarkdownChangeJournalError::TruncateUnavailable(error.into()))?;
            self.possible_extent = pending.file_length_before;
        }
        Ok(())
    }

    pub(super) fn release(&mut self, entry: Entry) {
        if let Payload::Pages { id, .. } = entry.payload {
            self.free_id(id);
            self.close_if_empty();
        }
        // 提交后回收只改有限纯metadata或close最后FD，不执行可失败的I/O。
        // 尚有活页时空页留在匿名文件内重用，物理大小始终不超过256MiB。
    }

    fn free_id(&mut self, id: u64) {
        if let Some(owners) = &mut self.owners {
            for owner in owners.iter_mut() {
                if *owner == id {
                    *owner = 0;
                }
            }
        }
    }

    fn close_if_empty(&mut self) -> bool {
        if self
            .owners
            .as_ref()
            .is_some_and(|owners| owners.iter().all(|owner| *owner == 0))
        {
            self.file = None;
            self.owners = None;
            self.possible_extent = 0;
            return true;
        }
        false
    }

    pub(super) fn visit<E>(
        &self,
        entry: &Entry,
        inverse: bool,
        mut visit: impl FnMut(Record) -> Result<(), E>,
    ) -> Result<(), VisitError<E>> {
        let (mut old_cursor, mut new_cursor) = (0, 0);
        let mut emit = |record: Record| -> Result<(), VisitError<E>> {
            let (old_end, new_end) =
                advance_coordinates(entry.old_length, old_cursor, new_cursor, record)
                    .map_err(VisitError::Journal)?;
            let actual = if inverse {
                Record {
                    start: new_end - record.inserted,
                    removed: record.inserted,
                    inserted: record.removed,
                }
            } else {
                record
            };
            visit(actual).map_err(VisitError::Observer)?;
            old_cursor = old_end;
            new_cursor = new_end;
            Ok(())
        };
        match &entry.payload {
            Payload::Inline { records, count } => {
                for record in &records[..*count] {
                    emit(*record)?;
                }
            }
            Payload::Pages {
                id,
                first,
                pages,
                records,
                digest,
            } => {
                let owners = self
                    .owners
                    .as_ref()
                    .ok_or(VisitError::Journal(MarkdownChangeJournalError::Corrupt))?;
                let file = self
                    .file
                    .as_ref()
                    .ok_or(VisitError::Journal(MarkdownChangeJournalError::Corrupt))?;
                if *pages == 0 || *pages > self.page_limit || *records > *pages * RECORDS_PER_PAGE {
                    return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
                }
                let mut buffer = [0u8; PAGE_BYTES];
                let mut page = *first;
                let mut read_records = 0;
                let mut hash = Sha256::new();
                for index in 0..*pages {
                    if page as usize >= self.page_limit || owners[page as usize] != *id {
                        return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
                    }
                    #[cfg(test)]
                    self.check_fault(Fault::Read).map_err(|error| {
                        VisitError::Journal(MarkdownChangeJournalError::ReadUnavailable(
                            error.into(),
                        ))
                    })?;
                    file.read_exact_at(&mut buffer, u64::from(page) * PAGE_BYTES as u64)
                        .map_err(|error| {
                            VisitError::Journal(MarkdownChangeJournalError::ReadUnavailable(
                                error.into(),
                            ))
                        })?;
                    let next = u32_at(&buffer, 16);
                    let count = u32_at(&buffer, 20) as usize;
                    if &buffer[..8] != MAGIC
                        || u64_at(&buffer, 8) != *id
                        || u32_at(&buffer, 24) as usize != index
                        || u32_at(&buffer, 28) != 0
                        || count == 0
                        || count > RECORDS_PER_PAGE
                        || index + 1 < *pages && count != RECORDS_PER_PAGE
                        || (index + 1 == *pages) != (next == NO_PAGE)
                    {
                        return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
                    }
                    let payload = &buffer[HEADER_BYTES..HEADER_BYTES + count * RECORD_BYTES];
                    hash.update(payload);
                    for bytes in payload.as_chunks::<RECORD_BYTES>().0 {
                        emit(Record {
                            start: usize_at(bytes, 0)?,
                            removed: usize_at(bytes, 8)?,
                            inserted: usize_at(bytes, 16)?,
                        })?;
                    }
                    read_records += count;
                    page = next;
                }
                let actual: [u8; 32] = hash.finalize().into();
                if read_records != *records || actual != *digest {
                    return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
                }
            }
        }
        if new_cursor.checked_add(entry.old_length - old_cursor) != Some(entry.new_length) {
            return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
        }
        Ok(())
    }

    #[cfg(test)]
    fn check_fault(&self, operation: Fault) -> io::Result<()> {
        if self.fault.get() == Some(operation) {
            self.fault.set(None);
            return Err(io::Error::other("synthetic journal failure"));
        }
        Ok(())
    }
}

pub(super) enum VisitError<E> {
    Journal(MarkdownChangeJournalError),
    Observer(E),
}

fn advance_coordinates(
    old_length: usize,
    old: usize,
    new: usize,
    record: Record,
) -> Result<(usize, usize), MarkdownChangeJournalError> {
    let old_end = record
        .start
        .checked_add(record.removed)
        .filter(|end| *end <= old_length)
        .ok_or(MarkdownChangeJournalError::InvalidChange)?;
    let gap = record
        .start
        .checked_sub(old)
        .ok_or(MarkdownChangeJournalError::InvalidChange)?;
    let new_end = new
        .checked_add(gap)
        .and_then(|start| start.checked_add(record.inserted))
        .ok_or(MarkdownChangeJournalError::InvalidChange)?;
    Ok((old_end, new_end))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("fixed journal field"))
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("fixed journal field"))
}
fn usize_at<E>(bytes: &[u8], at: usize) -> Result<usize, VisitError<E>> {
    usize::try_from(u64_at(bytes, at))
        .map_err(|_| VisitError::Journal(MarkdownChangeJournalError::Corrupt))
}

#[cfg(test)]
mod tests;
