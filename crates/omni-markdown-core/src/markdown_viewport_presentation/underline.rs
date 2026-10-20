//! 原产品精确`<u>`配对的分段状态；不在省略行后的投影文本里重新配对。

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SanitizedUnderlinePair {
    pub(super) open_start: usize,
    pub(super) content_start: usize,
    pub(super) close_start: usize,
    pub(super) end: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 只保原始source位置和固定pattern的匹配长度；Copy/checkpoint不持任何正文。
pub(super) struct SanitizedUnderlineScanner {
    next_byte: usize,
    open: Option<usize>,
    matched: usize,
}

impl SanitizedUnderlineScanner {
    pub(super) fn next_byte(self) -> usize {
        self.next_byte
    }

    /// 同次Parser已证明的代码段只推进原byte坐标；保留外部完整开标签，截断未完成token。
    pub(super) fn skip_literal(&mut self, bytes: usize) {
        self.next_byte += bytes;
        self.matched = 0; // 代码前后的半个标签不能拼接；代码内假close也不能吃掉外部open。
    }

    #[cfg(feature = "s2-table-window-prototype")]
    pub(super) fn has_open(self) -> bool {
        self.open.is_some()
    }

    /// 按原first-open→next-close顺序处理一个真实源byte；已有open时忽略后续open。
    pub(super) fn push(&mut self, byte: u8) -> Option<SanitizedUnderlinePair> {
        let pattern: &[u8] = if self.open.is_some() { b"</u>" } else { b"<u>" };
        if byte == pattern[self.matched] {
            // 两个固定pattern仅首位含'<'，故不需存源前缀或建立动态KMP表。
            self.matched += 1;
        } else {
            self.matched = usize::from(byte == b'<');
        }
        self.next_byte += 1;
        if self.matched != pattern.len() {
            return None;
        }
        self.matched = 0;
        if let Some(open_start) = self.open.take() {
            Some(SanitizedUnderlinePair {
                open_start,
                content_start: open_start + 3,
                close_start: self.next_byte - 4,
                end: self.next_byte,
            })
        } else {
            self.open = Some(self.next_byte - 3);
            None
        }
    }

    /// 已越过所有请求源区间时，只有可能覆盖它们的open/partial tag仍需继续真实前瞻。
    #[cfg(any(test, feature = "s2-table-window-prototype"))]
    pub(super) fn settled_after(self, end: usize) -> bool {
        if self.next_byte < end {
            return false;
        }
        if let Some(open) = self.open {
            return open >= end;
        }
        self.matched == 0 || self.next_byte - self.matched >= end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original(source: &str) -> Vec<SanitizedUnderlinePair> {
        let mut result = Vec::new();
        let mut search = 0;
        while let Some(relative) = source[search..].find("<u>") {
            let open_start = search + relative;
            let content_start = open_start + 3;
            let Some(relative) = source[content_start..].find("</u>") else {
                break;
            };
            let close_start = content_start + relative;
            let end = close_start + 4;
            result.push(SanitizedUnderlinePair {
                open_start,
                content_start,
                close_start,
                end,
            });
            search = end;
        }
        result
    }

    #[test]
    fn exact_pairing_matches_original_across_every_byte_chunk_and_nested_open() {
        for source in [
            "<u>header\nomitted <u>row\nvisible</u>tail",
            "中😀<u></u><u>two</u>",
            "<<u>literal</u><U>case</U><u attr>x</u>",
            "<u>unmatched",
            "<u>cross\r\nline</u>",
        ] {
            let expected = original(source);
            for chunk in 1..=source.len() {
                let mut scanner = SanitizedUnderlineScanner::default();
                let mut actual = Vec::new();
                for frame in source.as_bytes().chunks(chunk) {
                    for byte in frame {
                        if let Some(pair) = scanner.push(*byte) {
                            actual.push(pair);
                        }
                    }
                }
                assert_eq!(actual, expected, "chunk={chunk}");
                assert_eq!(scanner.next_byte(), source.len());
            }
        }
    }

    #[test]
    fn coverage_end_waits_for_partial_open_and_real_close_without_holding_body() {
        let mut scanner = SanitizedUnderlineScanner::default();
        for byte in b"plain<" {
            scanner.push(*byte);
        }
        assert!(!scanner.settled_after(6));
        scanner.push(b'u');
        assert!(!scanner.settled_after(6));
        scanner.push(b'>');
        assert!(!scanner.settled_after(6));
        for _ in 0..4 * 1024 * 1024 {
            scanner.push(b'x');
        }
        for byte in b"</u>" {
            scanner.push(*byte);
        }
        assert!(scanner.settled_after(6));
        assert!(std::mem::size_of::<SanitizedUnderlineScanner>() <= 32);
        assert!(!std::mem::needs_drop::<SanitizedUnderlineScanner>());
    }

    #[test]
    /// 字面代码保留外部完整开标签及源坐标；两侧残缺标签不能越过代码重新拼接。
    fn literal_code_preserves_outer_open_and_original_offsets_without_joining_partial_tags() {
        let mut scanner = SanitizedUnderlineScanner::default();
        for byte in b"<u>left</" {
            assert!(scanner.push(*byte).is_none());
        }
        scanner.skip_literal(4 * 1024 * 1024); // 原Parser代码段无需交给allowlist；本扫描器仍不持正文。
        for byte in b"u>after" {
            assert!(scanner.push(*byte).is_none());
        } // 代码前的半个close已被截断，不能跨代码恢复匹配。
        let mut actual = None;
        for byte in b"</u>" {
            actual = scanner.push(*byte).or(actual);
        }
        let pair = actual.unwrap();
        assert_eq!(pair.open_start, 0);
        assert_eq!(pair.content_start, 3);
        assert_eq!(
            pair.close_start,
            b"<u>left</".len() + 4 * 1024 * 1024 + b"u>after".len()
        );
        assert_eq!(pair.end, scanner.next_byte());

        let mut scanner = SanitizedUnderlineScanner::default();
        scanner.push(b'<');
        scanner.skip_literal(7);
        for byte in b"u>x</u>" {
            assert!(scanner.push(*byte).is_none());
        } // 未完成open同样不跨代码拼接；之后真正完整的pair仍使用原绝对offset。
        let start = scanner.next_byte();
        let mut actual = None;
        for byte in b"<u>x</u>" {
            actual = scanner.push(*byte).or(actual);
        }
        assert_eq!(actual.unwrap().open_start, start);
    }
}
