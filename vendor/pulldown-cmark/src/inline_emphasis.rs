// Copyright 2015 Google Inc. All rights reserved.
// 原强调规则的标量内核；完整 MIT 许可见 LICENSE。

//! 原 Parser 与有界来源消费者共用的强调规则；这些标量运算自身不授予来源或样式权限。

use crate::{scanners::is_punctuation, Options};
#[cfg(test)]
mod tests;

/// 原 firstpass 的左侧规则；table 边界仍由调用方按实际物理源给出。
pub(crate) fn can_open(
    previous: Option<char>,
    next: Option<char>,
    delimiter: u8,
    count: usize,
    in_table: bool,
    after_table_pipe: bool,
    options: Options,
) -> bool {
    let Some(next) = next.filter(|value| !value.is_whitespace()) else {
        return false;
    };
    let Some(previous) = previous else {
        return true;
    };
    if in_table {
        if after_table_pipe {
            return true;
        }
        if next == '|' {
            return false;
        }
    }
    if delimiter == b'~' && count == 1 && options.contains(Options::ENABLE_SUBSCRIPT) {
        return true;
    }
    if delimiter == b'*' && !is_punctuation(next) || delimiter == b'~' && count > 1 {
        return true;
    }
    if delimiter == b'~' && previous == '~' && !is_punctuation(next) {
        return true;
    }
    previous.is_whitespace()
        || is_punctuation(previous) && (delimiter != b'\'' || ![']', ')'].contains(&previous))
}

/// 原 firstpass 的右侧规则；缺失后继字符是真块末端，而不是输入 frame 或 viewport 末端。
pub(crate) fn can_close(
    previous: Option<char>,
    next: Option<char>,
    delimiter: u8,
    count: usize,
    in_table: bool,
    after_table_pipe: bool,
    options: Options,
) -> bool {
    let Some(previous) = previous.filter(|value| !value.is_whitespace()) else {
        return false;
    };
    let Some(next) = next else {
        return true;
    };
    if in_table {
        if after_table_pipe {
            return false;
        }
        if next == '|' {
            return true;
        }
    }
    if delimiter == b'~' && count == 1 && options.contains(Options::ENABLE_SUBSCRIPT) {
        return true;
    }
    if (delimiter == b'*' || delimiter == b'~' && count > 1) && !is_punctuation(previous) {
        return true;
    }
    if delimiter == b'~' && previous == '~' {
        return true;
    }
    next.is_whitespace() || is_punctuation(next)
}

#[derive(Clone, Debug, Default)]
/// 原 InlineStack 的九个失败下界；存储介质与节点来源不属于这个固定标量内核。
pub struct EmphasisBounds {
    values: [usize; 9],
}
impl EmphasisBounds {
    /// 嵌套 label 切换时保存真实的九个失败下界；外存拥有者负责其 I/O 与空间费用。
    /// 不能清零后重新搜索，否则会改变同一解析事务的累计工作量和失败剪枝语义。
    pub fn checkpoint(&self) -> [usize; 9] {
        self.values
    }

    /// 只恢复同一解析拥有者保存的固定标量，不从文件位置或当前深度推测旧边界。
    pub fn restore(values: [usize; 9]) -> Self {
        Self { values }
    }

    const UNDERSCORE_NOT_BOTH: usize = 0;
    const ASTERISK_NOT_BOTH: usize = 1;
    const ASTERISK_BASE: usize = 2;
    const TILDES: usize = 5;
    const UNDERSCORE_BASE: usize = 6;

    /// 与原栈相同地跳过已证不匹配的前缀，不以文件 record 编号代替真实栈深度。
    pub fn lower_bound(&self, delimiter: u8, count: usize, both: bool) -> usize {
        let (modulo, ordinary) = match delimiter {
            b'_' => (Self::UNDERSCORE_BASE + count % 3, Self::UNDERSCORE_NOT_BOTH),
            b'*' => (Self::ASTERISK_BASE + count % 3, Self::ASTERISK_NOT_BOTH),
            _ => return self.values[Self::TILDES],
        };
        if both {
            self.values[modulo]
        } else {
            self.values[modulo].min(self.values[ordinary])
        }
    }

    /// 只有原逆向搜索真实失败才推进对应下界；不同 delimiter 类别的状态不能互相覆盖。
    pub fn failed(&mut self, delimiter: u8, count: usize, both: bool, depth: usize) {
        match delimiter {
            b'_' => {
                if both {
                    self.values[Self::UNDERSCORE_BASE + count % 3] = depth;
                } else {
                    self.values[Self::UNDERSCORE_NOT_BOTH] = depth;
                }
            }
            b'*' => {
                self.values[Self::ASTERISK_BASE + count % 3] = depth;
                if !both {
                    self.values[Self::ASTERISK_NOT_BOTH] = depth;
                }
            }
            _ => self.values[Self::TILDES] = depth,
        }
    }

    /// 匹配截栈后按真实剩余深度收缩全部下界，保持原后继匹配顺序。
    pub fn truncate(&mut self, depth: usize) {
        for lower in &mut self.values {
            *lower = (*lower).min(depth);
        }
    }

    /// 原单/双波浪号入栈前会收缩共享下界，避免之前一种长度的失败屏蔽另一种长度。
    pub fn before_push(&mut self, delimiter: u8, depth: usize) {
        if delimiter == b'~' {
            self.values[Self::TILDES] = self.values[Self::TILDES].min(depth);
        }
    }

    /// 原栈出清结束当前块；不能让前块失败下界流入另一个真实块。
    pub fn clear(&mut self) {
        self.values = [0; 9];
    }
}

/// 原 find_match 的确切 mod3/波浪号长度条件；它只计算候选能否相配，不构造来源证明。
pub fn matches(
    delimiter: u8,
    run_length: usize,
    both: bool,
    opener_delimiter: u8,
    opener_run_length: usize,
    opener_both: bool,
) -> bool {
    if delimiter == b'~' && run_length != opener_run_length {
        return false;
    }
    opener_delimiter == delimiter
        && (!both && !opener_both
            || (run_length + opener_run_length) % 3 != 0
            || run_length % 3 == 0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 原消费结果的语义类别；实际源 owner 与 marker 显隐仍由消费者独立绑定。
pub enum EmphasisStyle {
    Literal,
    Emphasis,
    Strong,
    Strikethrough,
    Subscript,
    Superscript,
}

/// 每次从内向外消费最多两个 delimiter，与原 Tree 匹配循环共用这一规则。
pub fn consume_count(remaining: usize) -> usize {
    remaining.min(2)
}

/// 原 Parser 的扩展开关决定真实标签；关闭选项不能由来源消费者重新解释成其他样式。
pub fn style(delimiter: u8, count: usize, options: Options) -> EmphasisStyle {
    match delimiter {
        b'~' if count == 1 && options.contains(Options::ENABLE_SUBSCRIPT) => {
            EmphasisStyle::Subscript
        }
        b'~' if options.contains(Options::ENABLE_STRIKETHROUGH) => EmphasisStyle::Strikethrough,
        b'^' if options.contains(Options::ENABLE_SUPERSCRIPT) => EmphasisStyle::Superscript,
        b'~' | b'^' => EmphasisStyle::Literal,
        _ if count == 2 => EmphasisStyle::Strong,
        _ => EmphasisStyle::Emphasis,
    }
}
