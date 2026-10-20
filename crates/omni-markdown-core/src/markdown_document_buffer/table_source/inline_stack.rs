//! 强调、真实括号与label恢复点共用一个匿名文件；只有原2KiB尾页留在RAM。

use crate::markdown_document_buffer::table_window_request::{
    MarkdownTableRequestError, SemanticWork,
};
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use std::ops::Range;
const PLAIN_CONTROL: usize = 128;
use pulldown_cmark::global_frontier::{
    GlobalWork, InlineLinkKind, InlineLinkRecord, InlineLinkRecordId, InlineLinkRecordStatus,
    InlineLinkStorageOperation, InlineLinkStorageReply, InlineLinkStorageRequest,
    InlineLinkStorageStep, InlineReferenceKind,
};
use pulldown_cmark::inline_emphasis::EmphasisBounds;
use std::{fs::File, io, os::fd::IntoRawFd, os::unix::fs::FileExt};

const PAGE: usize = 2048;
const SLOT: usize = 64;
const ENTRY: u8 = 1;
const LINK: u8 = 2;
const SCOPE: u8 = 3;
const CONTINUATION: u8 = 4;

#[derive(Clone, Copy)]
pub(in crate::markdown_document_buffer) struct Entry {
    // 原delimiter链只指同文件中的Entry；Link/Scope拥有独立tag，不能混用它们的id。
    pub previous: usize,
    pub depth: usize,
    pub start: usize,
    pub count: usize,
    pub run_length: usize,
    pub delimiter: u8,
    pub both: bool,
}

pub(in crate::markdown_document_buffer) struct Scope {
    // source/label是原Begin的完整owner；head/depth/bounds是进入label前的外层匹配状态。
    pub source: Range<usize>,
    pub label: Range<usize>,
    pub head: usize,
    pub depth: usize,
    pub bounds: EmphasisBounds,
    pub previous: Option<usize>,
}

pub(in crate::markdown_document_buffer) enum RecordStep<T> {
    // 页分配/分片flush确实完成后才交Progress；值只有整个record可读后才交给游标。
    Ready { value: T, work: SemanticWork },
    Progress { work: SemanticWork },
}

/// count是原64B slot水位；flushed只记录本尾页已经成功写入的真实字节，不是排版进度。
/// 大记录用连续slot且不跨页；页尾不够时写真实padding，不建立另一个页缓存或记录目录。
pub(in crate::markdown_document_buffer) struct StackFile {
    file: Option<File>,
    page: Option<Box<[u8; PAGE]>>,
    count: usize,
    flushed: usize,
}
impl StackFile {
    pub(in crate::markdown_document_buffer) fn new() -> Self {
        Self {
            file: None,
            page: None,
            count: 0,
            flushed: 0,
        }
    }

    /// 第一次实际记录才分配尾页；真实quota先准入，分配与记录写入分步归还。
    /// 满页每次至多写1KiB，合法2048量子不会因2KiB flush加record而永久零进展。
    fn append_bytes(
        &mut self,
        bytes: &[u8],
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<RecordStep<usize>, MarkdownTableRequestError> {
        if budget < PLAIN_CONTROL {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        if self.page.is_none() {
            // 同事务只有这一个尾页。取消/失败时随原owner归还，成功时必须显式close。
            quota
                .admit_metadata(PAGE)
                .map_err(MarkdownTableRequestError::Window)?;
            self.page = Some(Box::new([0; PAGE]));
            work.metadata_bytes = PAGE;
            return Ok(RecordStep::Progress { work });
        }
        let mut used = self.count % (PAGE / SLOT) * SLOT;
        if used != 0 && used + bytes.len() > PAGE {
            // padding属于真实文件格式，不签发record id；后继不可能把残半Link当完整Entry。
            let padding = PAGE - used;
            if work.structural_transitions + padding > budget {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            self.page.as_mut().unwrap()[used..].fill(0);
            self.count += padding / SLOT;
            work.structural_transitions += padding;
            used = 0;
        }
        if used == 0 && self.count != 0 && self.flushed != PAGE {
            // 全页只是RAM水位，未完成pwrite不能挪到下一页。flushed只在本次系统写成功
            // 后前进；失败直接沿sticky链退出，不在同操作盲重放结局未知的写入。
            if self.file.is_none() {
                self.file = Some(tempfile::tempfile().map_err(storage_error)?);
            }
            let count = (PAGE - self.flushed)
                .min(1024)
                .min(budget.saturating_sub(work.structural_transitions));
            if count == 0 {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            let offset = self.count * SLOT - PAGE + self.flushed;
            self.file
                .as_ref()
                .unwrap()
                .write_all_at(
                    &self.page.as_ref().unwrap()[self.flushed..self.flushed + count],
                    offset as u64,
                )
                .map_err(storage_error)?;
            self.flushed += count;
            work.structural_transitions += count;
            return Ok(RecordStep::Progress { work });
        }
        if work.structural_transitions + bytes.len() > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        self.page.as_mut().unwrap()[used..used + bytes.len()].copy_from_slice(bytes);
        // 分配与flush都已完成；此时才增加可读slot水位，防止Progress泄露半条记录。
        let id = self.count + 1;
        self.count += bytes.len() / SLOT;
        self.flushed = 0;
        work.structural_transitions += bytes.len();
        Ok(RecordStep::Ready { value: id, work })
    }

    /// 同一个固定record只读原尾页或原FD；类型tag拒绝把Link半记录当Emphasis/Scope解释。
    fn read_bytes<const N: usize>(
        &self,
        id: usize,
        tag: u8,
    ) -> Result<[u8; N], MarkdownTableRequestError> {
        if id == 0
            || (id - 1)
                .checked_add(N / SLOT)
                .is_none_or(|end| end > self.count)
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let offset = (id - 1) * SLOT;
        if offset % PAGE + N > PAGE {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut bytes = [0; N];
        if offset / PAGE == (self.count - 1) * SLOT / PAGE {
            bytes.copy_from_slice(&self.page.as_ref().unwrap()[offset % PAGE..offset % PAGE + N]);
        } else {
            // 旧页只从原FD读恰好一个record，保留read_exact_at真实短读/errno，不作零填补齐。
            self.file
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
                .read_exact_at(&mut bytes, offset as u64)
                .map_err(storage_error)?;
        }
        if bytes[SLOT - 1] != tag
            || bytes
                .as_chunks::<SLOT>()
                .0
                .iter()
                .skip(1)
                .any(|part| part[SLOT - 1] != CONTINUATION)
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(bytes)
    }

    pub(in crate::markdown_document_buffer) fn append(
        &mut self,
        entry: Entry,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<RecordStep<usize>, MarkdownTableRequestError> {
        let mut bytes = [0; SLOT];
        // 强调保原64B槽跨度。新Link/Scope即使同文件存在，也不改变原prev链的编码单位。
        for (at, value) in [
            entry.previous,
            entry.depth,
            entry.start,
            entry.count,
            entry.run_length,
        ]
        .into_iter()
        .enumerate()
        {
            put(&mut bytes, at, value);
        }
        bytes[40] = entry.delimiter;
        bytes[41] = u8::from(entry.both);
        bytes[SLOT - 1] = ENTRY;
        self.append_bytes(&bytes, quota, budget)
    }
    pub(in crate::markdown_document_buffer) fn read(
        &self,
        id: usize,
    ) -> Result<Entry, MarkdownTableRequestError> {
        let bytes = self.read_bytes::<SLOT>(id, ENTRY)?;
        Ok(Entry {
            previous: get(&bytes, 0)?,
            depth: get(&bytes, 1)?,
            start: get(&bytes, 2)?,
            count: get(&bytes, 3)?,
            run_length: get(&bytes, 4)?,
            delimiter: bytes[40],
            both: bytes[41] != 0,
        })
    }

    /// 外层scope的九个失败下界完整保存，不能清零或把文件ordinal当成真实delimiter深度。
    pub(in crate::markdown_document_buffer) fn append_scope(
        &mut self,
        scope: &Scope,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<RecordStep<usize>, MarkdownTableRequestError> {
        let mut bytes = [0; 3 * SLOT];
        for (at, value) in [
            scope.source.start,
            scope.source.end,
            scope.label.start,
            scope.label.end,
            scope.head,
            scope.depth,
            scope.previous.unwrap_or(0),
        ]
        .into_iter()
        .chain(scope.bounds.checkpoint())
        .enumerate()
        {
            put(&mut bytes, at, value);
        }
        bytes[SLOT - 1] = SCOPE;
        bytes[2 * SLOT - 1] = CONTINUATION;
        bytes[3 * SLOT - 1] = CONTINUATION;
        self.append_bytes(&bytes, quota, budget)
    }
    pub(in crate::markdown_document_buffer) fn read_scope(
        &self,
        id: usize,
    ) -> Result<Scope, MarkdownTableRequestError> {
        let bytes = self.read_bytes::<{ 3 * SLOT }>(id, SCOPE)?;
        let mut bounds = [0; 9];
        // 九个失败下界恢复原kernel，不以默认bounds重启而改变跨label外层强调的匹配结果。
        for (at, bound) in bounds.iter_mut().enumerate() {
            *bound = get(&bytes, at + 7)?;
        }
        Ok(Scope {
            source: get(&bytes, 0)?..get(&bytes, 1)?,
            label: get(&bytes, 2)?..get(&bytes, 3)?,
            head: get(&bytes, 4)?,
            depth: get(&bytes, 5)?,
            bounds: EmphasisBounds::restore(bounds),
            previous: match get(&bytes, 6)? {
                0 => None,
                id => Some(id),
            },
        })
    }

    /// 这是vendor真实括号栈的唯一外存端口，不读Source、不解析语法，也不拥有第二个FD。
    /// 错误直接进入Core原sticky链；只有成功完成原record操作才返回Ready恢复同一请求。
    pub(in crate::markdown_document_buffer) fn storage(
        &mut self,
        request: &InlineLinkStorageRequest,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<(InlineLinkStorageStep, usize), MarkdownTableRequestError> {
        let (reply, work) = match &request.operation {
            InlineLinkStorageOperation::Append(record) => {
                match self.append_bytes(&encode_link(record), quota, budget)? {
                    RecordStep::Progress { work } => {
                        return Ok((
                            InlineLinkStorageStep::Progress {
                                work: GlobalWork {
                                    byte_reads: 0,
                                    transitions: work.structural_transitions,
                                },
                            },
                            work.metadata_bytes,
                        ));
                    }
                    RecordStep::Ready { value, work } => (
                        InlineLinkStorageReply::Appended {
                            id: InlineLinkRecordId::new(value)
                                .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                        },
                        work,
                    ),
                }
            }
            InlineLinkStorageOperation::Read(id) => {
                // 准入紧前完成；不能先pread再以余额不足退回，否则真实IO会漏账。
                if budget < PLAIN_CONTROL + 2 * SLOT {
                    return Err(MarkdownTableRequestError::WorkLimit);
                }
                let record = decode_link(&self.read_bytes::<{ 2 * SLOT }>(id.get(), LINK)?)?;
                (
                    InlineLinkStorageReply::Read { record },
                    SemanticWork {
                        structural_transitions: PLAIN_CONTROL + 2 * SLOT,
                        ..Default::default()
                    },
                )
            }
            InlineLinkStorageOperation::Update { id, record } => {
                if budget < PLAIN_CONTROL + 4 * SLOT {
                    return Err(MarkdownTableRequestError::WorkLimit);
                }
                // 先真实验证原record，再覆写同一位置；不接受邻接slot或任意byte范围。
                self.read_bytes::<{ 2 * SLOT }>(id.get(), LINK)?;
                let offset = (id.get() - 1) * SLOT;
                let bytes = encode_link(record);
                if offset / PAGE == (self.count - 1) * SLOT / PAGE {
                    // 当前尾页尚由同pending操作独占。没有另外一个cursor可在半flush中更新它。
                    self.page.as_mut().unwrap()[offset % PAGE..offset % PAGE + bytes.len()]
                        .copy_from_slice(&bytes);
                } else {
                    self.file
                        .as_ref()
                        .unwrap()
                        .write_all_at(&bytes, offset as u64)
                        .map_err(storage_error)?;
                }
                (
                    InlineLinkStorageReply::Updated,
                    SemanticWork {
                        structural_transitions: PLAIN_CONTROL + 4 * SLOT,
                        ..Default::default()
                    },
                )
            }
        };
        if work.structural_transitions > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        Ok((
            InlineLinkStorageStep::Ready {
                reply,
                work: GlobalWork {
                    byte_reads: 0,
                    transitions: work.structural_transitions,
                },
            },
            work.metadata_bytes,
        ))
    }

    /// 成功发布前真正归还FD；close失败不重试可能已被内核复用的descriptor。
    pub(in crate::markdown_document_buffer) fn close(
        &mut self,
    ) -> Result<(), MarkdownTableRequestError> {
        self.page = None;
        // 去掉RAM数据和所有id后再终止FD拥有；后继同owner只能从新空文件开始，旧id不复活。
        self.count = 0;
        self.flushed = 0;
        if let Some(file) = self.file.take() {
            let descriptor = file.into_raw_fd();
            // SAFETY: 唯一File已移交close义务，无clone或后继读取者；失败后也不再次关闭该数值。
            if unsafe { libc::close(descriptor) } != 0 {
                return Err(storage_error(io::Error::last_os_error()));
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::markdown_document_buffer) fn descriptor(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.file.as_ref().map(AsRawFd::as_raw_fd)
    }
    #[cfg(test)]
    pub(in crate::markdown_document_buffer) fn replace_file(&mut self, file: File) {
        self.file = Some(file);
    }
}

/// 每slot最后一字节保类型；字段的固定小端编码跨slot时跳过tag，不依赖Rust内存布局。
fn put(bytes: &mut [u8], field: usize, value: usize) {
    for (at, byte) in (value as u64).to_le_bytes().into_iter().enumerate() {
        let logical = field * 8 + at;
        bytes[logical + logical / (SLOT - 1)] = byte;
    }
}
fn get(bytes: &[u8], field: usize) -> Result<usize, MarkdownTableRequestError> {
    let mut value = [0; 8];
    for (at, byte) in value.iter_mut().enumerate() {
        let logical = field * 8 + at;
        *byte = bytes[logical + logical / (SLOT - 1)];
    }
    usize::try_from(u64::from_le_bytes(value))
        .map_err(|_| MarkdownTableRequestError::InvalidRequest)
}
fn encode_link(record: &InlineLinkRecord) -> [u8; 2 * SLOT] {
    // 两槽固定格式保全owner/target/title/reference及两条真实链；不为64B尺寸删字段。
    let mut bytes = [0; 2 * SLOT];
    for (at, value) in [
        record.previous.map_or(0, InlineLinkRecordId::get),
        record.next.map_or(0, InlineLinkRecordId::get),
        record.serial,
        record.source_start,
        record.label_start,
        record.label_end,
        record.source_end,
        record.destination.start,
        record.destination.end,
        record.title.as_ref().map_or(0, |v| v.start),
        record.title.as_ref().map_or(0, |v| v.end),
        record.reference.as_ref().map_or(0, |(_, v)| v.start),
        record.reference.as_ref().map_or(0, |(_, v)| v.end),
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut bytes, at, value);
    }
    bytes[106] = u8::from(record.title.is_some());
    bytes[107] = match &record.reference {
        None => 0,
        Some((InlineReferenceKind::Full, _)) => 1,
        Some((InlineReferenceKind::Collapsed, _)) => 2,
        Some((InlineReferenceKind::Shortcut, _)) => 3,
    };
    bytes[108] = match record.kind {
        InlineLinkKind::Link => 0,
        InlineLinkKind::Image => 1,
    };
    bytes[109] = match record.status {
        InlineLinkRecordStatus::Open => 0,
        InlineLinkRecordStatus::Literal => 1,
        InlineLinkRecordStatus::Direct => 2,
        InlineLinkRecordStatus::Reference => 3,
    };
    bytes[SLOT - 1] = LINK;
    bytes[2 * SLOT - 1] = CONTINUATION;
    bytes
}
fn decode_link(bytes: &[u8; 2 * SLOT]) -> Result<InlineLinkRecord, MarkdownTableRequestError> {
    // 先拒绝未知枚举，再创建真实值。None仅使用格式自己的零哨兵，不把坏status当Literal。
    let reference = match bytes[107] {
        0 => None,
        1 => Some(InlineReferenceKind::Full),
        2 => Some(InlineReferenceKind::Collapsed),
        3 => Some(InlineReferenceKind::Shortcut),
        _ => return Err(MarkdownTableRequestError::InvalidRequest),
    };
    Ok(InlineLinkRecord {
        previous: InlineLinkRecordId::new(get(bytes, 0)?),
        next: InlineLinkRecordId::new(get(bytes, 1)?),
        serial: get(bytes, 2)?,
        source_start: get(bytes, 3)?,
        label_start: get(bytes, 4)?,
        label_end: get(bytes, 5)?,
        source_end: get(bytes, 6)?,
        destination: get(bytes, 7)?..get(bytes, 8)?,
        title: match bytes[106] {
            0 => None,
            1 => Some(get(bytes, 9)?..get(bytes, 10)?),
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        },
        reference: match reference {
            Some(kind) => Some((kind, get(bytes, 11)?..get(bytes, 12)?)),
            None => None,
        },
        kind: match bytes[108] {
            0 => InlineLinkKind::Link,
            1 => InlineLinkKind::Image,
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        },
        status: match bytes[109] {
            0 => InlineLinkRecordStatus::Open,
            1 => InlineLinkRecordStatus::Literal,
            2 => InlineLinkRecordStatus::Direct,
            3 => InlineLinkRecordStatus::Reference,
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        },
    })
}
fn storage_error(error: io::Error) -> MarkdownTableRequestError {
    MarkdownTableRequestError::InlineStorageUnavailable(error.raw_os_error().unwrap_or(libc::EIO))
}

impl std::fmt::Debug for StackFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StackFile")
            .field("slots", &self.count)
            .field("file_open", &self.file.is_some())
            .finish_non_exhaustive()
    }
}
