//! 动作外层唯一模板 arena；每个命中只记录 capture 坐标，不复制同一 literal。

use std::collections::HashMap;

use super::*;

/// Literal 的范围属于 arena，不是正文；capture token 指向去重后的 wire 槽。
#[derive(Clone, Debug)]
pub(super) enum TemplateToken {
    Literal { bytes: Range<usize>, utf16: usize },
    Capture(usize),
}

/// owner 独立于 reader，故返回给 core 的 Cow 可以安全借到整个同步事务结束。
pub(super) struct ReplacementTemplate {
    pub(super) arena: String,
    pub(super) tokens: Vec<TemplateToken>,
    pub(super) captures: Vec<usize>,
}

impl ReplacementTemplate {
    pub(super) fn parse(text: &str, regex: bool, capture_count: usize) -> Self {
        let mut result = Self {
            arena: String::new(),
            tokens: Vec::new(),
            captures: Vec::new(),
        };
        if !regex {
            // 字面模式的 $ 不展开，整个有限模板只复制一次。
            result.literal(text);
            return result;
        }
        let mut captures = HashMap::new();
        let mut literal = String::new();
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            // 单向解析有界模板；最坏只按数字 token 长度尝试有效 capture 前缀，不扫描正文。
            if bytes[index] == b'$' && index + 1 < bytes.len() {
                if bytes[index + 1] == b'$' {
                    // 双美元在 arena 中规范为一个字面美元，不为每个匹配再次反转义。
                    literal.push('$');
                    index += 2;
                    continue;
                }
                let mut capture = None;
                let mut consumed = 1;
                if bytes[index + 1] == b'&' {
                    capture = Some(0); // 完整匹配也作为去重的第零 capture 坐标记录。
                } else if bytes[index + 1].is_ascii_digit() {
                    let mut end = index + 1;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1; // 数字的全部尾部先界定，再从最长有效前缀处理。
                    }
                    for candidate_end in (index + 2..=end).rev() {
                        if let Ok(candidate) = text[index + 1..candidate_end].parse::<usize>()
                            && candidate > 0
                            && candidate < capture_count
                        {
                            // 多余尾部数字保留给后续 literal；无效 $0/越界编号原样保留。
                            capture = Some(candidate);
                            consumed = candidate_end - index - 1;
                            break;
                        }
                    }
                }
                if let Some(capture) = capture {
                    // 固定 literal 完成后才创建 capture token，arena 的 UTF-8 范围永久稳定。
                    result.literal(&literal);
                    literal.clear();
                    let slot = *captures.entry(capture).or_insert_with(|| {
                        let slot = result.captures.len();
                        result.captures.push(capture);
                        slot
                    });
                    result.tokens.push(TemplateToken::Capture(slot));
                    index += consumed + 1;
                    continue;
                }
            }
            let character = text[index..]
                .chars()
                .next()
                .expect("the template index is scalar aligned");
            literal.push(character);
            index += character.len_utf8();
        }
        result.literal(&literal);
        result
    }

    fn literal(&mut self, text: &str) {
        if text.is_empty() {
            // 空段无需 token；空模板仍产生合法的删除记录。
            return;
        }
        let start = self.arena.len();
        self.arena.push_str(text);
        self.tokens.push(TemplateToken::Literal {
            bytes: start..self.arena.len(),
            utf16: text.encode_utf16().count(),
        });
    }

    pub(super) fn inserted_utf16(
        &self,
        captures: &[Option<Range<usize>>],
    ) -> Result<usize, String> {
        self.tokens.iter().try_fold(0usize, |total, token| {
            let units = match token {
                TemplateToken::Literal { utf16, .. } => *utf16, // 字面宽度在解析时只计算一次。
                TemplateToken::Capture(slot) => captures
                    .get(*slot)
                    .ok_or("替换捕获记录不完整")?
                    .as_ref()
                    .map_or(9, Range::len), // baseline 未参与分支展开为 undefined。
            };
            total
                .checked_add(units)
                .ok_or_else(|| "替换内容超过 UTF-16 长度范围".to_owned())
        })
    }
}
