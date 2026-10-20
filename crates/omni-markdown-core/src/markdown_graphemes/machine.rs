//! 固定状态只记录规则所需的有限后缀，不把piece、配额或窗口边界当作段首/段尾。
use super::{
    MarkdownGraphemeBoundary as Boundary, MarkdownGraphemeError as Error,
    MarkdownGraphemeStatus as Status, MarkdownGraphemeStep as Step, Result, store::BoundaryIndex,
    unicode_data,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Properties {
    gcb: u8,
    incb: u8,
    ep: bool,
}
fn property(table: &[(u32, u32, u8)], scalar: u32) -> u8 {
    let index = table.partition_point(|row| row.0 <= scalar);
    if index == 0 || scalar > table[index - 1].1 {
        0
    } else {
        table[index - 1].2
    }
}
fn properties(scalar: char) -> Properties {
    let scalar = scalar as u32;
    Properties {
        gcb: property(unicode_data::GCB, scalar),
        incb: property(unicode_data::INCB, scalar),
        ep: property(unicode_data::EP, scalar) != 0,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct State {
    previous: Option<u8>,
    ri_odd: bool,
    ep_extend_suffix: bool,
    zwj_after_ep: bool,
    linker_extend_suffix: bool,
}
impl State {
    fn break_before(self, current: Properties) -> bool {
        let Some(previous) = self.previous else {
            return true;
        };
        // GB3优先于控制字符；CR/LF跨chunk仍是同一对，不会因为新页而拆开。
        if previous == 1 && current.gcb == 2 {
            return false;
        }
        // GB4/GB5在所有扩展、Indic与emoji规则前应用。
        if matches!(previous, 1..=3) || matches!(current.gcb, 1..=3) {
            return true;
        }
        // GB6..8保持Hangul组合次序，LV/LVT直接使用官方GCB数据。
        if previous == 9 && matches!(current.gcb, 9 | 10 | 12 | 13) {
            return false;
        }
        if matches!(previous, 10 | 12) && matches!(current.gcb, 10 | 11) {
            return false;
        }
        if matches!(previous, 11 | 13) && current.gcb == 11 {
            return false;
        }
        // GB9/9a/9b只决定边界，随后状态更新仍处理每个真实标量。
        if matches!(current.gcb, 4 | 5 | 8) || previous == 7 {
            return false;
        }
        // Unicode18 GB9c是Linker Extend* × Consonant，不再要求Linker左边存在Consonant。
        if current.incb == 1 && self.linker_extend_suffix {
            return false;
        }
        // GB11只认GCB Extend和实际EP属性，不能把其他InCB Extend混为emoji上下文。
        if previous == 5 && current.ep && self.zwj_after_ep {
            return false;
        }
        // GB12/13只需RI连续段奇偶，不持有任意长RI列表或按chunk重置计数。
        if previous == 6 && current.gcb == 6 && self.ri_odd {
            return false;
        }
        true
    }
    fn advance(&mut self, current: Properties) {
        self.ri_odd = current.gcb == 6 && (self.previous != Some(6) || !self.ri_odd);
        self.zwj_after_ep = current.gcb == 5 && self.ep_extend_suffix;
        self.ep_extend_suffix = current.ep || (current.gcb == 4 && self.ep_extend_suffix);
        self.linker_extend_suffix =
            current.incb == 3 || (current.incb == 2 && self.linker_extend_suffix);
        self.previous = Some(current.gcb);
    }
}

/// 脚本阶段复用原字素规则与数据；只携带固定后缀，不创建第二份边界索引或正文源。
#[cfg(feature = "paged-scripts")]
#[derive(Default)]
pub(crate) struct ScalarCursor(State);
#[cfg(feature = "paged-scripts")]
impl ScalarCursor {
    pub(crate) fn push(&mut self, scalar: char) -> bool {
        let current = properties(scalar);
        let boundary = self.0.break_before(current);
        self.0.advance(current);
        boundary // 必须先用旧后缀判断，再推进同一个State；chunk或配额不会重置它。
    }
}

#[derive(Default)]
pub(super) struct Machine {
    state: State,
    pub(super) byte: usize,
    pub(super) utf16: usize,
    pending: [u8; 4],
    pub(super) used: usize,
    needed: usize,
    scalar_start: usize,
    pub(super) frontier: Option<Boundary>,
    last_boundary: Option<Boundary>,
    pub(super) ready: bool,
}

impl Machine {
    /// 原 row 票证明 BOF/完整换行之后的 GB1/GB4 首边界；未读内部不产生任何字素记录。
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn begin_table(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
        index: &mut BoundaryIndex,
        step: &mut Step,
        records: usize,
    ) -> Result<bool> {
        if self
            .last_boundary
            .is_none_or(|last| last.byte < span.start.byte)
        {
            if step.records == records {
                return Ok(false);
            }
            index.push(span.start)?;
            self.last_boundary = Some(span.start);
            step.records += 1;
        }
        // 已读短前缀只保其真实历史记录；剩余正文没有要恢复的 GCB 上下文，末 CR/LF 将实际重入 DFA。
        self.byte = span.tail.byte;
        self.utf16 = span.tail.utf16;
        self.used = 0;
        self.needed = 0;
        self.pending = [0; 4];
        self.state = State::default();
        Ok(true)
    }

    /// 真末 CR/LF 已完整消费后才提交 GB4 尾端，未终止的真实 Source EOF 则由 GB2 认证。
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn finish_table(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
        index: &mut BoundaryIndex,
        step: &mut Step,
    ) -> Result<()> {
        if self.byte != span.end.byte
            || self.utf16 != span.end.utf16
            || self.used != 0
            || !span.eof && !matches!(self.state.previous, Some(1 | 2))
        {
            return Err(Error::CorruptMetadata);
        }
        if span.eof {
            return self.finish(index, step);
        }
        if self.last_boundary != Some(span.end) {
            index.push(span.end)?;
            self.last_boundary = Some(span.end);
            step.records += 1;
        }
        self.frontier = Some(span.end);
        Ok(())
    }
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn last_committed_boundary(&self) -> Option<Boundary> {
        self.last_boundary
    }

    pub(super) fn push(
        &mut self,
        byte: u8,
        index: &mut BoundaryIndex,
        step: &mut Step,
    ) -> Result<()> {
        self.push_scalar_byte(byte, index, step, false)
    }

    /// 末尾真实 CR/LF 只恢复规则后缀；隐藏内部不能写边界或扩大已分类前沿。
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn push_fold_tail(
        &mut self,
        byte: u8,
        index: &mut BoundaryIndex,
        step: &mut Step,
    ) -> Result<()> {
        self.push_scalar_byte(byte, index, step, true)
    }

    /// 原 UTF8 解码和规则状态始终只有一份；opaque_tail 仅允许已证明隐区末端的真实控制字节。
    fn push_scalar_byte(
        &mut self,
        byte: u8,
        index: &mut BoundaryIndex,
        step: &mut Step,
        opaque_tail: bool,
    ) -> Result<()> {
        let position = self.byte;
        // 此byte已经从真实piece解引用；即使下面发现UTF8错误，也要计入实际消耗后永久失败。
        self.byte = self.byte.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        step.bytes = step.bytes.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        if self.used == 0 {
            self.scalar_start = position;
            // UTF8前导长度仅保留单标量残余；后继完整from_utf8再核过长编码、代理区和上界。
            self.needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(Error::InvalidUtf8),
            };
        }
        self.pending[self.used] = byte;
        self.used += 1;
        if self.used != self.needed {
            // 真实借用已结束也不丢弃残余；下次step从同一全局byte继续，不重新发sot。
            return Ok(());
        }
        let scalar = std::str::from_utf8(&self.pending[..self.used])
            .ok()
            .and_then(|value| value.chars().next())
            .ok_or(Error::InvalidUtf8)?;
        if opaque_tail && !matches!(scalar, '\r' | '\n') {
            return Err(Error::CorruptMetadata);
        } // 只有实际硬控制能切断任意长 RI/emoji/Indic 后缀，不能拿任意隐藏标量重置状态。
        let boundary = Boundary {
            byte: self.scalar_start,
            utf16: self.utf16,
        };
        let value = properties(scalar);
        if !opaque_tail && self.state.break_before(value) && self.last_boundary != Some(boundary) {
            // 只有实际规则确认的边界才写索引；IO失败不会继续更新或假装发布这一记录。
            index.push(boundary)?;
            self.last_boundary = Some(boundary);
            step.records = step
                .records
                .checked_add(1)
                .ok_or(Error::CoordinateOverflow)?;
        }
        self.state.advance(value);
        if !opaque_tail {
            self.frontier = Some(boundary);
        }
        self.utf16 = self
            .utf16
            .checked_add(scalar.len_utf16())
            .ok_or(Error::CoordinateOverflow)?;
        self.used = 0;
        step.scalars = step
            .scalars
            .checked_add(1)
            .ok_or(Error::CoordinateOverflow)?;
        Ok(())
    }

    /// 已完整读取首分隔符后才允许跨过隐区；坐标跳跃不产生任何读取、标量或边界计数。
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn jump_fold(
        &mut self,
        from: Boundary,
        to: Boundary,
        visible_start: Boundary,
    ) -> Result<()> {
        if self.used != 0
            || self.byte != from.byte
            || self.utf16 != from.utf16
            || to.byte < from.byte
            || to.utf16 < from.utf16
            || self.last_boundary != Some(visible_start)
            || !matches!(self.state.previous, Some(1 | 2))
        {
            return Err(Error::CorruptMetadata);
        }
        self.byte = to.byte;
        self.utf16 = to.utf16;
        self.frontier = Some(visible_start);
        // 保留真实首控制后缀，随后只消费真实末控制；两者均由 GB4/5 切断隐藏文本上下文。
        // 无终止符的真正 EOF 不再查询后继，只能由原 finish 发布 GB2。
        Ok(())
    }

    pub(super) fn finish(&mut self, index: &mut BoundaryIndex, step: &mut Step) -> Result<()> {
        let end = Boundary {
            byte: self.byte,
            utf16: self.utf16,
        };
        if self.last_boundary != Some(end) {
            // 空源也只有一个0..0边界；末尾记录必须仍消费原step记录预算。
            index.push(end)?;
            self.last_boundary = Some(end);
            step.records = step
                .records
                .checked_add(1)
                .ok_or(Error::CoordinateOverflow)?;
        }
        // 先真实封存匿名索引，再授予完整EOF权限；同步失败时调用方永久撤权。
        index.seal()?;
        self.frontier = Some(end);
        self.ready = true;
        step.status = Status::Ready;
        Ok(())
    }
}
