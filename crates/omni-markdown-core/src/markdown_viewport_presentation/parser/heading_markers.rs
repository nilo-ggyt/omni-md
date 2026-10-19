//! 原ATX closing显示规则的同一反向状态；Parser与有限来源分别提供自己的真实bytes。

use std::ops::Range;

#[derive(Clone, Copy)]
enum Phase {
    Space,
    Hash,
    Done,
}

/// 只保原始首行边界与固定标量，不识别ATX块、不配对inline，也不保留正文。
pub(crate) struct HeadingMarkerTail {
    start: usize,
    next: usize,
    end: usize,
    phase: Phase,
    marker: Option<Range<usize>>,
}

impl HeadingMarkerTail {
    /// 调用方给出真实已确认ATX的首行范围；end不含首个raw-LF，CR照旧保留。
    pub(crate) fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            next: end,
            end,
            phase: Phase::Space,
            marker: None,
        }
    }

    /// 原单slice和有限反向帧都按同一个连续逆序输入，不能跨过未观察的源字节。
    pub(crate) fn observe_reverse(&mut self, byte: u8) {
        self.next -= 1;
        match self.phase {
            Phase::Space if matches!(byte, b' ' | b'\t') => {
                // 对应旧trim_end_matches([' ', '\t'])；CR和其他空白不在修剪集合中。
            }
            Phase::Space if byte == b'#' => {
                self.phase = Phase::Hash; // 至少真实见到一个末尾#后才判定前置分隔。
            }
            Phase::Hash if byte == b'#' => {
                // 对应旧trim_end_matches('#')，跨frame仍保留同一个尾串起点。
            }
            Phase::Hash => {
                if matches!(byte, b' ' | b'\t') {
                    self.marker = Some(self.next + 1..self.end);
                } // 与旧ends_with([' ', '\t'])一致，隐藏范围包含尾部空白。
                self.phase = Phase::Done;
            }
            _ => self.phase = Phase::Done, // trim后不是#就没有closing，不继续向正文搜索另一组候选。
        }
    }

    pub(crate) fn next_end(&self) -> usize {
        self.next
    }

    pub(crate) fn done(&self) -> bool {
        self.next == self.start || matches!(self.phase, Phase::Done)
    }

    pub(crate) fn marker(&self) -> Option<Range<usize>> {
        self.marker.clone()
    }
}
