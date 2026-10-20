// Copyright 2015 Google Inc. All rights reserved.
// 原数学状态转移提取自固定pulldown-cmark0.13.4 firstpass.rs；MIT条款见LICENSE。

//! 原lexer和分段窗口共享数学brace状态；保留上游u8标签回绕及25层以上计数语义。

// Each level of brace nesting adds another entry to a hash table.
// To limit the amount of memory consumed, do not create a new brace
// context beyond some amount deep.
//
// There are actually two limits at play here: this one,
// and the one where the maximum amount of distinct contexts passes
// the 255 item limit imposed by using `u8`. When over 255 distinct
// contexts are created, it wraps around, while this one instead makes it
// saturate, which is a better behavior.
const MAX_NESTING: usize = 25;

#[derive(Clone, Copy, Debug)]
/// 真实block起点重置的有限状态；26个槽足够覆盖原Vec最大长度，不持正文或分配。
pub struct MathContext {
    stack: [u8; MAX_NESTING + 1],
    len: usize,
    next: usize,
}

impl PartialEq for MathContext {
    fn eq(&self, other: &Self) -> bool {
        // reset/pop后的未使用槽不是语义状态，不让旧block的无效栈尾制造虚假差异。
        self.len == other.len
            && self.next == other.next
            && self.stack[..self.len] == other.stack[..other.len]
    }
}
impl Eq for MathContext {}

/// 同一原 lexer 的逐 byte 数学转移；行窗口和单格入口捕获共用，不能各自猜 brace/escape 语义。
pub(crate) fn observe_math_byte(context: &mut MathContext, escaped: &mut bool, byte: u8) {
    if *escaped {
        *escaped = false;
        if crate::scanners::is_ascii_punctuation(byte) {
            return; // 原反斜杠吞掉 punctuation；被额外跳过的 pipe/backtick 不含数学 token。
        }
    }
    match byte {
        b'\\' => *escaped = true,
        b'$' => {
            context.dollar();
        }
        b'{' => context.open(),
        b'}' => context.close(),
        _ => {} // 其他字节也由所属调用方计费，但不会改变数学上下文。
    }
}

impl Default for MathContext {
    fn default() -> Self {
        Self {
            stack: [0; MAX_NESTING + 1],
            len: 0,
            next: 0,
        }
    }
}

impl MathContext {
    /// 原parse_block唯一reset边界；不能在窗口/行边界伪造新的math block。
    pub(crate) fn reset(&mut self) {
        self.len = 0;
        self.next = 0;
    }

    /// 对应原`$`分支；首次math token才建立root，保留原u8标签截断。
    pub(crate) fn dollar(&mut self) -> u8 {
        if self.len > MAX_NESTING {
            // 超过真实栈深后仅使用计数标签，不能偷偷增加独立上下文容量。
            self.next as u8
        } else if self.len > 0 {
            self.stack[self.len - 1]
        } else {
            // 未见math前的普通brace不会建立状态；首次dollar使用原root值255。
            self.stack[0] = !0;
            self.len = 1;
            !0
        }
    }

    /// 对应原`{`分支；饱和后继续原depth计数，未启用math前保持无状态。
    pub(crate) fn open(&mut self) {
        if self.len == MAX_NESTING {
            self.stack[self.len] = self.next as u8;
            self.len += 1;
            self.next = MAX_NESTING;
        } else if self.len > MAX_NESTING {
            // 与上游一样累积深度；source/work上限由外层真实请求限制。
            self.next += 1;
        } else if self.len > 0 {
            self.stack[self.len] = self.next as u8;
            self.len += 1;
            self.next += 1;
        }
    }

    /// 对应原`}`分支；不平衡close改变root标签，不能按普通brace净计数折叠。
    pub(crate) fn close(&mut self) {
        if self.len == 1 {
            self.stack[0] = self.stack[0].wrapping_sub(1);
        } else if self.len > MAX_NESTING {
            // 降回真实栈范围才pop；饱和区间不能丢掉历史depth。
            if self.next <= MAX_NESTING {
                self.len -= 1;
            } else {
                self.next -= 1;
            }
        } else if self.len > 0 {
            self.len -= 1;
        }
    }
}

#[cfg(feature = "omni-block-frontier")]
mod stream;
#[cfg(feature = "omni-block-frontier")]
pub use stream::*;

#[cfg(test)]
mod tests;
