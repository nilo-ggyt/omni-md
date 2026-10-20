// Copyright 2015 Google Inc. All rights reserved.
// 分支来源为固定pulldown-cmark0.13.4 scanners.rs；完整MIT条款见LICENSE。

//! 叶块opening只存固定前缀和计数；巨长HTML属性或fence info不形成整行缓冲。

use crate::block_context::get_html_end_tag;
use crate::leaf_scan::LeafStart;
use crate::scanners::{is_ascii_whitespace, is_ascii_whitespace_no_nl, starts_html_block_type_6};
use crate::HeadingLevel;

#[derive(Clone, Copy, Debug)]
enum Phase {
    First,
    HtmlPrefix,
    HtmlOpen,
    HtmlNameStart,
    HtmlName,
    HtmlBetween { space: bool },
    HtmlAttribute,
    HtmlAfterAttribute { space: bool },
    HtmlValueStart,
    HtmlQuoted(u8),
    HtmlUnquoted,
    HtmlSlash,
    HtmlTrailing,
    Rule,
    RuleCr,
    Heading,
    FenceRun,
    FenceInfo,
    Done,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LeafProbeStep {
    Progress,
    NeedInput,
    Complete(LeafStart),
}

/// next可为回放起点；读过的lookahead不等于已提交block范围。
pub(crate) struct LeafProbe {
    origin: usize,
    next: usize,
    phase: Phase,
    prefix: [u8; 16],
    prefix_len: usize,
    marker: u8,
    count: usize,
    close_tag: bool,
    result: Option<LeafStart>,
    #[cfg(feature = "omni-block-frontier")]
    html_type7: bool,
}

impl LeafProbe {
    pub(crate) fn new(origin: usize) -> Self {
        Self {
            origin,
            next: origin,
            phase: Phase::First,
            prefix: [0; 16],
            prefix_len: 0,
            marker: 0,
            count: 0,
            close_tag: false,
            result: None,
            #[cfg(feature = "omni-block-frontier")]
            html_type7: false,
        }
    }

    pub(crate) fn next_byte(&self) -> usize {
        self.next
    }
    #[cfg(feature = "omni-block-frontier")]
    pub(crate) fn html_type7(&self) -> bool {
        self.html_type7
    }

    fn finish(&mut self, result: LeafStart) {
        self.result = Some(result);
        self.phase = Phase::Done;
    }

    fn reject(&mut self) {
        self.finish(LeafStart::DefinitionOrParagraph);
    }

    /// 一次transition最多处理一个字节或一次有限状态转换；调用者统一验证frame来源和EOF权限。
    pub(crate) fn advance(
        &mut self,
        bytes: &[u8],
        real_eof: bool,
        budget: usize,
    ) -> (LeafProbeStep, usize, usize) {
        let mut read = 0;
        let mut work = 0;
        while work < budget {
            if let Some(result) = self.result {
                return (LeafProbeStep::Complete(result), read, work);
            }
            let byte = bytes.get(read).copied();
            if byte.is_none() && !real_eof {
                return (LeafProbeStep::NeedInput, read, work);
            }
            work += 1;
            let consumed = self.step(byte);
            if consumed {
                read += 1;
                self.next += 1;
            }
            if self.next == self.origin && matches!(self.phase, Phase::HtmlOpen) {
                // 固定前缀只证明1–6失败；type7从真实opening重放，不能丢掉先前借过的属性字节。
                return (LeafProbeStep::Progress, read, work);
            }
        }
        let result = self
            .result
            .map_or(LeafProbeStep::Progress, LeafProbeStep::Complete);
        (result, read, work)
    }

    fn step(&mut self, byte: Option<u8>) -> bool {
        match self.phase {
            Phase::First => {
                // 叶块优先级与non_metadata_leaf_start相同；不同首字节不会共享候选正文。
                match byte {
                    Some(b'<') => self.phase = Phase::HtmlPrefix,
                    Some(marker @ (b'*' | b'-' | b'_')) => {
                        self.marker = marker;
                        self.phase = Phase::Rule;
                    }
                    Some(b'#') => self.phase = Phase::Heading,
                    Some(marker @ (b'`' | b'~')) => {
                        self.marker = marker;
                        self.phase = Phase::FenceRun;
                    }
                    _ => self.reject(),
                }
                false
            }
            Phase::HtmlPrefix => {
                let stop = match byte {
                    Some(byte) => {
                        self.prefix[self.prefix_len] = byte;
                        self.prefix_len += 1;
                        self.prefix_len == self.prefix.len() || matches!(byte, b'\r' | b'\n')
                    }
                    None => true,
                };
                if stop {
                    // 最长type6标签名10字节，16字节已包含其必要边界；短前缀只在真实EOL/EOF判断。
                    let prefix = &self.prefix[..self.prefix_len];
                    if let Some(end) = get_html_end_tag(&prefix[1..]) {
                        self.finish(LeafStart::HtmlTerminated(end));
                    } else if starts_html_block_type_6(&prefix[1..]) {
                        self.finish(LeafStart::HtmlBlankTerminated);
                    } else {
                        self.next = self.origin;
                        self.phase = Phase::HtmlOpen;
                        return false;
                    }
                }
                byte.is_some()
            }
            Phase::HtmlOpen => {
                // 回放首字符只做原scan_html_block_inner已要求的'<'，不扩展成任意HTML容错parser。
                if byte == Some(b'<') {
                    self.phase = Phase::HtmlNameStart;
                } else {
                    self.reject();
                }
                byte.is_some()
            }
            Phase::HtmlNameStart => {
                match byte {
                    Some(b'/') if !self.close_tag => {
                        self.close_tag = true;
                        return true;
                    }
                    Some(byte) if byte.is_ascii_alphabetic() => self.phase = Phase::HtmlName,
                    _ => self.reject(),
                }
                byte.is_some()
            }
            Phase::HtmlName => {
                match byte {
                    Some(byte) if byte.is_ascii_alphanumeric() || byte == b'-' => return true,
                    _ => self.phase = Phase::HtmlBetween { space: false },
                }
                false
            }
            Phase::HtmlBetween { space } => {
                match byte {
                    Some(byte) if is_ascii_whitespace_no_nl(byte) => {
                        self.phase = Phase::HtmlBetween { space: true };
                        return true;
                    }
                    Some(b'>') => {
                        self.phase = Phase::HtmlTrailing;
                        return true;
                    }
                    Some(b'/') if !self.close_tag => {
                        self.phase = Phase::HtmlSlash;
                        return true;
                    }
                    Some(byte) if !self.close_tag && space && attr_name_start(byte) => {
                        self.phase = Phase::HtmlAttribute;
                        return true;
                    }
                    _ => self.reject(),
                }
                false
            }
            Phase::HtmlAttribute => {
                match byte {
                    Some(byte) if attr_name_continue(byte) => return true,
                    _ => self.phase = Phase::HtmlAfterAttribute { space: false },
                }
                false
            }
            Phase::HtmlAfterAttribute { space } => {
                match byte {
                    Some(byte) if is_ascii_whitespace_no_nl(byte) => {
                        self.phase = Phase::HtmlAfterAttribute { space: true };
                        return true;
                    }
                    Some(b'=') => {
                        self.phase = Phase::HtmlValueStart;
                        return true;
                    }
                    _ => {
                        // 原scanner无'='时回退到attribute name末尾；已读空白可等价交回外层分隔权限。
                        self.phase = Phase::HtmlBetween { space };
                    }
                }
                false
            }
            Phase::HtmlValueStart => {
                match byte {
                    Some(byte) if is_ascii_whitespace_no_nl(byte) => return true,
                    Some(quote @ (b'\'' | b'"')) => {
                        self.phase = Phase::HtmlQuoted(quote);
                        return true;
                    }
                    Some(byte) if crate::scanners::is_valid_unquoted_attr_value_char(byte) => {
                        self.phase = Phase::HtmlUnquoted;
                        return true;
                    }
                    _ => self.reject(),
                }
                false
            }
            Phase::HtmlQuoted(quote) => {
                match byte {
                    Some(byte) if byte == quote => {
                        self.phase = Phase::HtmlBetween { space: false };
                        return true;
                    }
                    Some(b'\r' | b'\n') | None => self.reject(),
                    Some(_) => return true,
                }
                false
            }
            Phase::HtmlUnquoted => {
                match byte {
                    Some(byte) if crate::scanners::is_valid_unquoted_attr_value_char(byte) => {
                        return true;
                    }
                    _ => self.phase = Phase::HtmlBetween { space: false },
                }
                false
            }
            Phase::HtmlSlash => {
                if byte == Some(b'>') {
                    self.phase = Phase::HtmlTrailing;
                    return true;
                }
                self.reject();
                false
            }
            Phase::HtmlTrailing => {
                match byte {
                    Some(byte) if is_ascii_whitespace_no_nl(byte) => return true,
                    Some(b'\r' | b'\n') | None => {
                        #[cfg(feature = "omni-block-frontier")]
                        {
                            self.html_type7 = true;
                        }
                        self.finish(LeafStart::HtmlBlankTerminated);
                    }
                    _ => self.reject(),
                }
                false
            }
            Phase::Rule => {
                match byte {
                    Some(byte) if byte == self.marker => {
                        self.count += 1;
                        return true;
                    }
                    Some(b' ' | b'\t') => return true,
                    Some(b'\r') if self.count >= 3 => {
                        self.phase = Phase::RuleCr;
                        return true;
                    }
                    Some(b'\n') if self.count >= 3 => {
                        self.finish(LeafStart::Rule(self.next + 1 - self.origin));
                        return true;
                    }
                    None if self.count >= 3 => {
                        self.finish(LeafStart::Rule(self.next - self.origin))
                    }
                    _ => self.reject(),
                }
                false
            }
            Phase::RuleCr => {
                // CRLF可跨frame；只有真正看到LF才把第二字节纳入rule范围。
                let extra = usize::from(byte == Some(b'\n'));
                self.finish(LeafStart::Rule(self.next + extra - self.origin));
                extra != 0
            }
            Phase::Heading => {
                match byte {
                    Some(b'#') if self.count < 6 => {
                        self.count += 1;
                        return true;
                    }
                    Some(byte) if is_ascii_whitespace(byte) && self.count > 0 => {
                        self.finish(LeafStart::Heading(
                            HeadingLevel::try_from(self.count).unwrap(),
                        ));
                    }
                    None if self.count > 0 => self.finish(LeafStart::Heading(
                        HeadingLevel::try_from(self.count).unwrap(),
                    )),
                    _ => self.reject(),
                }
                false
            }
            Phase::FenceRun => {
                if byte == Some(self.marker) {
                    self.count += 1;
                    return true;
                }
                if self.count < 3 {
                    self.reject();
                } else if self.marker == b'~' {
                    self.finish(LeafStart::Fence {
                        marker: self.marker,
                        count: self.count,
                    });
                } else {
                    self.phase = Phase::FenceInfo;
                }
                false
            }
            Phase::FenceInfo => {
                match byte {
                    Some(b'`') => self.reject(),
                    Some(b'\n') | None => self.finish(LeafStart::Fence {
                        marker: self.marker,
                        count: self.count,
                    }),
                    Some(_) => return true, // 原opening info以raw LF终止，lone CR不是提前成功权限。
                }
                false
            }
            Phase::Done => false,
        }
    }
}

fn attr_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':')
}
fn attr_name_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-')
}

#[cfg(test)]
mod tests;
