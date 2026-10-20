//! Unicode 18 扩展字素的分页索引；正文只借用原冻结piece，不建立第二份正文文件或编码镜像。
//!
//! 本模块只产生原 Source 的byte/UTF16边界，不计算软行、字体或Native几何。已扫描前缀可以查询；
//! 未确认的后继仍为Pending，真实EOF才是eot。`paged-graphemes`不会自动切换Native路径。

use thiserror::Error;

use crate::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownDocumentBufferError, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownLayoutTextSourceObserver,
};

mod machine;
#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod table_rows;
#[cfg(feature = "paged-shaping-frontier")]
pub use prefix::{MarkdownGraphemePrefix, MarkdownGraphemePrefixFacts, MarkdownGraphemePrefixRead};
mod store;
mod unicode_data;

#[cfg(feature = "paged-scripts")]
pub(crate) use machine::ScalarCursor as MarkdownGraphemeScalarCursor;

/// 每次正文借用的硬上限；即使一个编辑piece更长，交付范围仍不超过此值。
pub const MARKDOWN_GRAPHEME_SOURCE_BYTES: usize = MARKDOWN_LAYOUT_SOURCE_BYTES;
/// 一个索引只拥有这一页；页切换先同步并归还旧映射，不按文档长度增加RAM缓存。
pub const MARKDOWN_GRAPHEME_INDEX_BYTES: usize =
    crate::markdown_layout_index::MARKDOWN_LAYOUT_INDEX_BYTES;

#[derive(Debug, Error)]
pub enum MarkdownGraphemeError {
    #[error("字素扫描配额无效")]
    InvalidBudget,
    #[error("字素查询范围无效")]
    InvalidRange,
    #[error("字素查询位于显式折叠的隐藏内部")]
    FoldedRange,
    #[error("字素查询位于独立表格行内部，须使用该格显示来源")]
    TableRange,
    #[error("字素源权限不匹配")]
    SourceMismatch,
    #[error("字素坐标或计数溢出")]
    CoordinateOverflow,
    #[error("冻结正文不是完整UTF-8")]
    InvalidUtf8,
    #[error("字素索引元数据不完整")]
    CorruptMetadata,
    #[error("字素任务已因失败或取消永久撤权")]
    Poisoned,
    #[error("无法借用冻结正文")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("无法访问匿名字素索引")]
    Storage(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, MarkdownGraphemeError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGraphemeBoundary {
    pub byte: usize,
    pub utf16: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeUnit {
    Byte,
    Utf16,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeQuery {
    IsBoundary,
    Previous,
    Next,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeAnswer {
    Pending,
    Known(Option<MarkdownGraphemeBoundary>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeStatus {
    Pending,
    Ready,
}

/// 索引序号所属的实际记录域；两种域的 byte/UTF16 仍使用同一完整 Source 坐标。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeOrdinalDomain {
    Source,
    /// 仅可见边界与折叠两端；相邻序号之间可能是未分析的显式隐藏范围。
    FoldedVisible,
    /// 独立表格块内部没有正文 GCB 权限；序号只覆盖真正正文与结构行两端。
    TableVisible,
}

/// 三个独立计费维度；零配额不产生一个可重试的空Pending。
#[derive(Clone, Copy, Debug)]
pub struct MarkdownGraphemeBudget {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
}
impl Default for MarkdownGraphemeBudget {
    fn default() -> Self {
        Self {
            bytes: MARKDOWN_GRAPHEME_SOURCE_BYTES,
            scalars: 1024,
            records: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownGraphemeStep {
    pub status: MarkdownGraphemeStatus,
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownGraphemeStorageProfile {
    pub generation: u64,
    pub source_bytes: usize,
    pub source_utf16: usize,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
    /// 当前显式记录域中已证明的逻辑边界数；Dense不为这些边界逐一分配存储行。
    pub boundary_count: usize,
    /// 实际匿名索引中的16B行数；Dense始终为0。
    pub boundary_records: usize,
    pub dense_printable_ascii: bool,
    /// 真正成功的索引固定行读取/写入数，不含Dense整数计算。
    pub index_records_read: u64,
    pub index_records_written: u64,
    pub index_page_loads: u64,
    pub index_peak_mapped_bytes: usize,
    pub unicode_version: (u8, u8, u8),
}

/// 只含固定原子元数据；观察者在任务释放后仍可核对真实Drop，不延长任何正文/文件/映射生命。
#[derive(Clone)]
pub struct MarkdownGraphemeResourceObserver {
    index: store::BoundaryResourceObserver,
    source_dropped: MarkdownLayoutTextSourceObserver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGraphemeResourceSnapshot {
    /// Paged曾实际分配匿名FD；Dense=false不能被解释为已释放过真实映射。
    pub metadata_storage_allocated: bool,
    /// Paged真实资源已归还，或Dense原owner实际close/Drop；均不延长Source生命。
    pub metadata_owner_released: bool,
    pub metadata_file_open: bool,
    pub metadata_map_bytes: usize,
    pub metadata_peak_map_bytes: usize,
    /// 原匿名metadata唯一写buffer的当前/历史实际heap字节；与正文映射分别计量。
    pub metadata_buffer_bytes: usize,
    pub metadata_peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    /// 只证明本source payload的Snapshot字段已Drop，不冒称所有其他快照或Native值均已释放。
    pub source_payload_released: bool,
}
impl MarkdownGraphemeResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownGraphemeResourceSnapshot {
        let index = self.index.snapshot();
        MarkdownGraphemeResourceSnapshot {
            metadata_storage_allocated: index.allocated,
            metadata_owner_released: index.released,
            metadata_file_open: index.file_open,
            metadata_map_bytes: index.mapped_bytes,
            metadata_peak_map_bytes: index.peak_mapped_bytes,
            metadata_buffer_bytes: index.buffer_bytes,
            metadata_peak_buffer_bytes: index.peak_buffer_bytes,
            cleanup_failed: index.cleanup_failed,
            source_payload_released: self.source_dropped.payload_released(),
        }
    }
}

/// 一个原Source与一种封闭边界表示；普通Unicode保持续机器/匿名文件，Dense只取完整原证明。
/// 未知区域不会通过空索引变成Ready，已证明Dense也仍受同一健康/权限撤销门约束。
pub struct MarkdownGraphemeJob {
    // index先释放真实映射/FD，再交还唯一源payload；observer自己仅保留固定计数。
    index: store::BoundaryIndex,
    source: MarkdownLayoutTextSource,
    machine: machine::Machine,
    failed: bool,
    // 全局游标可跨过显式隐区；实际读取必须另按真正解引用的 byte 累加。
    source_bytes_read: usize,
    source_borrow_max: usize,
    #[cfg(feature = "paged-line-compose")]
    folds: Option<FoldCursor>,
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    tables: Option<table_rows::State>,
    #[cfg(test)]
    split: Option<usize>,
}

/// 各算法共享同一端点证明；本 job 只保留自己的推进位置，不复制范围表或正文。
#[cfg(feature = "paged-line-compose")]
struct FoldCursor {
    plan: crate::markdown_layout_source::folds::FoldPlan,
    next: usize,
    tail: bool,
}

impl MarkdownGraphemeJob {
    /// 同冻结源全Printable ASCII可直接证明整数GCB；其余源创建原匿名分页owner。
    /// 不扫描正文，也不从同generation、抽样或相同字节长度推导字符域。
    ///
    /// # Errors
    /// 原Source、坐标或匿名资源失败均保留typed首因；局部owner随失败真实归还。
    pub fn new(source: MarkdownLayoutTextSource) -> Result<Self> {
        source.health()?;
        #[cfg(feature = "paged-shaping-frontier")]
        if source.is_printable_ascii_range(0..source.utf16_len())? {
            if source.byte_len() != source.utf16_len() {
                return Err(MarkdownGraphemeError::CorruptMetadata);
            }
            let index = store::BoundaryIndex::dense_ascii(source.byte_len())?;
            return Self::with_index(source, index);
        } // 只问完整原Snapshot范围；mixed根直接取精确聚合，不读partial叶或新增Source证明。
        Self::with_index(source, store::BoundaryIndex::new()?)
    }

    fn with_index(source: MarkdownLayoutTextSource, index: store::BoundaryIndex) -> Result<Self> {
        let mut machine = machine::Machine::default();
        if index.is_dense() {
            // 整源精确证明已经给出真实EOF；这里是逻辑进度，不冒称读过N个正文byte。
            machine.byte = source.byte_len();
            machine.utf16 = source.utf16_len();
            machine.frontier = Some(MarkdownGraphemeBoundary {
                byte: machine.byte,
                utf16: machine.utf16,
            });
            machine.ready = true;
        }
        source.health()?;
        Ok(Self {
            index,
            source,
            machine,
            failed: false,
            source_bytes_read: 0,
            source_borrow_max: 0,
            #[cfg(feature = "paged-line-compose")]
            folds: None,
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            tables: None,
            #[cfg(test)]
            split: None,
        })
    }

    /// 只在尚未读取原源时装入同 Source 的显式折叠；普通无折叠任务的表示与序号不变。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn install_fold_plan(
        &mut self,
        plan: crate::markdown_layout_source::folds::FoldPlan,
    ) -> Result<()> {
        self.guard(plan.authority())?;
        if plan.spans().is_empty() {
            return Ok(());
        } // 空显示意图不替换 Dense 或增加普通任务的第二套初始化状态。
        if self.machine.byte != 0 || self.machine.ready || self.index.len() != 0 {
            self.failed = true;
            return Err(MarkdownGraphemeError::InvalidRange);
        }
        self.folds = Some(FoldCursor {
            plan,
            next: 0,
            tail: false,
        });
        self.source.health().inspect_err(|_| self.failed = true)?;
        Ok(())
    }

    /// 计数和 prefix 读取都以此域解释序号；折叠域绝不声称提供原全源 GCB ordinal。
    #[must_use]
    pub(crate) fn ordinal_domain(&self) -> MarkdownGraphemeOrdinalDomain {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if self.tables.is_some() {
            return MarkdownGraphemeOrdinalDomain::TableVisible;
        }
        #[cfg(feature = "paged-line-compose")]
        if self.folds.is_some() {
            return MarkdownGraphemeOrdinalDomain::FoldedVisible;
        }
        MarkdownGraphemeOrdinalDomain::Source
    }

    /// 原流式/真实FD故障oracle显式走分页表示，不让Dense跳过被测I/O或分块边界。
    #[cfg(test)]
    fn new_paged(source: MarkdownLayoutTextSource) -> Result<Self> {
        source.health()?;
        Self::with_index(source, store::BoundaryIndex::new()?)
    }

    fn guard(&mut self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        if let Err(error) = self.source.health() {
            self.failed = true;
            return Err(error.into()); // 同源别的reader已撤权时，旧Ready/metadata也不能继续成功。
        }
        if self.failed {
            return Err(MarkdownGraphemeError::Poisoned);
        }
        if !self.source.permits(authority) {
            // 外来票据永久撤销此任务；不能先回采较新source再继续使用原索引。
            self.failed = true;
            return Err(MarkdownGraphemeError::SourceMismatch);
        }
        Ok(())
    }

    /// 有界推进原source。回调借用不逃逸，不建立正文String；最多4B残余跨piece/step继续。
    ///
    /// # Errors
    /// 无效权限、配额、正文或IO使任务永久撤权；即使旧页仍在RAM也不能再发布成功查询。
    pub fn step(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownGraphemeBudget,
    ) -> Result<MarkdownGraphemeStep> {
        self.guard(authority)?;
        if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 {
            self.failed = true;
            return Err(MarkdownGraphemeError::InvalidBudget);
        }
        let result = self.advance(budget);
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        };
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn advance(&mut self, budget: MarkdownGraphemeBudget) -> Result<MarkdownGraphemeStep> {
        let mut step = MarkdownGraphemeStep {
            status: MarkdownGraphemeStatus::Pending,
            bytes: 0,
            scalars: 0,
            records: 0,
        };
        if self.machine.ready {
            step.status = MarkdownGraphemeStatus::Ready;
            return Ok(step);
        }
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if table_rows::advance(self, budget, &mut step)? {
            return Ok(step);
        }
        let source = &self.source;
        let start = self.machine.byte;
        let end = start
            .saturating_add(budget.bytes.min(MARKDOWN_GRAPHEME_SOURCE_BYTES))
            .min(self.source.byte_len());
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        let end = table_rows::read_end(self, end);
        #[cfg(feature = "paged-line-compose")]
        let fold = self
            .folds
            .as_ref()
            .and_then(|cursor| cursor.plan.spans().get(cursor.next).copied())
            .map(|span| (span, self.folds.as_ref().is_some_and(|cursor| cursor.tail)));
        #[cfg(feature = "paged-line-compose")]
        let end = fold.map_or(end, |(span, tail)| {
            end.min(if tail {
                span.end.byte
            } else if start < span.start.byte {
                span.start.byte
            } else {
                span.header_end.byte
            })
        }); // 每次借用止于真正控制边界；隐藏行体从不进入 visitor。
        #[cfg(test)]
        let end = self
            .split
            .filter(|&split| split > start)
            .map_or(end, |split| end.min(split));
        // 本范围可以在UTF8内部结束；只有Machine拥有原连续状态，visit的结束不授予eot。
        let mut failure = None;
        let mut stopped = false;
        let result = source.visit_byte_range(start..end, |piece| {
            self.source_borrow_max = self.source_borrow_max.max(piece.len());
            for byte in piece {
                // 迭代只产出借用；先核配额再显式解引用，不能预读下一byte后仍记作零读取。
                if step.scalars == budget.scalars || step.records == budget.records {
                    // visitor错误仅作立即停止的控制信号；下面独立区分它与实际Source错误。
                    stopped = true;
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                self.source_bytes_read += 1;
                #[cfg(feature = "paged-line-compose")]
                let pushed = if fold.is_some_and(|(_, tail)| tail) {
                    self.machine
                        .push_fold_tail(*byte, &mut self.index, &mut step)
                } else {
                    self.machine.push(*byte, &mut self.index, &mut step)
                };
                #[cfg(not(feature = "paged-line-compose"))]
                let pushed = self.machine.push(*byte, &mut self.index, &mut step);
                if let Err(error) = pushed {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            // 此唯一sentinel只允许由本回调的配额暂停或本地机器失败产生；真实回收错误必须优先传播。
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
                if stopped || failure.is_some() => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(error) = failure {
            return Err(error);
        }
        #[cfg(feature = "paged-line-compose")]
        if let Some((span, tail)) = fold {
            if tail && self.machine.byte == span.end.byte {
                if self.machine.utf16 != span.end.utf16 || self.machine.used != 0 {
                    return Err(MarkdownGraphemeError::CorruptMetadata);
                }
                let cursor = self
                    .folds
                    .as_mut()
                    .expect("原 fold cursor 仍由本次推进持有");
                cursor.next += 1;
                cursor.tail = false;
            } else if !tail && self.machine.byte == span.header_end.byte {
                // 首 CRLF 已由原 DFA 完整消费；仅末真实 CRLF 还需读，其他隐藏内容不解码。
                let target = if span.header_end == span.end {
                    span.end
                } else {
                    span.tail_start
                };
                self.machine
                    .jump_fold(span.header_end, target, span.start)?;
                let cursor = self
                    .folds
                    .as_mut()
                    .expect("原 fold cursor 仍由本次推进持有");
                if target == span.end {
                    cursor.next += 1;
                    cursor.tail = false;
                } else {
                    cursor.tail = true;
                }
            }
        }
        if self.machine.byte == self.source.byte_len() {
            // 只在真实冻结源EOF结算；完整UTF16聚合和逐标量计数必须一致，不能用短读补eot。
            if self.machine.used != 0 || self.machine.utf16 != self.source.utf16_len() {
                return Err(MarkdownGraphemeError::InvalidUtf8);
            }
            if step.records < budget.records {
                self.machine.finish(&mut self.index, &mut step)?;
            }
        }
        Ok(step)
    }

    /// 只查询当前显式域已经发布的边界；Previous/Next 是该域的严格前驱/后继。
    /// 普通域允许观察UTF8内部或低代理项坐标，折叠隐藏内部则以 FoldedRange 明确拒绝。
    ///
    /// # Errors
    /// 越界、外来权限或元数据错误永久撤权；FoldedRange 保留健康任务供可见位置继续使用。
    pub fn query(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        unit: MarkdownGraphemeUnit,
        offset: usize,
        query: MarkdownGraphemeQuery,
    ) -> Result<MarkdownGraphemeAnswer> {
        self.guard(authority)?;
        let result = self.query_inner(unit, offset, query);
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        let result = result.and_then(|answer| {
            if let (MarkdownGraphemeAnswer::Known(Some(value)), Some(state)) =
                (&answer, &self.tables)
                && state.cursor.plan.contains(value.utf16, true)?
            {
                return Err(MarkdownGraphemeError::TableRange);
            }
            Ok(answer)
        });
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        };
        if result.as_ref().is_err_and(|error| {
            !matches!(
                error,
                MarkdownGraphemeError::FoldedRange | MarkdownGraphemeError::TableRange
            )
        }) {
            self.failed = true;
        }
        result
    }

    /// 只在当前记录域真实Ready后暴露record数；须与 ordinal_domain 一起解释。
    /// None 表示未知，不能被行输出解释为零个边界或原全源总数。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn ready_boundary_count(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<Option<usize>> {
        // 同源首I/O先于旧failed位；先前取消或越界不应隐藏后来实际映射故障。
        self.source.health().inspect_err(|_| self.failed = true)?;
        self.guard(authority)?;
        let count = self.machine.ready.then_some(self.index.len());
        self.source.health().inspect_err(|_| self.failed = true)?;
        Ok(count)
    }

    /// 迁移已经完成的原GCB表，一次≤256条；不泄露页借用、源强owner或可变索引。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn read_ready_boundaries(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        range: std::ops::Range<usize>,
        output: &mut [MarkdownGraphemeBoundary],
    ) -> Result<bool> {
        let result = (|| {
            self.source.health()?;
            self.guard(authority)?;
            if range.end.checked_sub(range.start) != Some(output.len()) || output.len() > 256 {
                return Err(MarkdownGraphemeError::InvalidRange);
            }
            if !self.machine.ready {
                return Ok(false);
            } // 未Ready不读输出，也不把已知前缀当完整表。
            self.index.read_batch(range, output)?;
            self.guard(authority)?;
            Ok(true)
        })();
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        };
        if result.is_err() {
            self.failed = true;
        } // IO/参数/外来票失败都不能复活旧Ready或已写前缀。
        result
    }

    fn query_inner(
        &mut self,
        unit: MarkdownGraphemeUnit,
        offset: usize,
        query: MarkdownGraphemeQuery,
    ) -> Result<MarkdownGraphemeAnswer> {
        let position = |b: MarkdownGraphemeBoundary| match unit {
            MarkdownGraphemeUnit::Byte => b.byte,
            MarkdownGraphemeUnit::Utf16 => b.utf16,
        };
        let maximum = match unit {
            MarkdownGraphemeUnit::Byte => self.source.byte_len(),
            MarkdownGraphemeUnit::Utf16 => self.source.utf16_len(),
        };
        if offset > maximum {
            return Err(MarkdownGraphemeError::InvalidRange);
        }
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(tables) = &self.tables
            && tables
                .cursor
                .plan
                .contains(offset, unit == MarkdownGraphemeUnit::Utf16)?
        {
            return Err(MarkdownGraphemeError::TableRange);
        }
        #[cfg(feature = "paged-line-compose")]
        if self.folds.as_ref().is_some_and(|cursor| {
            cursor
                .plan
                .hides(offset, unit == MarkdownGraphemeUnit::Utf16)
        }) {
            return Err(MarkdownGraphemeError::FoldedRange);
        } // 合法的隐藏内部查询是显示域拒绝，不伪造不存在的全源边界，也不损坏健康 job。
        if self.index.is_dense() {
            // 原精确证明使每个整数位置均为GCB，严格Previous/Next只做checked坐标运算。
            // 虽然无页读取，外层仍在前后检查同Source健康与原authority。
            let ordinal = match query {
                MarkdownGraphemeQuery::IsBoundary => Some(offset),
                MarkdownGraphemeQuery::Previous => offset.checked_sub(1),
                MarkdownGraphemeQuery::Next if offset == maximum => None,
                MarkdownGraphemeQuery::Next => Some(
                    offset
                        .checked_add(1)
                        .ok_or(MarkdownGraphemeError::CoordinateOverflow)?,
                ),
            };
            return Ok(MarkdownGraphemeAnswer::Known(ordinal.map(|position| {
                MarkdownGraphemeBoundary {
                    byte: position,
                    utf16: position,
                }
            })));
        }
        let known = self.machine.frontier.is_some_and(|b| position(b) >= offset);
        if !known
            && matches!(
                query,
                MarkdownGraphemeQuery::IsBoundary | MarkdownGraphemeQuery::Previous
            )
        {
            // 原稳定前沿尚未分类这个位置，两种查询原本也只能返回Pending。
            // 不为一个必定未知的结果遍历旧记录；外层Source健康后验与表格/折叠拒绝仍照常生效。
            return Ok(MarkdownGraphemeAnswer::Pending);
        }
        let mut low = 0;
        let mut high = self.index.len();
        // 只保本次二分真正读到的两侧边界，不跨query保存，也不添索引页或来源缓存。
        // 循环结束时左值就是严格前驱，右值就是lower_bound，不能再读同一原record来取结果。
        let mut previous = None;
        let mut following = None;
        if let Some(page) = self.index.append_page_records()?
            && !page.is_empty()
        {
            // 当前前沿通常位于原16KiB追加页内。先消费该页首条真实记录，便可在RAM区间
            // 内完成后继二分；旧位置仍查原FD，不预读旧页，也不增加驻留记录或来源强边。
            let first = self.index.get(page.start)?;
            if position(first) < offset {
                low = page.start + 1;
                high = page.end;
                previous = Some(first);
            } else {
                high = page.start;
                following = Some(first); // 旧区间的严格Next可能恰好就是追加页首记录。
                if position(first) == offset {
                    low = page.start;
                    if matches!(query, MarkdownGraphemeQuery::Previous) && page.start != 0 {
                        // 页首不能拿自身冒充严格前驱；上一条仍必须读取原FD的真实记录。
                        previous = Some(self.index.get(page.start - 1)?);
                    }
                }
            }
        }
        while low < high {
            let middle = low + (high - low) / 2;
            let boundary = self.index.get(middle)?;
            if position(boundary) < offset {
                low = middle + 1;
                previous = Some(boundary);
            } else {
                high = middle;
                following = Some(boundary);
            }
        }
        match query {
            MarkdownGraphemeQuery::IsBoundary => Ok(MarkdownGraphemeAnswer::Known(
                following.filter(|boundary| position(*boundary) == offset),
            )),
            MarkdownGraphemeQuery::Previous => Ok(MarkdownGraphemeAnswer::Known(previous)),
            MarkdownGraphemeQuery::Next => {
                if let Some(boundary) = following {
                    if position(boundary) != offset {
                        return Ok(MarkdownGraphemeAnswer::Known(Some(boundary)));
                    }
                    // Next是严格后继；当前边界已由二分交付，只在相等时再读真正下一条记录。
                    low += 1;
                }
                if low < self.index.len() {
                    return Ok(MarkdownGraphemeAnswer::Known(Some(self.index.get(low)?)));
                }
                Ok(if self.machine.ready {
                    MarkdownGraphemeAnswer::Known(None)
                } else {
                    MarkdownGraphemeAnswer::Pending
                })
            }
        }
    }

    /// 取消仅撤销权限，不强制清空仍可能被外部克隆持有的不可变值；真实释放由Drop/close观察。
    pub fn cancel(&mut self) {
        self.failed = true;
    }
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.failed && self.machine.ready && self.source.health().is_ok()
    }
    #[must_use]
    pub fn storage_profile(&self) -> MarkdownGraphemeStorageProfile {
        MarkdownGraphemeStorageProfile {
            generation: self.source.generation(),
            source_bytes: self.source.byte_len(),
            source_utf16: self.source.utf16_len(),
            source_bytes_read: self.source_bytes_read,
            source_borrow_max_bytes: self.source_borrow_max,
            boundary_count: self.index.len(),
            boundary_records: self.index.physical_records(),
            dense_printable_ascii: self.index.is_dense(),
            index_records_read: self.index.io_counts().0,
            index_records_written: self.index.io_counts().1,
            index_page_loads: self.index.page_loads(),
            index_peak_mapped_bytes: self.index.resource_observer().snapshot().peak_mapped_bytes,
            unicode_version: (18, 0, 0),
        }
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownGraphemeResourceObserver {
        MarkdownGraphemeResourceObserver {
            index: self.index.resource_observer(),
            source_dropped: self.source.resource_observer(),
        }
    }

    /// 显式归还本索引的真实映射和FD；失败仍归还可归还的资源，但不会签发成功释放。
    ///
    /// # Errors
    /// 既有写入或close失败保留原IO负面事实。外部source克隆的生命周期独立，不在此强制撤销。
    pub fn close(mut self) -> Result<()> {
        let result = self.index.close();
        self.source.health()?; // 即使源已失败也先归还实际索引，随后仍保原Source首因。
        result
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-line-compose",
    feature = "paged-shaping-frontier"
))]
impl MarkdownGraphemeJob {
    /// 结构跳跃只在原 machine 已消费真实行尾后完成，尚未提交端点保持待决。
    pub(crate) fn table_passed(&self, end: usize) -> bool {
        self.machine.utf16 >= end
            && self
                .machine
                .last_committed_boundary()
                .is_some_and(|value| value.utf16 >= end)
    }
}
