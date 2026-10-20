//! 原表格 flow 的动态结构目录；唯一 writer 追加原页，消费者只读取已出版的不可变前缀。

use std::sync::{Arc, Mutex, Weak};

use super::{
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource, VerifiedMarkdownLayoutTableRow,
};
use crate::markdown_layout_index::{
    LayoutIndexError, LayoutIndexObserver, LayoutIndexReader, LayoutIndexResources,
    LayoutIndexSnapshotError, PagedLayoutIndex,
};
use crate::{MarkdownDocumentBufferError, MarkdownGraphemeBoundary};

type Result<T> = std::result::Result<T, MarkdownDocumentBufferError>;
const RECORD_BYTES: usize = 128;

/// 首尾都是原完整 Source 的硬边界；tail 只包含真实末 CR/LF，未终止 EOF 时等于 end。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TableSpan {
    pub(crate) start: MarkdownGraphemeBoundary,
    pub(crate) end: MarkdownGraphemeBoundary,
    pub(crate) tail: MarkdownGraphemeBoundary,
    pub(crate) header: usize,
    pub(crate) ordinal: usize,
    pub(crate) eof: bool,
}

/// GCB/LB/SCX/UBA 与原输出共享同一个已出版前缀，不共享可写页或文件游标。
#[derive(Clone)]
pub(crate) struct TablePlan(Arc<Plan>);
struct Plan {
    source: MarkdownLayoutTextAuthority,
    publication: Mutex<Publication>,
    resource: Arc<Mutex<Option<LayoutIndexObserver>>>,
}
struct Publication {
    rows: Option<PublishedRows>,
    failure: Option<MarkdownDocumentBufferError>,
}

#[derive(Clone)]
/// 首尾与 reader 在同一锁内出版，只描述该不可变前缀实际包含的结构行。
/// 包络外查询不必逐字形重读整张目录；包络内的间隙仍须由原二分证明，不能补签无表权限。
struct PublishedRows {
    reader: Arc<LayoutIndexReader<RECORD_BYTES>>,
    start: MarkdownGraphemeBoundary,
    end: MarkdownGraphemeBoundary,
}

/// 原 Coordinator 唯一持有追加页；样式重排继续移动该 owner，不复制 writer 或另建目录。
pub(crate) struct TablePlanWriter {
    plan: TablePlan,
    index: Option<PagedLayoutIndex<RECORD_BYTES>>,
    next: usize,
    last: Option<TableSpan>,
}

/// 一次原量子只做一条记录访问或一次有限后缀出版；真实 I/O 与目录工作交回原预算。
pub(crate) struct TableRowDirectoryStep {
    pub(crate) records: usize,
    pub(crate) metadata_units: usize,
    pub(crate) ready: bool,
}

impl TablePlanWriter {
    /// 首行真正追加前没有 FD 或页；共享值只持原弱 Source 身份和空出版槽。
    pub(crate) fn new(source: &MarkdownLayoutTextSource) -> Self {
        Self {
            plan: TablePlan(Arc::new(Plan {
                source: source.authority(),
                publication: Mutex::new(Publication {
                    rows: None,
                    failure: None,
                }),
                resource: Arc::new(Mutex::new(None)),
            })),
            index: None,
            next: 0,
            last: None,
        }
    }

    /// 分享的是只读权限；接收方无法追加、flush 或归还原 writer。
    pub(crate) fn plan(&self) -> TablePlan {
        self.plan.clone()
    }

    /// 同一不可变根重新布局时按原行序读取既有记录；不重置文件、页或语言算法 owner。
    pub(crate) fn rewind(&mut self) {
        self.next = 0;
    }

    /// 重排只查看下一条原结构记录，不推进 writer；真实行票仍须由当前语义出版物重新取得。
    /// 空计划和已出版前缀末端没有文件读取；返回的记录数只统计本次实际 pread。
    pub(crate) fn peek_published(
        &self,
        source: &MarkdownLayoutTextSource,
    ) -> Result<(Option<TableSpan>, usize)> {
        self.plan.require(source)?;
        let Some(published) = self.plan.published()? else {
            return Ok((None, 0));
        };
        let reader = &published.reader;
        if self.next >= reader.len() {
            self.plan.require(source)?;
            return Ok((None, 0));
        }
        // 锁内只借真实不可变 reader；原记录 I/O、错误记忆与临时 FD 引用归还都在锁外。
        let span = self.plan.read_from(reader, self.next);
        self.plan.require(source)?;
        Ok((Some(span?), 1))
    }

    /// 此入口没有共享锁；追加、flush、dup 和旧前缀 close 都由原协调器量子实际执行。
    pub(crate) fn advance_row(
        &mut self,
        source: &MarkdownLayoutTextSource,
        row: &VerifiedMarkdownLayoutTableRow,
    ) -> Result<TableRowDirectoryStep> {
        self.plan.require(source)?;
        if !source.permits(row.source_authority()) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let span = row.span();
        // 重排仍从第零条结构记录前进；一次只读当前原记录，不在无预算入口二分读整张目录。
        let published = self.plan.published()?;
        let count = published.as_ref().map_or(0, |rows| rows.reader.len());
        if self.next < count {
            let actual = self.plan.read_from(
                &published.as_ref().expect("非空前缀持真实 reader").reader,
                self.next,
            )?;
            if actual != span {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            self.next += 1;
            return Ok(TableRowDirectoryStep {
                records: 1,
                metadata_units: 1,
                ready: true,
            });
        }
        // 先释放本次借到的前缀；出版候选不因自己的临时 Arc 占住旧文件配额。
        drop(published);
        if self.index.is_none() {
            let index = PagedLayoutIndex::new().map_err(storage_error)?;
            *self
                .plan
                .0
                .resource
                .lock()
                .expect("原表格资源锁不得在 panic 后继续使用") = Some(index.resource_observer());
            self.index = Some(index);
        }
        let index = self.index.as_mut().expect("首条结构记录已建立唯一 writer");
        if index.len() == self.next {
            if self
                .last
                .is_some_and(|last| span.start.utf16 < last.end.utf16)
            {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            index
                .append(span.encode())
                .map_err(|error| self.plan.remember(storage_error(error)))?;
            self.last = Some(span);
            // 真实记录追加与后续出版分别付费；此时消费者看不到尚未 flush 的行。
            return Ok(TableRowDirectoryStep {
                records: 1,
                metadata_units: 1,
                ready: false,
            });
        }
        if index.len() != self.next + 1 || self.last != Some(span) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let reader = match index.snapshot_reader(index.len()) {
            Ok(reader) => Arc::new(reader),
            Err(LayoutIndexSnapshotError::ReadersBusy) => {
                // 跨线程的旧查询尚持原 FD；原请求、Grid 响应和已追加记录都保留，不重写或毒化目录。
                return Ok(TableRowDirectoryStep {
                    records: 0,
                    metadata_units: 1,
                    ready: false,
                });
            }
            Err(LayoutIndexSnapshotError::Index(error)) => {
                return Err(self.plan.remember(storage_error(error)));
            }
        };
        self.plan.require(source)?;
        let old = {
            let mut publication = self
                .plan
                .0
                .publication
                .lock()
                .expect("原表格出版锁不得在 panic 后继续使用");
            if let Some(error) = publication.failure {
                drop(publication);
                drop(reader); // 实际 close 必须位于共享锁之外。
                return Err(error);
            }
            // 原有首行不能随追加或样式 rewind 改写；新尾只来自本次刚成功 flush/dup 的行票。
            // 两个坐标系与同一 reader 一起替换，查询不会把新包络误用于较短的旧前缀。
            let start = publication
                .rows
                .as_ref()
                .map_or(span.start, |rows| rows.start);
            publication.rows.replace(PublishedRows {
                reader,
                start,
                end: span.end,
            })
        };
        drop(old); // 只移出旧 Arc；最后一个读者退出时执行原 reader 的唯一 close 责任。
        self.next += 1;
        self.plan.require(source)?;
        Ok(TableRowDirectoryStep {
            records: 0,
            metadata_units: 1,
            ready: true,
        })
    }
}

impl TablePlan {
    /// 观察者只保原子收据与目录弱身份，不能保活页、reader 或任何 FD。
    pub(crate) fn resource_observer(&self) -> TablePlanObserver {
        TablePlanObserver {
            owner: Arc::downgrade(&self.0),
            resource: self.0.resource.clone(),
        }
    }

    /// Source 的首错和精确身份先于派生目录错误；此处不访问任何文件。
    pub(crate) fn require(&self, source: &MarkdownLayoutTextSource) -> Result<()> {
        source.health()?;
        if !source.permits(&self.0.source) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.health()
    }

    /// 短锁仅复制当前不可变出版物；返回的 Arc 是实际 pread 期间原 FD 的释放边界。
    fn published(&self) -> Result<Option<PublishedRows>> {
        let publication = self
            .0
            .publication
            .lock()
            .expect("原表格出版锁不得在 panic 后继续使用");
        if let Some(error) = publication.failure {
            return Err(error);
        }
        Ok(publication.rows.clone())
    }

    /// 一次只读一条固定原记录；尚未出版不意味着整个 Source 已到 EOF。
    pub(crate) fn read(&self, ordinal: usize) -> Result<Option<TableSpan>> {
        let Some(published) = self.published()? else {
            return Ok(None);
        };
        let reader = &published.reader;
        if ordinal >= reader.len() {
            self.health()?;
            return Ok(None);
        }
        self.read_from(reader, ordinal).map(Some)
    }

    /// canonical 行内部没有原 Unicode 布局权限，两端仍是原 Source 的真实硬边界。
    pub(crate) fn contains(&self, position: usize, utf16: bool) -> Result<bool> {
        self.contains_position(position, utf16, false)
    }

    /// 样式/SCX/UBA 在行起点没有正文字符权限；caret 硬边界不等同于可整形首字符。
    pub(crate) fn contains_character(&self, position: usize, utf16: bool) -> Result<bool> {
        self.contains_position(position, utf16, true)
    }

    /// 二分只借同一份不可变前缀；整个 pread 序列均位于共享锁之外。
    pub(crate) fn intersects(&self, range: &std::ops::Range<usize>) -> Result<bool> {
        if range.is_empty() {
            return self.contains_character(range.start, true);
        }
        let Some(published) = self.published()? else {
            return Ok(false);
        };
        let reader = &published.reader;
        // 原包络只是当前前缀的真实首尾，不依赖文档 EOF。未来行尚未出版时，与原二分的
        // 有限前缀合同一致；首错仍先核原 reader，再核同目录现行健康，不能用快路洗掉失败。
        if range.end <= published.start.utf16 || range.start >= published.end.utf16 {
            reader
                .healthy()
                .map_err(|error| self.remember(storage_error(error)))?;
            self.health()?;
            return Ok(false);
        }
        let mut low = 0;
        let mut high = reader.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.read_from(reader, middle)?.end.utf16 <= range.start {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let result = low < reader.len() && self.read_from(reader, low)?.start.utf16 < range.end;
        self.health()?;
        Ok(result)
    }

    /// 原 observer 的原子健康检查也覆盖旧前缀的真实 I/O/close 失败，不用新出版物洗掉首错。
    pub(crate) fn health(&self) -> Result<()> {
        if let Some(published) = self.published()? {
            published
                .reader
                .healthy()
                .map_err(|error| self.remember(storage_error(error)))?;
        }
        Ok(())
    }

    /// 同一二分服务字符与边界查询；端点规则来自真实调用职责而非第二套表格协议。
    fn contains_position(&self, position: usize, utf16: bool, include_start: bool) -> Result<bool> {
        let Some(published) = self.published()? else {
            return Ok(false);
        };
        let reader = &published.reader;
        let start = if utf16 {
            published.start.utf16
        } else {
            published.start.byte
        };
        let end = if utf16 {
            published.end.utf16
        } else {
            published.end.byte
        };
        if !(start..end).contains(&position) || !include_start && position == start {
            // 代码块等真正位于所有已出版表行之后的字符极多。这里使用 writer 已证明的
            // 首尾元数据，不新增热缓存或逐字符 pread；范围内的每一处空隙仍走原精确查询。
            reader
                .healthy()
                .map_err(|error| self.remember(storage_error(error)))?;
            self.health()?;
            return Ok(false);
        }
        let mut low = 0;
        let mut high = reader.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let span = self.read_from(reader, middle)?;
            let end = if utf16 { span.end.utf16 } else { span.end.byte };
            if end <= position {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low == reader.len() {
            self.health()?;
            return Ok(false);
        }
        let span = self.read_from(reader, low)?;
        let start = if utf16 {
            span.start.utf16
        } else {
            span.start.byte
        };
        self.health()?;
        Ok(if include_start {
            start <= position
        } else {
            start < position
        })
    }

    /// 原只读 File 的有限访问不持 Plan 锁；真实失败才短锁保存第一次原因。
    fn read_from(
        &self,
        reader: &LayoutIndexReader<RECORD_BYTES>,
        ordinal: usize,
    ) -> Result<TableSpan> {
        let span = reader
            .read(ordinal)
            .map(TableSpan::decode)
            .map_err(|error| self.remember(storage_error(error)))?;
        self.health()?;
        Ok(span)
    }

    /// 首次失败属于这张真实目录，后续正确记录和其它窗口都不能恢复它。
    fn remember(&self, error: MarkdownDocumentBufferError) -> MarkdownDocumentBufferError {
        let mut publication = self
            .0
            .publication
            .lock()
            .expect("原表格出版锁不得在 panic 后继续使用");
        *publication.failure.get_or_insert(error)
    }
}

/// 单一资源角色仍通过原 writer 收据观察；前缀名额是实际尚未 close 的 FD 数。
#[derive(Clone)]
pub(crate) struct TablePlanObserver {
    owner: Weak<Plan>,
    resource: Arc<Mutex<Option<LayoutIndexObserver>>>,
}
pub(crate) struct TablePlanResources {
    pub(crate) owner_released: bool,
    pub(crate) file: Option<LayoutIndexResources>,
}
impl TablePlanObserver {
    /// 前缀不带页或映射；writer 退场但原 FD 仍有前缀持有时必须继续报告占用。
    pub(crate) fn snapshot(&self) -> TablePlanResources {
        let resource = self
            .resource
            .lock()
            .expect("原表格资源锁不得在 panic 后继续使用");
        let file = resource.as_ref().map(|observer| {
            let mut actual = observer.snapshot();
            actual.file_open |= actual.prefix_readers != 0;
            actual.reader_failed |= actual.producer_failed;
            actual
        });
        TablePlanResources {
            owner_released: self.owner.strong_count() == 0,
            file,
        }
    }
}

impl TableSpan {
    /// 固定字段只编码实际结构标量，所有正文仍属于原 pieces。
    fn encode(self) -> [u8; RECORD_BYTES] {
        let mut bytes = [0; RECORD_BYTES];
        for (index, value) in [
            self.start.byte,
            self.start.utf16,
            self.end.byte,
            self.end.utf16,
            self.tail.byte,
            self.tail.utf16,
            self.header,
            self.ordinal,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[index * 8..index * 8 + 8].copy_from_slice(&(value as u64).to_le_bytes());
        }
        bytes[64] = u8::from(self.eof);
        bytes
    }
    /// 目录内容仅由当前进程的已封闭行票写入，原 Reader 已拒绝短读；不重写表格协议规则。
    fn decode(bytes: [u8; RECORD_BYTES]) -> Self {
        let get = |index: usize| {
            u64::from_le_bytes(
                bytes[index * 8..index * 8 + 8]
                    .try_into()
                    .expect("固定八字节字段"),
            ) as usize
        };
        Self {
            start: MarkdownGraphemeBoundary {
                byte: get(0),
                utf16: get(1),
            },
            end: MarkdownGraphemeBoundary {
                byte: get(2),
                utf16: get(3),
            },
            tail: MarkdownGraphemeBoundary {
                byte: get(4),
                utf16: get(5),
            },
            header: get(6),
            ordinal: get(7),
            eof: bytes[64] != 0,
        }
    }
}

/// job 每次只保一条实际结构记录，目录追加不会让既有已消费行重新计费或重放。
pub(crate) struct TableCursor {
    pub(crate) plan: TablePlan,
    next: usize,
    span: Option<TableSpan>,
}
impl TableCursor {
    /// 每个原 Unicode job 有独立数字游标，共享文件和 Source 身份而不共享可变算法状态。
    pub(crate) fn new(plan: TablePlan) -> Self {
        Self {
            plan,
            next: 0,
            span: None,
        }
    }
    /// 新取一条 record 才收费；缓存只服务当前这一个行边界。
    pub(crate) fn peek(&mut self, records: &mut usize, budget: usize) -> Result<Option<TableSpan>> {
        if self.span.is_none() && *records < budget {
            self.span = self.plan.read(self.next)?;
            if self.span.is_some() {
                *records += 1;
            }
        }
        Ok(self.span)
    }
    /// 只有原 job 的 canonical 游标真正越过该行才推进，不能用 source 查询目标代替完成。
    pub(crate) fn finish(&mut self) {
        self.next += 1;
        self.span = None;
    }
    /// 已收费的一条结构事实可用于本轮 visitor 裁界，不再次读文件或取得后继行。
    pub(crate) fn current(&self) -> Option<TableSpan> {
        self.span
    }
}

/// 共用原匿名分页存储的实际失败类别，不制造 errno 或文本内容。
fn storage_error(error: LayoutIndexError) -> MarkdownDocumentBufferError {
    MarkdownDocumentBufferError::DisplayStorageUnavailable(match error {
        LayoutIndexError::Io(error) => error.raw_os_error(),
        _ => None,
    })
}
