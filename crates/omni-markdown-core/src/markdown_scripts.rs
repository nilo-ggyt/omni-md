//! Unicode18脚本run的有限扫描；正文窗口不构成段落、字素或脚本边界。
//!
//! 策略明确采用cluster首个specific脚本的完整SCX、Common/Inherited上下文、实际配对
//! 括号的外层脚本。候选仍有多个时依次取兼容前run、primary、稳定ISO tag序；它不是
//! Unicode规定的唯一itemization。字体、UBA level与宽度另在下游相交，不在此猜测。
use crate::{
    MarkdownDocumentBufferError, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLayoutTextSourceObserver, markdown_layout_index::LayoutIndexObserver,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use thiserror::Error;

#[cfg(feature = "paged-line-compose")]
mod folds;
mod machine;
#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod table_rows;
#[cfg(feature = "paged-shaping-frontier")]
pub use prefix::{
    MarkdownScriptPrefixAnswer, MarkdownScriptPrefixKind, MarkdownStableScriptPrefix,
};
#[cfg(feature = "paged-shaping-frontier")]
mod printable;
mod properties;
mod store;
#[cfg(test)]
mod tests;
mod unicode_data;

pub const MARKDOWN_SCRIPT_SOURCE_BYTES: usize = 4096;
pub const MARKDOWN_SCRIPT_INDEX_BYTES: usize =
    2 * crate::markdown_layout_index::MARKDOWN_LAYOUT_INDEX_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownScript(u8);
impl MarkdownScript {
    /// 返回规范ISO15924 tag；调用shaper时可用其公开tag构造器，不暴露vendor内部序号。
    pub fn iso15924_tag(self) -> [u8; 4] {
        unicode_data::TAGS[usize::from(self.0)]
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownScriptRun {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_utf16: usize,
    pub end_utf16: usize,
    pub script: MarkdownScript,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownScriptUnit {
    Byte,
    Utf16,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownScriptAnswer {
    Pending,
    Known(Option<MarkdownScriptRun>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownScriptStatus {
    Pending,
    Ready,
}

#[derive(Clone, Default)]
pub struct MarkdownScriptCancellation(Arc<AtomicBool>);
impl MarkdownScriptCancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(MarkdownScriptError::Cancelled)
        } else {
            Ok(())
        }
    }
}
#[derive(Debug, Error)]
pub enum MarkdownScriptError {
    #[error("脚本扫描配额无效")]
    InvalidBudget,
    #[error("脚本查询范围无效")]
    InvalidRange,
    #[cfg(feature = "paged-line-compose")]
    #[error("该原 Source 范围已折叠，没有可见脚本权限")]
    FoldedRange,
    #[error("独立表格行内部须查询该格显示来源的脚本")]
    TableRange,
    #[error("脚本源权限不匹配")]
    SourceMismatch,
    #[error("脚本坐标或计数溢出")]
    CoordinateOverflow,
    #[error("冻结脚本源不是完整UTF8")]
    InvalidUtf8,
    #[error("脚本索引元数据不完整")]
    CorruptMetadata,
    #[error("脚本任务已取消")]
    Cancelled,
    #[error("脚本任务已永久撤权")]
    Poisoned,
    #[error("无法读取冻结脚本源")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("无法访问匿名脚本索引")]
    Storage(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, MarkdownScriptError>;

/// record写入和固定metadata操作分别计费；深括号栈可以跨step继续，不能截断为32层。
#[derive(Clone, Copy, Debug)]
pub struct MarkdownScriptBudget {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub stack_ops: usize,
}
impl Default for MarkdownScriptBudget {
    fn default() -> Self {
        Self {
            bytes: 4096,
            scalars: 1024,
            records: 1024,
            stack_ops: 1024,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownScriptStep {
    pub status: MarkdownScriptStatus,
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub stack_ops: usize,
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownScriptStorageProfile {
    pub generation: u64,
    pub source_bytes: usize,
    pub source_utf16: usize,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
    /// 只表示消费原整源精确摘要；不把跳过的扫描计入source_bytes_read。
    pub printable_summary_used: bool,
    pub completed_runs: usize,
    pub bracket_records: usize,
    pub run_page_loads: u64,
    pub stack_page_loads: u64,
    pub unicode_version: (u8, u8, u8),
}
#[derive(Clone)]
pub struct MarkdownScriptResourceObserver {
    runs: LayoutIndexObserver,
    frames: LayoutIndexObserver,
    source: MarkdownLayoutTextSourceObserver,
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownScriptResourceSnapshot {
    pub run_file_open: bool,
    pub stack_file_open: bool,
    pub mapped_bytes: usize,
    pub run_peak_mapped_bytes: usize,
    pub stack_peak_mapped_bytes: usize,
    /// 两个独立索引的实际工作buffer合计；各自峰值不冒充同一时刻的进程峰值。
    pub buffer_bytes: usize,
    pub run_peak_buffer_bytes: usize,
    pub stack_peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    pub source_payload_released: bool,
}
impl MarkdownScriptResourceObserver {
    pub fn snapshot(&self) -> MarkdownScriptResourceSnapshot {
        let runs = self.runs.snapshot();
        let frames = self.frames.snapshot();
        MarkdownScriptResourceSnapshot {
            run_file_open: runs.file_open,
            stack_file_open: frames.file_open,
            mapped_bytes: runs.mapped_bytes + frames.mapped_bytes,
            run_peak_mapped_bytes: runs.peak_mapped_bytes,
            stack_peak_mapped_bytes: frames.peak_mapped_bytes,
            buffer_bytes: runs.buffer_bytes + frames.buffer_bytes,
            run_peak_buffer_bytes: runs.peak_buffer_bytes,
            stack_peak_buffer_bytes: frames.peak_buffer_bytes,
            cleanup_failed: runs.cleanup_failed || frames.cleanup_failed,
            source_payload_released: self.source.payload_released(),
        }
    }
}

pub struct MarkdownScriptJob {
    store: store::Store,
    machine: machine::Machine,
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    cancellation: MarkdownScriptCancellation,
    failed: bool,
    cancelled: bool,
    source_borrow_max: usize,
    source_bytes_read: usize,
    #[cfg(feature = "paged-line-compose")]
    folds: Option<folds::State>,
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    tables: Option<table_rows::State>,
    #[cfg(feature = "paged-shaping-frontier")]
    completed_ascii_suffix: Option<(std::ops::Range<usize>, bool)>,
    #[cfg(feature = "paged-shaping-frontier")]
    printable: Option<printable::Preparation>,
}
impl MarkdownScriptJob {
    /// 仅绑定原Source与Weak权限并创建有限metadata owner，不在构造时扫描正文。
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownScriptCancellation,
    ) -> Result<Self> {
        Self::create(source, authority, cancellation, true)
    }

    /// 测试差分和故障注入始终可执行原Machine，不能让新快路替换其语义oracle。
    #[cfg(test)]
    fn new_streaming(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownScriptCancellation,
    ) -> Result<Self> {
        Self::create(source, authority, cancellation, false)
    }

    fn create(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownScriptCancellation,
        use_summary: bool,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(authority) {
            return Err(MarkdownScriptError::SourceMismatch);
        }
        cancellation.check()?; // 取消/外来身份在任何匿名FD创建前拒绝。
        #[cfg(feature = "paged-shaping-frontier")]
        let printable = if use_summary {
            // 原Source完整root的精确事实不借正文；没有证明则完整保留原Machine。
            printable::Preparation::capture(&source)?
        } else {
            None // 仅测试构造会关闭摘要，产品入口统一使用同一可验证选择。
        };
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let _ = use_summary; // 未启用新布局功能时两入口均保持原逐scalar流程。
        source.health()?;
        cancellation.check()?;
        let store = store::Store::new(&cancellation);
        source.health()?;
        let store = store?;
        cancellation.check()?; // 构造期间的真实源错误优先，已取消候选不会作为活job返回。
        Ok(Self {
            store,
            machine: machine::Machine::default(),
            source,
            authority: authority.clone(),
            cancellation,
            failed: false,
            cancelled: false,
            source_borrow_max: 0,
            source_bytes_read: 0,
            #[cfg(feature = "paged-line-compose")]
            folds: None,
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            tables: None,
            #[cfg(feature = "paged-shaping-frontier")]
            completed_ascii_suffix: None,
            #[cfg(feature = "paged-shaping-frontier")]
            printable,
        })
    }
    fn guard(&self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        self.source.health()?; // 源的真实errno优先于旧metadata或取消，命中不能绕过sticky状态。
        if self.cancelled {
            return Err(MarkdownScriptError::Cancelled);
        }
        if self.failed {
            return Err(MarkdownScriptError::Poisoned);
        }
        if authority != &self.authority || !self.source.permits(authority) {
            return Err(MarkdownScriptError::SourceMismatch);
        }
        self.cancellation.check()
    }
    fn fail(&mut self, error: &MarkdownScriptError) {
        if matches!(error, MarkdownScriptError::TableRange) {
            return;
        }
        self.failed = true;
        self.cancelled = matches!(error, MarkdownScriptError::Cancelled);
        self.store.poison();
    }
    pub fn step(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownScriptBudget,
    ) -> Result<MarkdownScriptStep> {
        if let Err(error) = self.guard(authority) {
            self.fail(&error);
            return Err(error);
        }
        if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 || budget.stack_ops == 0
        {
            // 零配额是调用错误；不能造一个永远没有工作量的Pending要求重试。
            self.fail(&MarkdownScriptError::InvalidBudget);
            return Err(MarkdownScriptError::InvalidBudget);
        }
        let result = self.advance(budget);
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result.and_then(|step| {
                self.guard(authority)?;
                Ok(step)
            }),
        }; // metadata失败/取消出口也核真实源，不能由短路掩盖同时已确认的底层errno。
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }
    fn advance(&mut self, budget: MarkdownScriptBudget) -> Result<MarkdownScriptStep> {
        let mut step = MarkdownScriptStep {
            status: MarkdownScriptStatus::Pending,
            bytes: 0,
            scalars: 0,
            records: 0,
            stack_ops: 0,
        };
        if self.machine.ready {
            step.status = MarkdownScriptStatus::Ready;
            return Ok(step);
        }
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if table_rows::advance(self, budget, &mut step)? {
            self.source_bytes_read += step.bytes;
            return Ok(step);
        }
        #[cfg(feature = "paged-line-compose")]
        if folds::advance_boundary(self, budget, &mut step)? {
            self.source_bytes_read += step.bytes;
            return Ok(step);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if let Some(preparation) = &mut self.printable {
            // 原Preparation同时负责完整摘要与上下文后缀。后者的边叶和目录也付本量子配额，
            // 不把聚合跳过的正文计入实际读取，Store未真实封存绝不能发布Ready。
            let result = preparation.advance(
                &self.source,
                &mut self.source_borrow_max,
                &mut self.store,
                &mut step,
                budget,
            );
            self.source_bytes_read += step.bytes;
            match result? {
                printable::Progress::Ready => {
                    self.machine
                        .finish_proven_eof(self.source.byte_len(), self.source.utf16_len());
                    step.status = MarkdownScriptStatus::Ready;
                }
                printable::Progress::Rejected => {
                    self.completed_ascii_suffix = preparation.range().map(|range| (range, false));
                    self.printable = None;
                }
                printable::Progress::Resume(run) => {
                    self.machine
                        .advance_proven_ascii(run.end_byte, run.end_utf16);
                    self.completed_ascii_suffix = Some((run.start_utf16..run.end_utf16, true));
                    self.printable = None;
                } // 仍为Pending；下一原量子必须实际消费B/CRLF，不能把范围证明冒充已闭run。
                printable::Progress::Pending => {}
            }
            return Ok(step);
        }
        // 先完成上次暂停的配对/记录阶段；不为了推进metadata而多读正文。
        if !self
            .machine
            .pump(&mut self.store, &mut step, budget, false)?
        {
            return Ok(step);
        }
        let start = self.machine.byte;
        let end = start
            .saturating_add(budget.bytes.min(MARKDOWN_SCRIPT_SOURCE_BYTES))
            .min(self.source.byte_len());
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        let end = table_rows::read_end(self, end);
        #[cfg(feature = "paged-line-compose")]
        let end = folds::read_end(self, end);
        let mut paused = false;
        let mut failure = None;
        let source = &self.source;
        let result = source.visit_byte_range(start..end, |piece| {
            self.source_borrow_max = self.source_borrow_max.max(piece.len());
            for byte in piece {
                if let Err(error) = self.cancellation.check() {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                if step.scalars == budget.scalars
                    || step.records == budget.records
                    || step.stack_ops == budget.stack_ops
                {
                    paused = true;
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                if let Err(error) = self.machine.push_byte(*byte, &mut step) {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                match self.machine.pump(&mut self.store, &mut step, budget, false) {
                    Ok(true) => {}
                    Ok(false) => {
                        paused = true;
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                    }
                    Err(error) => {
                        failure = Some(error);
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                    }
                }
            }
            Ok(())
        });
        self.source_bytes_read += step.bytes;
        self.source.health()?; // 回调哨兵不能吞掉真实lease归还失败或其他reader撤权。
        match result {
            Ok(()) => {}
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
                if paused || failure.is_some() => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.cancellation.check()?;
        if self.machine.byte == self.source.byte_len() {
            if !self.machine.complete_utf8(self.source.utf16_len()) {
                return Err(MarkdownScriptError::InvalidUtf8);
            }
            self.machine
                .pump(&mut self.store, &mut step, budget, true)?;
        }
        if self.machine.ready {
            step.status = MarkdownScriptStatus::Ready;
        }
        Ok(step)
    }
    /// 只消费同源真实B正文末端或EOF；有限准备额度不构成Script边界。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn prepare_ascii_suffix(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        position: usize,
        content_end: usize,
        paragraph_end: usize,
    ) -> Result<bool> {
        let result = (|| {
            self.guard(authority)?;
            if content_end > paragraph_end || paragraph_end > self.source.utf16_len() {
                return Err(MarkdownScriptError::InvalidRange);
            } // 真实同源范围不能倒置或超出原Source；错误不夹取为可用证明。
            if let Some(preparation) = &self.printable {
                return Ok(preparation
                    .range()
                    .is_some_and(|range| range.end == content_end && range.contains(&position)));
            } // 原Preparation尚在证明或封存；只能继续它实际覆盖的原范围。
            if let Some((range, positive)) = &self.completed_ascii_suffix
                && range.end == content_end
                && range.contains(&position)
            {
                return Ok(*positive);
            } // 真反例不重试；正结果仍等待原B消费，不能反复证明而饿死自然闭合步骤。
            if self.machine.ready {
                return Ok(false);
            }
            let Some(run) = self.machine.latin_suffix(position, content_end) else {
                return Ok(false);
            };
            if content_end == self.source.utf16_len()
                && self.source.byte_len() - run.start_byte != content_end - run.start_utf16
            {
                self.completed_ascii_suffix = Some((run.start_utf16..content_end, false));
                return Ok(false); // 只有真EOF有完整byte长度；非EOF不能同步转换端点或读取整段。
            }
            self.printable = Some(printable::Preparation::suffix(
                &self.source,
                run,
                content_end,
            )?);
            self.guard(authority)?;
            Ok(true)
        })();
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        };
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    pub fn run_at(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        unit: MarkdownScriptUnit,
        offset: usize,
    ) -> Result<MarkdownScriptAnswer> {
        if let Err(error) = self.guard(authority) {
            self.fail(&error);
            return Err(error);
        }
        #[cfg(feature = "paged-line-compose")]
        if folds::hidden(self, unit, offset) {
            return Err(MarkdownScriptError::FoldedRange);
        } // 真实 Source 首错已核；隐藏查询不代表任务损坏，不撤销仍有效的可见 run。
        let result = self.find_run(unit, offset);
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result.and_then(|answer| {
                self.guard(authority)?;
                Ok(answer)
            }),
        };
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }
    fn find_run(
        &mut self,
        unit: MarkdownScriptUnit,
        offset: usize,
    ) -> Result<MarkdownScriptAnswer> {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if table_rows::hidden(self, unit, offset)? {
            return Err(MarkdownScriptError::TableRange);
        }
        #[cfg(feature = "paged-line-compose")]
        if folds::hidden(self, unit, offset) {
            return Err(MarkdownScriptError::FoldedRange);
        } // 稳定前缀也不能绕过 run_at 的隐藏权限，把后继脚本误授给未分析正文。
        let end = match unit {
            MarkdownScriptUnit::Byte => self.source.byte_len(),
            MarkdownScriptUnit::Utf16 => self.source.utf16_len(),
        };
        if offset > end {
            return Err(MarkdownScriptError::InvalidRange);
        }
        // 只对连续已完成run做二分；当前候选即使已有primary仍不是已发布完整run。
        let position = |run: MarkdownScriptRun| match unit {
            MarkdownScriptUnit::Byte => run.end_byte,
            MarkdownScriptUnit::Utf16 => run.end_utf16,
        };
        let mut low = 0;
        let count = self.store.run_count();
        let mut high = count;
        let mut following = None;
        if let Some(page) = self.store.run_append_page()?
            && !page.is_empty()
        {
            // 原追加页的首条完整run已经实际写入。根据它的真实末端，可把前向查询
            // 限在这个已初始化范围；旧区域仍走原FD，不能用当前候选或页容量补覆盖。
            let first = self.store.run(page.start)?;
            if position(first) <= offset {
                low = page.start + 1;
                high = page.end;
            } else {
                high = page.start;
                following = Some(first); // 旧区全部在offset左侧时，页首本身仍可能是目标run。
            }
        }
        while low < high {
            let middle = low + (high - low) / 2;
            let run = self.store.run(middle)?;
            if position(run) <= offset {
                low = middle + 1;
            } else {
                high = middle;
                following = Some(run);
            }
        }
        if low < count {
            // 本次upper_bound已经完整解码了最终run；只保本栈Copy结果，不再读取同一
            // 记录，也不跨query存续。相等末端仍属于后一run，原byte/UTF16语义不变。
            let run = following.ok_or(MarkdownScriptError::CorruptMetadata)?;
            return Ok(MarkdownScriptAnswer::Known(Some(run)));
        }
        // 未读后继和未闭合run不能通过空结果冒充已知不存在；只有真EOF授予Known(None)。
        Ok(if self.machine.ready {
            MarkdownScriptAnswer::Known(None)
        } else {
            MarkdownScriptAnswer::Pending
        })
    }
    pub fn storage_profile(&self) -> MarkdownScriptStorageProfile {
        let (run_page_loads, stack_page_loads) = self.store.loads();
        #[cfg(feature = "paged-shaping-frontier")]
        let printable_summary_used = self.printable.is_some();
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let printable_summary_used = false;
        MarkdownScriptStorageProfile {
            generation: self.source.generation(),
            source_bytes: self.source.byte_len(),
            source_utf16: self.source.utf16_len(),
            source_bytes_read: self.source_bytes_read,
            source_borrow_max_bytes: self.source_borrow_max,
            printable_summary_used,
            completed_runs: self.store.run_count(),
            bracket_records: self.store.frame_count(),
            run_page_loads,
            stack_page_loads,
            unicode_version: unicode_data::UNICODE_VERSION,
        }
    }
    pub fn resource_observer(&self) -> MarkdownScriptResourceObserver {
        let (runs, frames) = self.store.observers();
        MarkdownScriptResourceObserver {
            runs,
            frames,
            source: self.source.resource_observer(),
        }
    }
    pub fn close(mut self) -> Result<()> {
        // 不因取消跳过清理；两个真实owner都尝试close，之后字段析构交还原Source。
        self.store.close()
    }
}

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-line-compose",
    feature = "paged-shaping-frontier"
))]
impl MarkdownScriptJob {
    /// 原 SCX 游标真正越过行尾才可恢复后继正文，已读取字节总量不代表这个事实。
    pub(crate) fn table_passed(&self, end: usize) -> bool {
        self.machine.utf16 >= end
    }
    /// 新行结构撤销可能跨该行的原 suffix 试次，后继 job 仍沿原预算和同一 Source 前进。
    pub(crate) fn table_announced(&mut self) {
        self.printable = None;
        self.completed_ascii_suffix = None;
    }
}
