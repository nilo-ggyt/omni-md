// Copyright 2015 Google Inc. All rights reserved.
// 从固定0.13.4实体与代码分隔符规则提取；上游MIT许可保留在LICENSE。

//! 原整段Parser与分段候选共享的有限标量内核；这里不构造文本、AST或最终行内权限。

use crate::{entities, CowStr};

#[derive(Clone, Debug, Default)]
/// 原 LinkStack 的禁用规则：成功 Link 使当时所有尚存的 Link opener 失效，Image 不受影响。
/// serial 是真实 opener 身份，不是工作计数；因此外存栈无需重写所有祖先记录。
/// 后来打开的 opener 拥有更大的 serial，不会被上一次成功链接误禁用。
pub(crate) struct LinkStackDecision {
    opened: usize,
    disabled_through: usize,
}

impl LinkStackDecision {
    pub(crate) fn open(&mut self) -> Option<usize> {
        self.opened = self.opened.checked_add(1)?;
        Some(self.opened)
    }

    pub(crate) fn enabled(&self, serial: usize, image: bool) -> bool {
        image || serial > self.disabled_through
    }

    pub(crate) fn accept_link(&mut self) {
        self.disabled_through = self.opened;
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityValue {
    Named(&'static str),
    Scalar(char),
}

impl EntityValue {
    pub(crate) fn into_cow(self) -> CowStr<'static> {
        match self {
            // 命名值始终借用原静态表；数字值沿用原CowStr字符编码，不持输入正文。
            Self::Named(value) => value.into(),
            Self::Scalar(value) => value.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EntityTransition {
    Continue,
    Matched(EntityValue),
    Rejected,
}

#[derive(Clone, Copy, Debug)]
enum EntityPhase {
    First,
    NumericStart,
    Digits { radix: usize, maximum: usize },
    Named,
    Finished,
}

#[derive(Clone, Copy)]
/// 数字计数与真实实体表最大key共同限制状态；巨长未闭合alnum不进入String。
pub(crate) struct EntityScanner {
    phase: EntityPhase,
    name: [u8; entities::MAX_ENTITY_NAME_BYTES],
    length: usize,
    number: usize,
}

impl std::fmt::Debug for EntityScanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不把实体原名写入诊断；固定scratch只报告其有效长度。
        f.debug_struct("EntityScanner")
            .field("phase", &self.phase)
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

impl EntityScanner {
    /// 调用者已消费原始`&`；与旧scan_entity一样，不在这里重解释首字节。
    pub(crate) fn new() -> Self {
        Self {
            phase: EntityPhase::First,
            name: [0; entities::MAX_ENTITY_NAME_BYTES],
            length: 0,
            number: 0,
        }
    }

    /// 静态表lookup只在命名实体分号处发生；外层先预留真实表推导出的最坏比较字节费。
    #[cfg(feature = "omni-block-frontier")]
    pub(crate) fn lookup_work_bound(&self, byte: u8) -> usize {
        if byte == b';' && matches!(self.phase, EntityPhase::First | EntityPhase::Named) {
            entities::ENTITY_LOOKUP_BYTE_WORK_BOUND
        } else {
            0
        }
    }

    /// 每次只消费一个候选字节。Rejected的当前字节不属于实体，外层须按原词法重放。
    pub(crate) fn push(&mut self, byte: u8) -> EntityTransition {
        match self.phase {
            EntityPhase::First if byte == b'#' => {
                // 原数字实体在`#`后才决定十进制或十六进制，frame末不能提前拒绝。
                self.phase = EntityPhase::NumericStart;
                EntityTransition::Continue
            }
            EntityPhase::First | EntityPhase::Named => {
                if byte.is_ascii_alphanumeric() && self.length < self.name.len() {
                    // 只有静态表中可能存在的有限名字需要保留；再长不可能匹配原表。
                    self.name[self.length] = byte;
                    self.length += 1;
                    self.phase = EntityPhase::Named;
                    EntityTransition::Continue
                } else if byte == b';' {
                    self.phase = EntityPhase::Finished;
                    entities::get_entity(&self.name[..self.length])
                        .map_or(EntityTransition::Rejected, |value| {
                            EntityTransition::Matched(EntityValue::Named(value))
                        })
                } else {
                    // 非alnum或超过真实最大key均不可能是实体；不会吞掉后面的反引号等token。
                    self.phase = EntityPhase::Finished;
                    EntityTransition::Rejected
                }
            }
            EntityPhase::NumericStart => {
                if byte.eq_ignore_ascii_case(&b'x') {
                    self.phase = EntityPhase::Digits {
                        radix: 16,
                        maximum: 6,
                    };
                    EntityTransition::Continue
                } else {
                    // 原十进制分支的当前字节也是首个digit；只调用同一单字节转换一次。
                    self.phase = EntityPhase::Digits {
                        radix: 10,
                        maximum: 7,
                    };
                    self.digit(byte, 10, 7)
                }
            }
            EntityPhase::Digits { radix, maximum } => self.digit(byte, radix, maximum),
            EntityPhase::Finished => EntityTransition::Rejected,
        }
    }

    fn digit(&mut self, byte: u8, radix: usize, maximum: usize) -> EntityTransition {
        let digit = match byte {
            b'0'..=b'9' => Some(usize::from(byte - b'0')),
            b'a'..=b'f' if radix == 16 => Some(usize::from(byte - b'a' + 10)),
            b'A'..=b'F' if radix == 16 => Some(usize::from(byte - b'A' + 10)),
            _ => None,
        };
        if let Some(digit) = digit.filter(|_| self.length < maximum) {
            if let Some(number) = self
                .number
                .checked_mul(radix)
                .and_then(|n| n.checked_add(digit))
            {
                // 原实现到达digit上限后要求下一个字节恰为`;`，不能接受长数字的短前缀。
                self.number = number;
                self.length += 1;
                return EntityTransition::Continue;
            }
        }
        self.phase = EntityPhase::Finished;
        if byte == b';' && self.length != 0 {
            // 原规则仅替换0、无效scalar与越界值；不额外采用浏览器的Windows-1252替换表。
            let scalar = u32::try_from(self.number)
                .ok()
                .filter(|&value| value != 0)
                .and_then(char::from_u32)
                .unwrap_or('\u{fffd}');
            EntityTransition::Matched(EntityValue::Scalar(scalar))
        } else {
            EntityTransition::Rejected
        }
    }
}

/// 原MaybeCode opener只在preceding-backslash时少一个tick；closer仍比较原始run长度。
pub(crate) fn code_search_length(raw_count: usize, preceded_by_backslash: bool) -> Option<usize> {
    raw_count
        .checked_sub(usize::from(preceded_by_backslash))
        .filter(|&count| count != 0)
}

pub(crate) fn code_matches(search_count: usize, raw_closer_count: usize) -> bool {
    search_count == raw_closer_count
}
