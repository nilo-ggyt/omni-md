// Copyright 2015 Google Inc. All rights reserved.
// 本地分段source协议与原metadata判定共用scanner；完整上游MIT许可见LICENSE。

//! 远处closing未决时的两个cursor：只做opening验证，不把lookahead末尾当作metadata实际块末尾。
//!
//! Accepted之后仍须从body_start按真实容器规则读取metadata；lookahead可能跨过已退出的容器，
//! 因而该结果不能直接授予table成员、block extent或跳过源区间的权限。

use crate::block_frontier::{
    FrontierError, SourceFrame, MAX_FRONTIER_FRAME_BYTES, MAX_FRONTIER_STEP_BYTES,
};
use crate::metadata_scan::{MetadataDecision, MetadataLookahead};

/// 所有frame在任何状态写入前统一验证；step上限只截取工作量，不把截取末尾伪装成EOF。
pub(crate) fn admitted_bytes(
    frame: &SourceFrame<'_>,
    revision: u64,
    source_end: usize,
    next_byte: usize,
    budget: usize,
) -> Result<usize, FrontierError> {
    if budget == 0 || budget > MAX_FRONTIER_STEP_BYTES {
        return Err(FrontierError::InvalidBudget);
    }
    let end = frame.byte_start.checked_add(frame.bytes.len());
    if frame.source_revision != revision
        || frame.bytes.len() > MAX_FRONTIER_FRAME_BYTES
        || frame.byte_start != next_byte
        || !end.is_some_and(|end| end <= source_end)
        || (frame.real_eof && end != Some(source_end))
    {
        return Err(FrontierError::InvalidFrame);
    }
    Ok(frame.bytes.len().min(budget))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataValidation {
    /// 真实EOF或首body非法否定opening；外层须从opening重分类，不能丢掉期间的普通表。
    Rejected { replay_from: usize },
    /// 仅证明远处确有raw closing；外层实际metadata读取从body_start开始，而非跳到closing。
    Accepted {
        body_start: usize,
        closing_start: usize,
        closing_marker_end: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataSourceStep {
    Progress {
        consumed: usize,
    },
    NeedInput {
        consumed: usize,
    },
    Validated {
        consumed: usize,
        result: MetadataValidation,
    },
}

#[derive(Clone, Copy, Debug)]
/// pending仅保锚点、scanner计数与已消费offset；没有正文String、AST、源owner或全量行集合。
pub struct PendingMetadataValidation {
    revision: u64,
    source_end: usize,
    opening: usize,
    body_start: usize,
    next_byte: usize,
    scanner: MetadataLookahead,
    result: Option<MetadataValidation>,
}

impl PendingMetadataValidation {
    /// 仅供结构producer在共享opening规则成功后使用；本对象不验证opening所处的block上下文。
    pub fn new(
        revision: u64,
        source_end: usize,
        opening: usize,
        body_start: usize,
        marker: u8,
    ) -> Result<Self, FrontierError> {
        if !matches!(marker, b'-' | b'+') || opening > body_start || body_start > source_end {
            return Err(FrontierError::InvalidFrame);
        }
        Ok(Self {
            revision,
            source_end,
            opening,
            body_start,
            next_byte: body_start,
            scanner: MetadataLookahead::new(marker, body_start),
            result: None,
        })
    }

    /// read cursor可单调越过巨长opaque正文；semantic committed cursor仍停opening，不能提前发布。
    pub fn next_byte(&self) -> usize {
        self.next_byte
    }
    pub fn committed_byte(&self) -> usize {
        self.opening
    }

    /// 单次最多64KiB；只有来源确认的真实EOF才把未闭合metadata判Rejected。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<MetadataSourceStep, FrontierError> {
        let limit = admitted_bytes(
            &frame,
            self.revision,
            self.source_end,
            self.next_byte,
            budget,
        )?;
        if let Some(result) = self.result {
            return Ok(MetadataSourceStep::Validated {
                consumed: 0,
                result,
            });
        }
        let mut consumed = 0;
        while consumed < limit {
            let decision = self
                .scanner
                .feed(&frame.bytes[consumed..consumed + 1], false);
            consumed += 1;
            self.next_byte += 1;
            if self.publish_decision(decision) {
                // 只报告本次真正消费的前缀；同frame剩余字节尚不属于已提交的metadata读取。
                return Ok(MetadataSourceStep::Validated {
                    consumed,
                    result: self.result.unwrap(),
                });
            }
        }
        if consumed == frame.bytes.len() && frame.real_eof {
            let decision = self.scanner.feed(&[], true);
            self.publish_decision(decision);
            return Ok(MetadataSourceStep::Validated {
                consumed,
                result: self.result.unwrap(),
            });
        }
        if consumed == frame.bytes.len() {
            Ok(MetadataSourceStep::NeedInput { consumed })
        } else {
            Ok(MetadataSourceStep::Progress { consumed })
        }
    }

    fn publish_decision(&mut self, decision: MetadataDecision) -> bool {
        self.result = match decision {
            MetadataDecision::NeedInput => return false,
            MetadataDecision::Rejected => Some(MetadataValidation::Rejected {
                replay_from: self.opening,
            }),
            MetadataDecision::Accepted {
                closing_start,
                closing_marker_end,
            } => Some(MetadataValidation::Accepted {
                body_start: self.body_start,
                closing_start,
                closing_marker_end,
            }),
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanners::{scan_metadata_block_opening, scan_nextline};
    use crate::{Event, Options, Parser, Tag};

    fn pending(source: &str) -> PendingMetadataValidation {
        let (opening, marker) = scan_metadata_block_opening(source.as_bytes(), true, true).unwrap();
        let body_start = opening + scan_nextline(&source.as_bytes()[opening..]);
        PendingMetadataValidation::new(11, source.len(), 0, body_start, marker).unwrap()
    }

    fn finish(
        state: &mut PendingMetadataValidation,
        source: &str,
        chunk: usize,
        budget: usize,
    ) -> MetadataValidation {
        loop {
            let start = state.next_byte();
            let end = (start + chunk).min(source.len());
            let step = state
                .advance(
                    SourceFrame {
                        source_revision: 11,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..end],
                        real_eof: end == source.len(),
                    },
                    budget,
                )
                .unwrap();
            match step {
                MetadataSourceStep::Validated { result, .. } => return result,
                MetadataSourceStep::Progress { consumed }
                | MetadataSourceStep::NeedInput { consumed } => {
                    assert!(consumed > 0, "本fixture只有未到真实EOF时才等待更多源");
                    assert!(consumed <= budget);
                    assert_eq!(state.committed_byte(), 0, "前瞻不得提交源或伪造块extent");
                }
            }
        }
    }

    #[test]
    fn true_eof_and_arbitrary_unicode_frames_match_real_metadata_opening() {
        for source in [
            "---\n标题😀\r\n---\n| H |\n| - |\n",
            "---\n标题😀\r\n| H |\n| - |\n",
            "+++\ntitle\n+++",
            "---\n\n---\n",
            "---\n---\n",
        ] {
            let expected = Parser::new_ext(
                source,
                Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
                    | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS,
            )
            .any(|event| matches!(event, Event::Start(Tag::MetadataBlock(_))));
            for chunk in 1..=source.len() {
                let mut state = pending(source);
                let actual = finish(&mut state, source, chunk, 3);
                assert_eq!(
                    matches!(actual, MetadataValidation::Accepted { .. }),
                    expected
                );
            }
        }
    }

    #[test]
    fn pending_cursor_rejects_false_eof_and_preserves_replay_anchor_after_long_opaque_line() {
        let mut source = String::from("---\n标题😀\n");
        source.extend(std::iter::repeat('x').take(4 * 1024 * 1024));
        source.push_str("\n| H |\n| - |\nbody\n");
        let mut state = pending(&source);
        let before = state.next_byte();
        assert_eq!(
            state.advance(
                SourceFrame {
                    source_revision: 11,
                    byte_start: before,
                    bytes: b"x",
                    real_eof: true,
                },
                1
            ),
            Err(FrontierError::InvalidFrame)
        );
        assert_eq!(state.next_byte(), before);
        assert_eq!(
            finish(&mut state, &source, 4096, 4096),
            MetadataValidation::Rejected { replay_from: 0 }
        );
        assert_eq!(state.committed_byte(), 0);
        assert!(std::mem::size_of::<PendingMetadataValidation>() <= 256);
    }
}
