//! 字面查找的有界流式匹配；NFKD、大小写与精确边界独立于 AppKit。

use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;

use unicode_normalization::UnicodeNormalization;

use crate::{MarkdownDocumentBuffer, MarkdownDocumentBufferError};

/// 字面查询选项；UI 的反转义在调用前完成，不能在两种搜索引擎中重复处理。
#[derive(Clone, Copy, Debug, Default)]
pub struct MarkdownLiteralFindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub overlapping: bool,
}

/// 坐标仍指向原正文；归一化命中字符内部时可选择但不得替换整个字符。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownFindMatch {
    pub range_utf16: Range<usize>,
    pub precise: bool,
}

#[derive(Clone, Copy)]
/// 每个归一化 code unit 对应的原始位置，环形窗口只保留查询长度的元数据。
struct Origin {
    start: usize,
    end: usize,
    start_precise: bool,
    end_precise: bool,
}

/// 只有NFKD与所需小写都逐scalar不变时复用原栈编码；其余字符沿原完整转换保精度语义。
fn scalar_units(scalar: char, case_sensitive: bool, original: &[u16]) -> Cow<'_, [u16]> {
    let mut decomposition = scalar.nfkd();
    let unchanged = decomposition.next() == Some(scalar) && decomposition.next().is_none();
    if unchanged
        && (case_sensitive || {
            let mut lower = scalar.to_lowercase();
            lower.next() == Some(scalar) && lower.next().is_none()
        })
    {
        return Cow::Borrowed(original);
    } // 实际迭代确认恒等，不能按CJK区间、主题或一个大小写属性猜测兼容字/多字符转换。
    let normalized = scalar.to_string().nfkd().collect::<String>();
    let normalized = if case_sensitive {
        normalized
    } else {
        normalized.to_lowercase()
    }; // 非恒等仍按原单scalar NFKD字符串整体小写，不能改成逐字符小写而丢上下文规则。
    Cow::Owned(normalized.encode_utf16().collect())
}

/// 访问升序字面匹配；回调返回 false 即结束，不缓存全部匹配或正文副本。
///
/// NFKD 按原编辑器的逐 scalar 规则映射；大小写使用 Unicode 小写而非 ASCII 降级。
/// 额外内存与查询长度成正比，源窗口固定 4,096 UTF-16 units。回调失败原样传播，正文不变。
///
/// # Errors
///
/// 查询范围必须位于正文且两端 scalar aligned；所有源读取错误和回调错误均使本次动作失败。
pub fn visit_markdown_literal_matches(
    document: &MarkdownDocumentBuffer,
    query: &str,
    range: Range<usize>,
    options: MarkdownLiteralFindOptions,
    visit: impl FnMut(MarkdownFindMatch) -> Result<bool, MarkdownDocumentBufferError>,
) -> Result<(), MarkdownDocumentBufferError> {
    visit_markdown_literal_matches_cancellable(document, query, range, options, || true, visit)
}

/// 与普通 visitor 同坐标/精度合同；每最多 4,096 源 units 核对显式取消，不依赖存在匹配。
///
/// # Errors
///
/// `keep_searching` 返回 false 时返回 `FindInterrupted`；不会把部分匹配冒充完整结果。
pub fn visit_markdown_literal_matches_cancellable(
    document: &MarkdownDocumentBuffer,
    query: &str,
    range: Range<usize>,
    options: MarkdownLiteralFindOptions,
    mut keep_searching: impl FnMut() -> bool,
    mut visit: impl FnMut(MarkdownFindMatch) -> Result<bool, MarkdownDocumentBufferError>,
) -> Result<(), MarkdownDocumentBufferError> {
    document.byte_offset_for_utf16(range.start)?;
    document.byte_offset_for_utf16(range.end)?;
    if range.start > range.end {
        // 反向坐标不是一个空搜索区间；拒绝而不偷偷交换用户范围。
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }
    let normalized: String = query.nfkd().collect();
    let normalized = if options.case_sensitive {
        // 区分大小写仍须执行 NFKD，兼容连字与组合字符的查找位置。
        normalized
    } else {
        // 查询整体小写保留原编辑器的上下文规则，例如希腊词末 sigma。
        normalized.to_lowercase()
    };
    let pattern: Vec<_> = normalized.encode_utf16().collect();
    if pattern.is_empty() {
        // 空查询不产生无限零长字面匹配，也不触发源扫描。
        return Ok(());
    }
    let mut prefix = vec![0; pattern.len()];
    let mut matched = 0;
    for index in 1..pattern.len() {
        // KMP 失败表使重复字符的稠密正文仍保持线性比较，不保留每个部分匹配。
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            // 此前缀可被后续不匹配复用，避免重新扫描源字符。
            matched += 1;
        }
        prefix[index] = matched;
    }
    matched = 0;
    let mut origins = VecDeque::with_capacity(pattern.len());
    let mut source = FindSource::new(document);
    let mut position = range.start;
    let mut next_check = range.start;
    while position < range.end {
        if position >= next_check {
            // 无匹配或巨型行也有进度检查；条件只读调用方标量/时钟，不需要 timer 或线程。
            if !keep_searching() {
                return Err(MarkdownDocumentBufferError::FindInterrupted);
            }
            next_check = position.saturating_add(4_096);
        }
        // 每次解码一个完整 scalar；跨缓存边界的 surrogate pair 仍指回原文完整位置。
        let scalar = source.scalar_at(position)?;
        let end = position + scalar.len_utf16();
        let mut original_buffer = [0; 2];
        let original = scalar.encode_utf16(&mut original_buffer);
        let ascii = [if options.case_sensitive {
            scalar as u16
        } else {
            scalar.to_ascii_lowercase() as u16
        }];
        let expanded;
        let units: &[u16] = if scalar.is_ascii() {
            // 1 MiB ASCII 正文不逐字符分配；最常用路径只使用一个栈内 code unit。
            &ascii
        } else {
            // 常见恒等scalar只借原2-unit栈缓冲，兼容展开和变大小写继续使用原有限算法。
            expanded = scalar_units(scalar, options.case_sensitive, original);
            &expanded
        };
        let mut start = position;
        let mut start_precise = true;
        for (index, unit) in units.iter().copied().enumerate() {
            // 一次 expansion 的内部命中保留 precise=false，替换动作不能吞掉未匹配的原内容。
            if origins.len() == pattern.len() {
                origins.pop_front();
            }
            origins.push_back(Origin {
                start,
                end,
                start_precise,
                end_precise: index + 1 == units.len(),
            });
            while matched > 0 && unit != pattern[matched] {
                // 当前归一化单元继续最长后缀，不回退源游标。
                matched = prefix[matched - 1];
            }
            if unit == pattern[matched] {
                matched += 1;
            }
            if matched == pattern.len() {
                let first = origins[origins.len() - pattern.len()];
                let last = *origins.back().expect("a matched query has one origin");
                let candidate = MarkdownFindMatch {
                    range_utf16: first.start..last.end,
                    precise: first.start_precise && last.end_precise,
                };
                let admitted =
                    !options.whole_word || source.word_boundary(candidate.range_utf16.clone())?;
                if admitted && !visit(candidate)? {
                    // callback 只通过显式 false 结束，不再扫描后续正文或构造匹配数组。
                    return Ok(());
                }
                matched = if options.overlapping || !admitted {
                    // next/previous 可以考虑重叠；被 word filter 拒绝的候选也不能跳过有效后缀。
                    prefix[matched - 1]
                } else {
                    // 全部替换的非重叠枚举从已消费的匹配末尾继续。
                    0
                };
                if admitted {
                    // baseline cursor 在首次已接受命中时返回，原 scalar 已整体消费；不再从同一连字尾部产生第二个命中。
                    break;
                }
            }
            if start_precise && original.get(index) == Some(&unit) {
                // 未改变的原 UTF-16 前缀可以精确落在两个 surrogate 之间；最终 scalar 校验仍由事务负责。
                start += 1;
            } else {
                // 第一次宽度/内容扩展后，剩余单元都不再有独立原始起点。
                start_precise = false;
            }
        }
        position = end;
    }
    Ok(())
}

/// 共享字面/正则的整词检查；字母、数字及下划线构成词，不把 ASCII 边界当 Unicode 边界。
pub fn whole_word_boundary(
    document: &MarkdownDocumentBuffer,
    range: Range<usize>,
) -> Result<bool, MarkdownDocumentBufferError> {
    if range.is_empty() {
        // 与 baseline 相同，正则的合法零长匹配不受整词筛选限制。
        return Ok(true);
    }
    let mut source = FindSource::new(document);
    source.word_boundary(range)
}

/// 固定源窗口只复制请求附近的 code units；所有 offsets 仍相对 canonical 文档。
struct FindSource<'a> {
    document: &'a MarkdownDocumentBuffer,
    units: [u16; 4_096],
    start: usize,
    length: usize,
}

impl<'a> FindSource<'a> {
    fn word_boundary(&mut self, range: Range<usize>) -> Result<bool, MarkdownDocumentBufferError> {
        Ok(
            (!self.word_before(range.start)? || !self.word_after(range.start)?)
                && (!self.word_after(range.end)? || !self.word_before(range.end)?),
        )
    }
    fn new(document: &'a MarkdownDocumentBuffer) -> Self {
        Self {
            document,
            units: [0; 4_096],
            start: 0,
            length: 0,
        }
    }

    fn unit(&mut self, at: usize) -> Result<u16, MarkdownDocumentBufferError> {
        if at < self.start || at >= self.start + self.length {
            // 换窗不会越过正文末尾；不会为巨型行分配巨型连续字符串。
            self.start = at;
            self.length = self
                .units
                .len()
                .min(self.document.utf16_len().saturating_sub(at));
            self.document
                .copy_utf16_code_units(at, &mut self.units[..self.length])?;
        }
        self.units
            .get(at - self.start)
            .copied()
            .filter(|_| at < self.start + self.length)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)
    }

    fn scalar_at(&mut self, at: usize) -> Result<char, MarkdownDocumentBufferError> {
        let first = self.unit(at)?;
        if (0xd800..=0xdbff).contains(&first) {
            // canonical 是有效 UTF-8，读取高 surrogate 必须能得到其完整低 surrogate。
            let second = self.unit(at + 1)?;
            char::from_u32(
                0x1_0000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second) - 0xdc00,
            )
            .ok_or(MarkdownDocumentBufferError::NotUnicodeBoundary)
        } else {
            char::from_u32(u32::from(first)).ok_or(MarkdownDocumentBufferError::NotUnicodeBoundary)
        }
    }

    fn previous(&mut self, at: usize) -> Result<(usize, char), MarkdownDocumentBufferError> {
        let last = self.unit(at - 1)?;
        let start = at
            - if (0xdc00..=0xdfff).contains(&last) {
                2
            } else {
                1
            };
        Ok((start, self.scalar_at(start)?))
    }

    fn word_before(&mut self, at: usize) -> Result<bool, MarkdownDocumentBufferError> {
        if at == 0 {
            // 正文边缘的空字符不是词字符，允许第一/最后一个词独立匹配。
            return Ok(false);
        }
        let (mut position, mut scalar) = self.previous(at)?;
        loop {
            // 组合字符不掩盖其基字母；ZWJ 链只累积同一 cluster 内的字符类别。
            if scalar.is_alphabetic() || scalar.is_numeric() || scalar == '_' {
                return Ok(true);
            }
            if position == 0 {
                return Ok(false);
            }
            let (before, prior) = self.previous(position)?;
            if !is_extend(scalar) && scalar != '\u{200d}' && prior != '\u{200d}' {
                return Ok(false);
            }
            position = before;
            scalar = prior;
        }
    }

    fn word_after(&mut self, mut at: usize) -> Result<bool, MarkdownDocumentBufferError> {
        let mut prior_joiner = false;
        while at < self.document.utf16_len() {
            // 查询从 cluster 中部开始时，不回头吞入本次边界前的业务字符。
            let scalar = self.scalar_at(at)?;
            if scalar.is_alphabetic() || scalar.is_numeric() || scalar == '_' {
                return Ok(true);
            }
            at += scalar.len_utf16();
            if at == self.document.utf16_len() {
                return Ok(false);
            }
            let next = self.scalar_at(at)?;
            if !prior_joiner && scalar != '\u{200d}' && next != '\u{200d}' && !is_extend(next) {
                return Ok(false);
            }
            prior_joiner = scalar == '\u{200d}';
        }
        Ok(false)
    }
}

fn is_extend(scalar: char) -> bool {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    // 组合 marks/变体选择符保持同一查询边界；精确 baseline Extend 数据由差分测试继续约束。
    matches!(
        scalar.general_category(),
        GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
    ) || ('\u{1f3fb}'..='\u{1f3ff}').contains(&scalar)
}

#[cfg(test)]
#[path = "markdown_find/scalar_tests.rs"]
mod scalar_tests;
#[cfg(test)]
mod tests;
