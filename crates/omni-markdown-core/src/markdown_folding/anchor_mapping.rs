//! 非inclusive折叠端点的纯映射事务；输入是真实旧坐标ChangeSet，不是首尾变化envelope。
//!
//! 最多保存256个fold和512个排序端点，不保存编辑、匹配、正文或piece root。调用方须绑定
//! 自己的文档/代次票据，实际文本事务成功后才提交本候选；取消或上游失败直接丢弃即可。

use std::ops::Range;

use super::MarkdownFoldRegion;

/// 当前前台状态允许的折叠锚点上限；超量整批拒绝，不能静默丢掉后部fold。
pub const MAX_MARKDOWN_FOLD_ANCHORS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一个真实变化段；全部range都属于同一旧文档，插入量使用UTF-16 code unit。
///
/// 保留相邻变化的独立边界，以及同位置插入的原有顺序。调用方不得把离散变化合成envelope，
/// 也不得依次把range改成前一项已应用后的坐标。零宽变化仍参与delete事件的严格相交判断。
pub struct MarkdownFoldChange {
    pub old_utf16_range: Range<usize>,
    pub inserted_utf16_len: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// 首错使整个候选失效；调用方原有fold集合从未被此mapper修改。
pub enum MarkdownFoldMappingError {
    #[error("折叠锚点数量超过当前上限")]
    TooManyAnchors,
    #[error("旧折叠范围无效或越过旧文档")]
    InvalidAnchor,
    #[error("变化范围无效或越过旧文档")]
    InvalidChange,
    #[error("变化没有按旧坐标升序排列或相互重叠")]
    ChangesOutOfOrder,
    #[error("折叠坐标映射发生整数溢出")]
    CoordinateOverflow,
    #[error("变化流计算出的新文档长度不一致")]
    NewLengthMismatch,
    #[error("显式主选区head越过新文档")]
    MainHeadOutsideDocument,
    #[error("折叠端点映射未完整完成")]
    IncompleteMapping,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum Side {
    // 同位置先处理end(-1)，才能把零宽插入排除在fold末端之外。
    End,
    Start,
}

#[derive(Clone, Copy)]
struct Endpoint {
    old: usize,
    anchor: usize,
    side: Side,
}

struct Anchor {
    old_start: usize,
    start: Option<usize>,
    end: Option<usize>,
    delete_touched: bool,
}

/// 不持源身份的候选；权限由拥有实际文本事务的调用方校验，不能凭本对象伪造根/代次。
pub struct MarkdownFoldAnchorMapping {
    old_length: usize,
    old_cursor: usize,
    new_cursor: usize,
    delete_user_event: bool,
    last_changed_old_end: Option<usize>,
    anchors: Vec<Anchor>,
    endpoints: Vec<Endpoint>,
    next_endpoint: usize,
    failure: Option<MarkdownFoldMappingError>,
}

impl MarkdownFoldAnchorMapping {
    /// 复制有限旧范围并排序端点；旧header_line不参与坐标映射，也不被误报为新行号。
    ///
    /// # Errors
    /// 超过256项、空/倒序fold或越过旧长度时拒绝整批，不截断或修改输入slice。
    pub fn new(
        old_utf16_len: usize,
        folds: &[MarkdownFoldRegion],
        delete_user_event: bool,
    ) -> Result<Self, MarkdownFoldMappingError> {
        if folds.len() > MAX_MARKDOWN_FOLD_ANCHORS {
            return Err(MarkdownFoldMappingError::TooManyAnchors);
        }
        let mut anchors = Vec::with_capacity(folds.len());
        let mut endpoints = Vec::with_capacity(folds.len() * 2);
        for fold in folds {
            let range = &fold.hidden_utf16_range;
            if range.start >= range.end || range.end > old_utf16_len {
                return Err(MarkdownFoldMappingError::InvalidAnchor);
            }
            let anchor = anchors.len();
            anchors.push(Anchor {
                old_start: range.start,
                start: None,
                end: None,
                delete_touched: false,
            });
            endpoints.push(Endpoint {
                old: range.start,
                anchor,
                side: Side::Start,
            });
            endpoints.push(Endpoint {
                old: range.end,
                anchor,
                side: Side::End,
            });
        }
        // 最多512个端点；稳定保留anchors原顺序，避免调用方需再保存索引或旧对象。
        endpoints.sort_unstable_by_key(|endpoint| (endpoint.old, endpoint.side));
        Ok(Self {
            old_length: old_utf16_len,
            old_cursor: 0,
            new_cursor: 0,
            delete_user_event,
            last_changed_old_end: None,
            anchors,
            endpoints,
            next_endpoint: 0,
            failure: None,
        })
    }

    /// 消费一项旧坐标变化；最多推进尚未完成的有限端点，不缓存当前项或此前编辑。
    ///
    /// # Errors
    /// 错序、重叠、越界或溢出记录首错；之后push/finish只返回该首错，不能发布部分候选。
    pub fn push(&mut self, change: MarkdownFoldChange) -> Result<(), MarkdownFoldMappingError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = self.push_inner(change);
        if let Err(error) = result {
            self.failure = Some(error); // 不回滚内部scratch；失败对象永远无提交权限。
        }
        result
    }

    fn push_inner(&mut self, change: MarkdownFoldChange) -> Result<(), MarkdownFoldMappingError> {
        let range = change.old_utf16_range;
        if range.start > range.end || range.end > self.old_length {
            return Err(MarkdownFoldMappingError::InvalidChange);
        }
        if range.start < self.old_cursor {
            return Err(MarkdownFoldMappingError::ChangesOutOfOrder);
        }
        let new_start = self
            .new_cursor
            .checked_add(range.start - self.old_cursor)
            .ok_or(MarkdownFoldMappingError::CoordinateOverflow)?;
        let new_end = new_start
            .checked_add(change.inserted_utf16_len)
            .ok_or(MarkdownFoldMappingError::CoordinateOverflow)?;
        while let Some(endpoint) = self.next() {
            if endpoint.old < range.start {
                // 变化前的未改动间隙线性平移；不会对每项编辑重扫所有fold。
                let mapped = self
                    .new_cursor
                    .checked_add(endpoint.old - self.old_cursor)
                    .ok_or(MarkdownFoldMappingError::CoordinateOverflow)?;
                self.resolve(endpoint, mapped, false);
            } else if endpoint.old == range.start {
                if range.is_empty() && endpoint.side == Side::Start {
                    // start(+1)必须越过本次及同位置后续插入，留给下一真实变化或finish结算。
                    break;
                }
                // replacement的原起点映到新起点；纯插入的end(-1)停在插入之前。
                self.resolve(endpoint, new_start, false);
            } else if endpoint.old < range.end {
                // 删除内部端点按assoc两侧分配：start(+1)在替换后，end(-1)在替换前。
                let mapped = match endpoint.side {
                    Side::Start => new_end,
                    Side::End => new_start,
                };
                self.resolve(endpoint, mapped, true);
            } else {
                // 原终点须继续看相邻变化；不能把两个replacement或插入误并成一个变化段。
                break;
            }
        }
        // 已排序的真实变化end不下降。结算fold末端时，它是此前变化是否严格穿入fold的充分信息；
        // 当前变化起点恰等于fold末端时，末端已在上面先结算，所以边界不会误unfold。
        self.last_changed_old_end = Some(range.end);
        self.old_cursor = range.end;
        self.new_cursor = new_end;
        Ok(())
    }

    fn next(&self) -> Option<Endpoint> {
        self.endpoints.get(self.next_endpoint).copied()
    }

    fn resolve(&mut self, endpoint: Endpoint, mapped: usize, inside_current_change: bool) {
        let anchor = &mut self.anchors[endpoint.anchor];
        match endpoint.side {
            Side::Start => anchor.start = Some(mapped),
            Side::End => {
                anchor.end = Some(mapped);
                anchor.delete_touched = self.delete_user_event
                    && (inside_current_change
                        || self
                            .last_changed_old_end
                            .is_some_and(|end| end > anchor.old_start));
            }
        }
        self.next_endpoint += 1;
    }

    /// 验证全流长度后移交候选新范围；此函数本身不提交到任何文档或前台状态。
    ///
    /// explicit_main_head_utf16只来自显式selection事务的main.head，不能拿旧caret或anchor代替；
    /// 严格内部才展开，边界不展开。调用方在真实新文档按新range.start重建header_line，
    /// 并继续检查原owner/文档代次；模式切换清空仍由原调用方policy决定。
    ///
    /// # Errors
    /// 首错、溢出、新长度不符、head越界或内部端点不完整时整批拒绝，原fold仍由调用方拥有。
    pub fn finish(
        mut self,
        new_utf16_len: usize,
        explicit_main_head_utf16: Option<usize>,
    ) -> Result<Vec<Range<usize>>, MarkdownFoldMappingError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.anchors.len() > MAX_MARKDOWN_FOLD_ANCHORS {
            return Err(MarkdownFoldMappingError::TooManyAnchors);
        }
        let expected = self
            .new_cursor
            .checked_add(self.old_length - self.old_cursor)
            .ok_or(MarkdownFoldMappingError::CoordinateOverflow)?;
        if expected != new_utf16_len {
            return Err(MarkdownFoldMappingError::NewLengthMismatch);
        }
        if explicit_main_head_utf16.is_some_and(|head| head > new_utf16_len) {
            return Err(MarkdownFoldMappingError::MainHeadOutsideDocument);
        }
        while let Some(endpoint) = self.next() {
            // 最后一个变化之后的真实后缀与同位置延迟start在此一次结算。
            let mapped = self
                .new_cursor
                .checked_add(endpoint.old - self.old_cursor)
                .ok_or(MarkdownFoldMappingError::CoordinateOverflow)?;
            self.resolve(endpoint, mapped, false);
        }
        let mut result = Vec::with_capacity(self.anchors.len());
        for anchor in self.anchors {
            let (Some(start), Some(end)) = (anchor.start, anchor.end) else {
                return Err(MarkdownFoldMappingError::IncompleteMapping);
            };
            if anchor.delete_touched || start >= end {
                continue; // delete事件穿入原fold或映射后折叠已空/反转，均失去隐藏权限。
            }
            if explicit_main_head_utf16.is_some_and(|head| start < head && head < end) {
                continue; // 显式主head在内部时展开；只在边界或仅anchor进入内部不触发。
            }
            result.push(start..end);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
