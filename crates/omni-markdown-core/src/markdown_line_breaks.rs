//! Unicode18 / UAX14 revision57默认断行机会。它不做宽度选行、词典、locale或Native排版。
//!
//! 同一冻结source只借用有限piece；待决规则保留固定属性状态，真实机会进入独立匿名分页
//! 索引。NoBreak不逐项存储，chunk末尾不是eot；显式feature不改变默认编辑器路径。

use crate::{
    MarkdownDocumentBufferError, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLayoutTextSourceObserver,
    markdown_layout_index::{LayoutIndexError, LayoutIndexObserver},
};
use thiserror::Error;

mod machine;
mod store;
mod unicode_data;

pub const MARKDOWN_LINE_BREAK_SOURCE_BYTES: usize = 4096;
pub const MARKDOWN_LINE_BREAK_INDEX_BYTES: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum MarkdownLineBreakError {
    #[error("默认断行扫描配额无效")]
    InvalidBudget,
    #[error("默认断行查询范围无效")]
    InvalidRange,
    #[error("默认断行源权限不匹配")]
    SourceMismatch,
    #[error("默认断行坐标或计数溢出")]
    CoordinateOverflow,
    #[error("冻结正文不是完整UTF-8")]
    InvalidUtf8,
    #[error("默认断行索引元数据不完整")]
    CorruptMetadata,
    #[error("默认断行任务已永久撤权")]
    Poisoned,
    #[error("无法借用默认断行的冻结正文")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("无法访问匿名默认断行索引")]
    Storage(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, MarkdownLineBreakError>;

impl From<LayoutIndexError> for MarkdownLineBreakError {
    fn from(value: LayoutIndexError) -> Self {
        match value {
            LayoutIndexError::InvalidRecord => Self::CorruptMetadata,
            LayoutIndexError::Overflow => Self::CoordinateOverflow,
            LayoutIndexError::Poisoned => Self::Poisoned,
            LayoutIndexError::Io(error) => Self::Storage(error),
        }
    }
}
impl From<machine::Error> for MarkdownLineBreakError {
    fn from(value: machine::Error) -> Self {
        match value {
            machine::Error::OutOfOrder => Self::CorruptMetadata,
            machine::Error::InvalidUtf8 => Self::InvalidUtf8,
            machine::Error::OffsetOverflow => Self::CoordinateOverflow,
            machine::Error::Closed | machine::Error::Poisoned => Self::Poisoned,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakKind {
    Allowed,
    Mandatory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLineBreakBoundary {
    pub byte: usize,
    pub utf16: usize,
    pub kind: MarkdownLineBreakKind,
    /// 机会发布前已读source包含尚需词典分析的SA；false不证明未读后继没有SA。
    pub needs_dictionary: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakUnit {
    Byte,
    Utf16,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakQuery {
    IsBoundary,
    Previous,
    Next,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakAnswer {
    Pending,
    Known(Option<MarkdownLineBreakBoundary>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakStatus {
    Pending,
    Ready,
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineBreakBudget {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
}
impl Default for MarkdownLineBreakBudget {
    fn default() -> Self {
        Self {
            bytes: 4096,
            scalars: 1024,
            records: 1024,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineBreakStep {
    pub status: MarkdownLineBreakStatus,
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub needs_dictionary: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineBreakStorageProfile {
    pub generation: u64,
    pub source_bytes: usize,
    pub source_utf16: usize,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
    pub boundary_records: usize,
    pub index_page_loads: u64,
    pub index_peak_mapped_bytes: usize,
    pub machine_bytes: usize,
    pub needs_dictionary: bool,
    pub unicode_version: (u8, u8, u8),
    pub revision: u8,
}

#[derive(Clone)]
pub struct MarkdownLineBreakResourceObserver {
    index: LayoutIndexObserver,
    source: MarkdownLayoutTextSourceObserver,
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineBreakResourceSnapshot {
    pub metadata_file_open: bool,
    pub metadata_map_bytes: usize,
    pub metadata_peak_map_bytes: usize,
    /// 原匿名metadata唯一写buffer的当前/历史实际heap字节；与正文映射分别计量。
    pub metadata_buffer_bytes: usize,
    pub metadata_peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    pub source_payload_released: bool,
}
impl MarkdownLineBreakResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownLineBreakResourceSnapshot {
        let index = self.index.snapshot();
        MarkdownLineBreakResourceSnapshot {
            metadata_file_open: index.file_open,
            metadata_map_bytes: index.mapped_bytes,
            metadata_peak_map_bytes: index.peak_mapped_bytes,
            metadata_buffer_bytes: index.buffer_bytes,
            metadata_peak_buffer_bytes: index.peak_buffer_bytes,
            cleanup_failed: index.cleanup_failed,
            source_payload_released: self.source.payload_released(),
        }
    }
}

/// 只拥有同一个冻结source和自己的32B机会索引；没有第二份正文、字形或全局注册表。
pub struct MarkdownLineBreakJob {
    index: store::LineBreakIndex,
    source: MarkdownLayoutTextSource,
    machine: machine::Machine,
    failed: bool,
    ready: bool,
    source_borrow_max: usize,
    #[cfg(test)]
    split: Option<usize>,
}
impl MarkdownLineBreakJob {
    /// 创建匿名元数据owner但不扫描source；调用方可让GCB与本任务共享同一个源payload。
    ///
    /// # Errors
    /// 匿名文件或页大小合同不可用时直接失败，局部资源由其原owner释放。
    pub fn new(source: MarkdownLayoutTextSource) -> Result<Self> {
        Ok(Self {
            index: store::LineBreakIndex::new()?,
            source,
            machine: machine::Machine::new(),
            failed: false,
            ready: false,
            source_borrow_max: 0,
            #[cfg(test)]
            split: None,
        })
    }
    fn guard(&mut self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        if self.failed {
            return Err(MarkdownLineBreakError::Poisoned);
        }
        if let Err(error) = self.source.health() {
            // 同一冻结源的回收失败永久撤销已发布前缀与Ready权限；保留这次真实源错误。
            self.fail();
            return Err(MarkdownLineBreakError::Source(error));
        }
        if !self.source.permits(authority) {
            self.fail();
            return Err(MarkdownLineBreakError::SourceMismatch);
        }
        Ok(())
    }
    fn fail(&mut self) {
        self.failed = true;
        self.index.poison();
    }

    /// 三个配额均严格计费；前缀机会不代表完整Ready，任何失败永久撤销整个索引权限。
    ///
    /// # Errors
    /// 外来源、零配额、UTF8/聚合不符、计数溢出或实际索引IO失败均返回原typed原因。
    pub fn step(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineBreakBudget,
    ) -> Result<MarkdownLineBreakStep> {
        self.guard(authority)?;
        let result = self.step_inner(budget);
        if result.is_err() {
            self.fail();
        }
        result
    }

    fn step_inner(&mut self, budget: MarkdownLineBreakBudget) -> Result<MarkdownLineBreakStep> {
        if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 {
            return Err(MarkdownLineBreakError::InvalidBudget);
        }
        let mut step = MarkdownLineBreakStep {
            status: MarkdownLineBreakStatus::Pending,
            bytes: 0,
            scalars: 0,
            records: 0,
            needs_dictionary: self.machine.needs_dictionary(),
        };
        if self.ready {
            step.status = MarkdownLineBreakStatus::Ready;
            return Ok(step);
        }
        while usize::try_from(self.machine.byte_offset())
            .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?
            < self.source.byte_len()
            && step.bytes < budget.bytes
            && step.scalars < budget.scalars
            && step.records < budget.records
        {
            let start = usize::try_from(self.machine.byte_offset())
                .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?;
            // 每字节至多完成一个scalar/机会；按三项余量的最小值借用，完整消费原piece回调，
            // 不用伪错误提前中止访问，也不会先读4KiB再把其中未计费的正文丢掉。
            let size = (budget.bytes - step.bytes)
                .min(budget.scalars - step.scalars)
                .min(budget.records - step.records)
                .min(MARKDOWN_LINE_BREAK_SOURCE_BYTES)
                .min(self.source.byte_len() - start);
            let end = start
                .checked_add(size)
                .ok_or(MarkdownLineBreakError::CoordinateOverflow)?;
            #[cfg(test)]
            let end = self
                .split
                .filter(|split| start < *split && *split < end)
                .unwrap_or(end);
            self.source_borrow_max = self.source_borrow_max.max(end - start);
            let before_byte = self.machine.byte_offset();
            let before_scalars = self.machine.scalar_count();
            let mut failure = None;
            let machine = &mut self.machine;
            let index = &mut self.index;
            let source_result = self.source.visit_byte_range(start..end, |bytes| {
                if failure.is_some() {
                    return Ok(());
                } // 首失败后不再写文件或消费后继piece。
                for byte in bytes {
                    if failure.is_some() {
                        break;
                    } // 实际IO已失败，后继字节不能继续取得元数据写权。
                    let mut event = None;
                    let mut duplicate = false;
                    let result =
                        machine.push(machine.byte_offset(), std::slice::from_ref(byte), |value| {
                            duplicate |= event.replace(value).is_some();
                        });
                    if let Err(error) = result {
                        failure = Some(error.into());
                        break;
                    }
                    if duplicate {
                        failure = Some(MarkdownLineBreakError::CorruptMetadata);
                        break;
                    }
                    if let Some(event) = event {
                        // 一个字节最多完成一个scalar/机会。先结束machine可变借用，再取它的
                        // 原SA事实写同一个record，不把默认AL解析错误标记为词典已经完成。
                        match index.push(event, machine.needs_dictionary()) {
                            Ok(()) => step.records += 1,
                            Err(error) => failure = Some(error),
                        }
                    }
                }
                Ok(())
            });
            if let Some(error) = failure {
                return Err(error);
            }
            source_result?;
            step.bytes += usize::try_from(self.machine.byte_offset() - before_byte)
                .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?;
            step.scalars += usize::try_from(self.machine.scalar_count() - before_scalars)
                .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?;
        }
        if usize::try_from(self.machine.byte_offset())
            .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?
            == self.source.byte_len()
        {
            if usize::try_from(self.machine.utf16_offset())
                .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?
                != self.source.utf16_len()
            {
                return Err(MarkdownLineBreakError::InvalidUtf8); // 精确source EOF也须吻合原UTF16聚合，短读不补eot。
            }
            while step.records < budget.records && !self.machine.finished() {
                if let Some(event) = self.machine.finish_one()? {
                    self.index.push(event, self.machine.needs_dictionary())?;
                    step.records += 1;
                }
            }
            if self.machine.finished() {
                self.index.seal()?;
                self.ready = true;
            }
        }
        step.needs_dictionary = self.machine.needs_dictionary();
        step.status = if self.ready {
            MarkdownLineBreakStatus::Ready
        } else {
            MarkdownLineBreakStatus::Pending
        };
        Ok(step)
    }

    /// 只查询原source已经认证的前缀；Previous/Next为严格前驱/后继，未知后继保持Pending。
    ///
    /// # Errors
    /// 越界、外来权限或元数据IO错误永久撤权；内部UTF8/低代理项坐标只能是非边界。
    pub fn query(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        unit: MarkdownLineBreakUnit,
        offset: usize,
        query: MarkdownLineBreakQuery,
    ) -> Result<MarkdownLineBreakAnswer> {
        self.guard(authority)?;
        let result = self.query_inner(unit, offset, query);
        if result.is_err() {
            self.fail();
        }
        result
    }
    fn query_inner(
        &mut self,
        unit: MarkdownLineBreakUnit,
        offset: usize,
        query: MarkdownLineBreakQuery,
    ) -> Result<MarkdownLineBreakAnswer> {
        let position = |value: MarkdownLineBreakBoundary| match unit {
            MarkdownLineBreakUnit::Byte => value.byte,
            MarkdownLineBreakUnit::Utf16 => value.utf16,
        };
        let maximum = match unit {
            MarkdownLineBreakUnit::Byte => self.source.byte_len(),
            MarkdownLineBreakUnit::Utf16 => self.source.utf16_len(),
        };
        if offset > maximum {
            return Err(MarkdownLineBreakError::InvalidRange);
        }
        let (byte, utf16) = self.machine.confirmed_offsets();
        let frontier = usize::try_from(match unit {
            MarkdownLineBreakUnit::Byte => byte,
            MarkdownLineBreakUnit::Utf16 => utf16,
        })
        .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?;
        let mut low = 0;
        let mut high = self.index.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if position(self.index.get(middle)?) < offset {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let exact = if low < self.index.len() {
            Some(self.index.get(low)?)
        } else {
            None
        };
        match query {
            MarkdownLineBreakQuery::IsBoundary => {
                if let Some(value) = exact.filter(|value| position(*value) == offset) {
                    return Ok(MarkdownLineBreakAnswer::Known(Some(value)));
                }
                if self.ready || offset < frontier || (offset == 0 && self.source.byte_len() != 0) {
                    Ok(MarkdownLineBreakAnswer::Known(None))
                } else {
                    Ok(MarkdownLineBreakAnswer::Pending)
                }
            }
            MarkdownLineBreakQuery::Previous => {
                if !self.ready && offset > frontier {
                    return Ok(MarkdownLineBreakAnswer::Pending);
                }
                Ok(MarkdownLineBreakAnswer::Known(if low == 0 {
                    None
                } else {
                    Some(self.index.get(low - 1)?)
                }))
            }
            MarkdownLineBreakQuery::Next => {
                if exact.is_some_and(|value| position(value) == offset) {
                    low += 1;
                }
                if low < self.index.len() {
                    return Ok(MarkdownLineBreakAnswer::Known(Some(self.index.get(low)?)));
                }
                Ok(if self.ready {
                    MarkdownLineBreakAnswer::Known(None)
                } else {
                    MarkdownLineBreakAnswer::Pending
                })
            }
        }
    }

    pub fn cancel(&mut self) {
        self.fail();
    }
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.failed && self.ready
    }
    #[must_use]
    pub fn storage_profile(&self) -> MarkdownLineBreakStorageProfile {
        MarkdownLineBreakStorageProfile {
            generation: self.source.generation(),
            source_bytes: self.source.byte_len(),
            source_utf16: self.source.utf16_len(),
            source_bytes_read: usize::try_from(self.machine.byte_offset())
                .expect("只消费不超过原usize源长度的借用"),
            source_borrow_max_bytes: self.source_borrow_max,
            boundary_records: self.index.len(),
            index_page_loads: self.index.page_loads(),
            index_peak_mapped_bytes: self.index.resource_observer().snapshot().peak_mapped_bytes,
            machine_bytes: std::mem::size_of::<machine::Machine>(),
            needs_dictionary: self.machine.needs_dictionary(),
            unicode_version: (18, 0, 0),
            revision: 57,
        }
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLineBreakResourceObserver {
        MarkdownLineBreakResourceObserver {
            index: self.index.resource_observer(),
            source: self.source.resource_observer(),
        }
    }
    /// 归还本索引真实FD和唯一写buffer；其他source克隆仍独立有效，不能强制清空其不可变正文。
    ///
    /// # Errors
    /// 实际写入/close失败保留负面资源收据，不能被已poison状态改写成释放成功。
    pub fn close(mut self) -> Result<()> {
        self.index.close()
    }
}

#[cfg(test)]
mod tests;
