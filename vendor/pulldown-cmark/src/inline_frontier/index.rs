// Copyright 2017 Google Inc. All rights reserved.
// 原MaybeCode最大run/首个同长closer的有限索引；MIT许可见LICENSE。

//! 只存raw长度的最后位置，不存所有occurrence或代码正文。
//!
//! 该索引不能重排行内优先序。只有HTML/math/link等原路径实际到达的opener才能查询；
//! 词法中存在未来run不意味着此前的HTML/math没有消费当前opener。完整扫描也不是renderer Ready。

use std::collections::BTreeMap;
use std::mem::size_of;
use std::ops::Range;

use super::{
    CodeDelimiterCandidate, CodeSpanProbe, InlineCandidate, InlineError, InlineLexicalCursor,
    InlineStepStatus, InlineWork, MAX_INLINE_FRAME_BYTES, MAX_INLINE_STEP_WORK,
};
use crate::block_frontier::SourceFrame;
use crate::inline_scan::code_search_length;

/// 单份索引的metadata硬上限；不是整产品resident内存上限，也不是正文大小限制。
pub const MAX_CODE_INDEX_METADATA_BYTES: usize = 1024 * 1024;
/// 为一次最坏小节点分裂/移动预留空间；每个step只执行一次map更新，没有批量隐藏insert。
pub const MIN_CODE_INDEX_STEP_WORK: usize = 32 * 1024;

// 固定Rust1.99的BTreeMap采用B=6：node最多11key/12edge，非root至少5key。
// 来源：https://github.com/rust-lang/rust/blob/1.99.0/library/alloc/src/collections/btree/node.rs
// 升级工具链须重核这些界限；它们不是所有Rust版本的公共布局承诺。
const NODE_KEYS: usize = 11;
const NODE_EDGES: usize = 12;
const MIN_NODE_KEYS: usize = 5;
// header留四个pointer宽度，覆盖parent/两u16/padding；包含测试key时也按实际size算。
const NODE_BYTES_BOUND: usize = 4 * size_of::<usize>()
    + NODE_KEYS * (size_of::<RunKey>() + size_of::<usize>())
    + NODE_EDGES * size_of::<usize>();

#[cfg(not(test))]
type RunKey = usize;

#[cfg(test)]
#[derive(Clone, Debug)]
struct RunKey {
    length: usize,
    comparisons: std::rc::Rc<std::cell::Cell<usize>>,
}

#[cfg(test)]
impl PartialEq for RunKey {
    fn eq(&self, other: &Self) -> bool {
        self.length == other.length
    }
}
#[cfg(test)]
impl Eq for RunKey {}
#[cfg(test)]
impl PartialOrd for RunKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
#[cfg(test)]
impl Ord for RunKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 仅纯测观测实际std比较次数；production的key就是usize，不新增计数器/每keyRc。
        self.comparisons
            .set(self.comparisons.get().saturating_add(1));
        self.length.cmp(&other.length)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeIndexError {
    InvalidMetadataBudget,
    MetadataLimit,
    InvalidBudget,
    InvalidFrame,
    InvalidOpener,
    Cancelled,
    Closed,
    Overflow,
    Scanner(InlineError),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 比较/树高/分裂移动/分配均报保守上界；源peek仍报实际次数，不混称精确CPU指令。
pub struct CodeIndexWork {
    pub lexical: InlineWork,
    pub key_comparisons_bound: usize,
    pub tree_levels_bound: usize,
    pub allocation_bytes_bound: usize,
    pub moved_metadata_bytes_bound: usize,
    pub transitions: usize,
    /// 当前活metadata上界是容量量，不再次加入本step工作量。
    pub live_metadata_bytes_bound: usize,
    #[cfg(test)]
    actual_key_comparisons: usize,
}

impl CodeIndexWork {
    pub fn total(self) -> usize {
        self.lexical.total()
            + self.key_comparisons_bound
            + self.tree_levels_bound
            + self.allocation_bytes_bound
            + self.moved_metadata_bytes_bound
            + self.transitions
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeIndexStatus {
    Progress,
    NeedInput,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeIndexStep {
    pub work: CodeIndexWork,
    pub status: CodeIndexStatus,
}

#[derive(Debug)]
pub enum IndexedCodeDecision {
    Literal,
    Unmatched { replay_from: usize },
    Search(CodeSpanProbe),
}

#[derive(Debug)]
pub struct CodeIndexLookup {
    pub work: CodeIndexWork,
    pub decision: IndexedCodeDecision,
}

#[derive(Debug)]
pub struct CodeLastRunIndexBuilder {
    revision: u64,
    source_end: usize,
    range: Range<usize>,
    cursor: Option<InlineLexicalCursor>,
    last: BTreeMap<RunKey, usize>,
    pending: Option<CodeDelimiterCandidate>,
    distinct_limit: usize,
    metadata_limit: usize,
    complete: bool,
    failure: Option<CodeIndexError>,
    #[cfg(test)]
    comparisons: std::rc::Rc<std::cell::Cell<usize>>,
}

#[derive(Debug)]
/// 只有完整构建后才能获得本类型；map没有Clone实现或公开部分last值接口。
pub struct CodeLastRunIndex {
    revision: u64,
    source_end: usize,
    range: Range<usize>,
    last: BTreeMap<RunKey, usize>,
    cancelled: bool,
    #[cfg(test)]
    comparisons: std::rc::Rc<std::cell::Cell<usize>>,
}

fn metadata_bound(entries: usize) -> Result<usize, CodeIndexError> {
    // 每个非root至少5entry；root至少1entry。因此节点数≤1+floor((entries-1)/5)。
    let nodes = if entries == 0 {
        0
    } else {
        1 + (entries - 1) / MIN_NODE_KEYS
    };
    nodes
        .checked_mul(NODE_BYTES_BOUND)
        .and_then(|bytes| bytes.checked_add(size_of::<CodeLastRunIndexBuilder>()))
        // take_complete后caller可能仍保留已封闭builder；两个固定头同时活着也计入。
        .and_then(|bytes| bytes.checked_add(size_of::<CodeLastRunIndex>()))
        // 纯测counter共享一次Rc分配；production没有该单元，不将测试开销藏在map预算外。
        .and_then(|bytes| {
            bytes.checked_add(if cfg!(test) {
                4 * size_of::<usize>()
            } else {
                0
            })
        })
        .ok_or(CodeIndexError::Overflow)
}

fn maximum_distinct_runs(bytes: usize) -> usize {
    // 每种正长度至少出现一个不重叠最大run，故1+…+K≤真实range字节数。
    // binary search至多usize::BITS轮；不以循环sqrt(bytes)制造构造时的无fuel长工作。
    let mut low = 0usize;
    let mut high = bytes;
    while low < high {
        let middle = low + (high - low) / 2 + 1;
        let product = if middle % 2 == 0 {
            middle
                .checked_add(1)
                .and_then(|b| (middle / 2).checked_mul(b))
        } else {
            middle.checked_mul(middle / 2 + 1)
        };
        if product.is_some_and(|value| value <= bytes) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

fn levels_bound(entries: usize) -> usize {
    if entries == 0 {
        return 0;
    }
    // root+两棵最小child树：L层最少2*6^(L-1)-1个entry；不把log2深度冒充实际树高。
    let mut levels = 1;
    let mut next_level_minimum = 11usize;
    while entries >= next_level_minimum {
        levels += 1;
        let Some(next) = next_level_minimum
            .checked_add(1)
            .and_then(|n| n.checked_mul(6))
            .and_then(|n| n.checked_sub(1))
        else {
            break;
        };
        next_level_minimum = next;
    }
    levels
}

fn insertion_work(entries: usize) -> Result<CodeIndexWork, CodeIndexError> {
    let next = entries.checked_add(1).ok_or(CodeIndexError::Overflow)?;
    let levels = levels_bound(next).max(1);
    // get_mut无分配先查；缺失后才insert，最坏两次search。每node最多11个key比较。
    // 分裂/shift/修child parent至多按每层8个完整node字节收费，分配另算levels+1个node。
    Ok(CodeIndexWork {
        key_comparisons_bound: 2 * NODE_KEYS * levels,
        tree_levels_bound: 2 * levels,
        allocation_bytes_bound: NODE_BYTES_BOUND * (levels + 1),
        moved_metadata_bytes_bound: 8 * NODE_BYTES_BOUND * (levels + 1),
        transitions: 8,
        ..CodeIndexWork::default()
    })
}

impl CodeLastRunIndexBuilder {
    /// 按实际range推导K，不假设任意DocBuffer调用者都采用产品的32MiB打开限制。
    /// metadata_limit在任何map节点分配前核验；超预算是未完成失败，不能当Source渲染成功。
    pub fn new(
        revision: u64,
        source_end: usize,
        range: Range<usize>,
        metadata_limit: usize,
    ) -> Result<Self, CodeIndexError> {
        let cursor = InlineLexicalCursor::new(revision, source_end, range.clone())
            .map_err(CodeIndexError::Scanner)?;
        if metadata_limit > MAX_CODE_INDEX_METADATA_BYTES || metadata_limit < metadata_bound(0)? {
            return Err(CodeIndexError::InvalidMetadataBudget);
        }
        Ok(Self {
            revision,
            source_end,
            distinct_limit: maximum_distinct_runs(range.end - range.start),
            range,
            cursor: Some(cursor),
            last: BTreeMap::new(),
            pending: None,
            metadata_limit,
            complete: false,
            failure: None,
            #[cfg(test)]
            comparisons: std::rc::Rc::new(std::cell::Cell::new(0)),
        })
    }

    pub fn next_byte(&self) -> usize {
        self.cursor
            .as_ref()
            .map_or(self.range.end, InlineLexicalCursor::next_byte)
    }

    pub fn cancel(&mut self) {
        // 同步drop所有有限map节点和scanner scratch；不保留可查询的部分索引。
        self.last.clear();
        self.pending = None;
        self.cursor = None;
        self.failure.get_or_insert(CodeIndexError::Cancelled);
        self.complete = false;
    }

    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<CodeIndexStep, CodeIndexError> {
        let result = self.advance_inner(frame, budget);
        if let Err(error) = result {
            self.last.clear();
            self.pending = None;
            self.cursor = None;
            self.failure = Some(error);
            self.complete = false;
        }
        result
    }

    fn advance_inner(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<CodeIndexStep, CodeIndexError> {
        if let Some(error) = self.failure {
            // 首错的map已丢弃；后继frame不能把未完成包装成空索引。
            return Err(error);
        }
        if self.complete {
            // 完整值只能take一次；重复advance不是新的paragraph扫描。
            return Err(CodeIndexError::Closed);
        }
        if !(MIN_CODE_INDEX_STEP_WORK..=MAX_INLINE_STEP_WORK).contains(&budget) {
            return Err(CodeIndexError::InvalidBudget);
        }
        // metadata-only step同样先核源revision/cursor/真实EOF；frame尾不授段落终止权限。
        let end = frame.byte_start.checked_add(frame.bytes.len());
        if frame.source_revision != self.revision
            || frame.byte_start != self.next_byte()
            || frame.bytes.len() > MAX_INLINE_FRAME_BYTES
            || !end.is_some_and(|end| end <= self.source_end)
            || (frame.real_eof && end != Some(self.source_end))
        {
            return Err(CodeIndexError::InvalidFrame);
        }
        if let Some(run) = self.pending {
            // 词法child上一step已经返回；现在先为完整map操作收费，再允许真实节点分配。
            let work = insertion_work(self.last.len())?;
            if work.total() > budget {
                return Err(CodeIndexError::InvalidBudget);
            }
            #[cfg(test)]
            self.comparisons.set(0);
            #[cfg(test)]
            let key = RunKey {
                length: run.raw_count,
                comparisons: self.comparisons.clone(),
            };
            #[cfg(not(test))]
            let key = run.raw_count;
            if let Some(last) = self.last.get_mut(&key) {
                // 重复长度只改最后位置，不增长occurrence集合；没有节点分配或整树复制。
                *last = run.start;
            } else {
                let entries = self
                    .last
                    .len()
                    .checked_add(1)
                    .ok_or(CodeIndexError::Overflow)?;
                // 缺失key才可能分配node；在insert前核真实K证明及完整活metadata上界。
                if entries > self.distinct_limit || metadata_bound(entries)? > self.metadata_limit {
                    return Err(CodeIndexError::MetadataLimit);
                }
                self.last.insert(key, run.start);
            }
            let work = CodeIndexWork {
                live_metadata_bytes_bound: metadata_bound(self.last.len())?,
                #[cfg(test)]
                actual_key_comparisons: self.comparisons.get(),
                ..work
            };
            self.pending = None;
            return Ok(CodeIndexStep {
                work,
                status: CodeIndexStatus::Progress,
            });
        }
        let cursor = self.cursor.as_mut().ok_or(CodeIndexError::Closed)?;
        let step = cursor
            .advance(frame, budget - 1)
            .map_err(CodeIndexError::Scanner)?;
        let status = match step.status {
            InlineStepStatus::Candidate(InlineCandidate::CodeDelimiter(run)) => {
                // 每个最大run只产生一次map更新；本step不再insert，避免child用满后隐藏额外工作。
                self.pending = Some(run);
                CodeIndexStatus::Progress
            }
            InlineStepStatus::Complete => {
                self.complete = true;
                self.cursor = None;
                CodeIndexStatus::Complete
            }
            InlineStepStatus::NeedInput => CodeIndexStatus::NeedInput,
            InlineStepStatus::Progress
            | InlineStepStatus::Candidate(InlineCandidate::Entity(_)) => CodeIndexStatus::Progress,
        };
        Ok(CodeIndexStep {
            work: CodeIndexWork {
                lexical: step.work,
                transitions: 1,
                live_metadata_bytes_bound: metadata_bound(self.last.len())?,
                ..CodeIndexWork::default()
            },
            status,
        })
    }

    /// 只移动已经完整的map；未完成时None不等于“没有代码”，失败时仍保留typed首因。
    pub fn take_complete(&mut self) -> Result<Option<CodeLastRunIndex>, CodeIndexError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if !self.complete {
            return Ok(None);
        }
        self.complete = false;
        self.failure = Some(CodeIndexError::Closed);
        Ok(Some(CodeLastRunIndex {
            revision: self.revision,
            source_end: self.source_end,
            range: self.range.clone(),
            last: std::mem::take(&mut self.last),
            cancelled: false,
            #[cfg(test)]
            comparisons: self.comparisons.clone(),
        }))
    }
}

impl CodeLastRunIndex {
    pub fn cancel(&mut self) {
        self.last.clear();
        self.cancelled = true;
    }

    /// 仅供原行内顺序实际到达的opener；Index完成不授予跳过早先HTML/math/link的权限。
    pub fn lookup(
        &self,
        opener: CodeDelimiterCandidate,
        budget: usize,
    ) -> Result<CodeIndexLookup, CodeIndexError> {
        if self.cancelled {
            return Err(CodeIndexError::Cancelled);
        }
        if opener.revision != self.revision
            || opener.start < self.range.start
            || opener.start >= opener.end
            || opener.end > self.range.end
            || opener.end.checked_sub(opener.start) != Some(opener.raw_count)
        {
            return Err(CodeIndexError::InvalidOpener);
        }
        let levels = levels_bound(self.last.len());
        let work = CodeIndexWork {
            key_comparisons_bound: NODE_KEYS * levels,
            tree_levels_bound: levels,
            transitions: 4,
            live_metadata_bytes_bound: metadata_bound(self.last.len())?,
            ..CodeIndexWork::default()
        };
        if budget > MAX_INLINE_STEP_WORK || work.total() > budget {
            return Err(CodeIndexError::InvalidBudget);
        }
        let Some(length) = code_search_length(opener.raw_count, opener.escaped) else {
            return Ok(CodeIndexLookup {
                work,
                decision: IndexedCodeDecision::Literal,
            });
        };
        #[cfg(test)]
        self.comparisons.set(0);
        #[cfg(test)]
        let key = RunKey {
            length,
            comparisons: self.comparisons.clone(),
        };
        #[cfg(not(test))]
        let key = length;
        let last = self.last.get(&key).copied();
        let work = CodeIndexWork {
            #[cfg(test)]
            actual_key_comparisons: self.comparisons.get(),
            ..work
        };
        let decision = if last.is_some_and(|start| start >= opener.end) {
            // index只证明未来raw closer存在，实际probe仍沿源寻找第一个，绝不直接选最后一个。
            let probe = CodeSpanProbe::from_opener(opener, self.source_end, self.range.end)
                .map_err(CodeIndexError::Scanner)?
                .ok_or(CodeIndexError::InvalidOpener)?;
            IndexedCodeDecision::Search(probe)
        } else {
            IndexedCodeDecision::Unmatched {
                replay_from: opener.end,
            }
        };
        Ok(CodeIndexLookup { work, decision })
    }
}

#[cfg(test)]
mod tests;
