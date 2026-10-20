// Copyright 2015 Google Inc. All rights reserved.
// 叶块权限沿固定 pulldown-cmark 0.13.4 firstpass；完整 MIT 条款见 LICENSE。

//! 已由真实 producer 闭合的非 inline 叶块；此票据不授予 prefix、inline 或 renderer Ready。

use std::ops::Range;

use crate::{
    firstpass::leaf_line_facts::{FenceInfoBoundsScanner, HeadingBoundsScanner},
    line_frontier::LineFacts,
    scanners::is_ascii_whitespace_no_nl,
    structural_spine::StructuralSpineSnapshot,
    HeadingLevel, MetadataBlockKind, Options,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafBlockKind {
    AtxHeading(HeadingLevel),
    ThematicBreak,
    FencedCode,
    Html,
    IndentedCode,
    Metadata(MetadataBlockKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 完成凭证与源中存在 closing marker 是两件事；EOF 与容器退出绝不伪造显式闭合。
pub enum LeafBlockCloseReason {
    CompleteLine,
    ExplicitMarker,
    HtmlTerminator,
    BlankLine,
    IndentBreak,
    ContainerExit,
    TrueEof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtmlLeafTermination {
    Terminator(&'static str),
    BlankLine { type7: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 每段都是原源坐标。enclosure 仍可能含后续容器前缀，不能直接当作去前缀的正文投影。
pub enum LeafBlockDetails {
    AtxHeading {
        opening_marker: Range<usize>,
        lexical_content: Range<usize>,
        trim_target: usize,
        closing_hashes: Option<Range<usize>>,
        attributes: Option<Range<usize>>,
    },
    ThematicBreak {
        /// marker 可被空白分隔；此 enclosure 不宣称每个字节都是 marker。
        marker_enclosure: Range<usize>,
    },
    FencedCode {
        marker: u8,
        count: usize,
        indent: usize,
        opening_marker: Range<usize>,
        opening_line: Range<usize>,
        info: Range<usize>,
        body_enclosure: Range<usize>,
        closing_marker: Option<Range<usize>>,
        closing_scanner_end: Option<usize>,
    },
    Html {
        termination: HtmlLeafTermination,
        body_enclosure: Range<usize>,
        terminator: Option<Range<usize>>,
    },
    IndentedCode {
        first_remaining_space: usize,
        body_enclosure: Range<usize>,
        last_nonblank_end: usize,
    },
    Metadata {
        kind: MetadataBlockKind,
        opening_marker: Range<usize>,
        opening_line: Range<usize>,
        body_enclosure: Range<usize>,
        closing_marker: Option<Range<usize>>,
        closing_scanner_end: Option<usize>,
    },
}

#[derive(Clone, Debug)]
/// 构造器与字段只属于真实 producer；调用者不能用任意 Range 或同 revision 文档造权限。
/// 一个票据只保固定标量与原共享 scope 页，不保源、映射、行集合或 AST。
pub struct VerifiedGlobalLeafBlock {
    revision: u64,
    source_end: usize,
    options: Options,
    kind: LeafBlockKind,
    source_range: Range<usize>,
    physical_start: usize,
    // 和原 opening 同次捕获；部分Tab列不能由最终marker字节坐标倒推。
    pub(super) first_state_origin: usize,
    pub(super) first_state: crate::line_start_stream::LineStartState,
    consumed_end: usize,
    close_reason: LeafBlockCloseReason,
    details: LeafBlockDetails,
    pub(super) spine: StructuralSpineSnapshot,
}

impl VerifiedGlobalLeafBlock {
    /// 共享原物理行算法，但保Leaf真实种类、闭合末端与祖先capture；不构造Inline/Paragraph票。
    pub fn line_origins(
        &self,
        limits: super::BlockOriginLimits,
    ) -> Result<super::BlockOriginCursor, super::GlobalFrontierError> {
        super::BlockOriginCursor::from_leaf(self, limits)
    }

    /// 只扫描原 Root ATX 正文的一帧；返回候选事实，不授予最终 inline 或样式权限。
    /// 连续 next 与完整扫描由原 Core Leaf 续体维护，不能拿一帧成功代表整块 Plain。
    pub fn plain_heading_content_step(
        &self,
        frame: crate::block_frontier::SourceFrame<'_>,
        authority: super::GlobalSourceAuthority,
        budget: usize,
    ) -> Result<(bool, super::GlobalWork), super::GlobalFrontierError> {
        use super::GlobalFrontierError as Error;
        if authority.cancelled {
            return Err(Error::Cancelled); // 原取消优先于本次 frame，恢复参数不能替原 owner 续权。
        }
        if authority.current_revision != self.revision {
            return Err(Error::SourceSuperseded);
        }
        let LeafBlockDetails::AtxHeading {
            lexical_content,
            trim_target,
            attributes,
            ..
        } = &self.details
        else {
            return Err(Error::InvalidFrame); // 其他 Leaf 不借此入口改写自己的 inline 规则。
        };
        if attributes.is_some() {
            return Err(Error::InvalidFrame); // 属性仍须完整短块 Parser，不能当作普通字面正文忽略。
        }
        if !(2048..=crate::block_frontier::MAX_FRONTIER_STEP_BYTES).contains(&budget) {
            return Err(Error::InvalidBudget);
        }
        let frame_end = frame.byte_start.checked_add(frame.bytes.len());
        if frame.source_revision != self.revision
            || frame.byte_start < lexical_content.start
            || frame.byte_start > *trim_target
            || frame.bytes.len() > crate::block_frontier::MAX_FRONTIER_STEP_BYTES
            || !frame_end.is_some_and(|end| end <= self.source_end)
            || (frame.real_eof && frame_end != Some(self.source_end))
        {
            return Err(Error::InvalidFrame); // Root/正文范围来自原 Leaf，不能把外部任意 range 当作块权限。
        }
        // 每帧固定表初始化与实际 peek/转换共同收费；有限正文不复制，也不构造第二个 Parser。
        let lut = crate::firstpass::special_bytes(&self.options);
        let count = frame
            .bytes
            .len()
            .min((budget - 128 - 256) / 2)
            .min(trim_target - frame.byte_start);
        let (read, general) = super::inline_semantic::scan_plain_bytes(
            &frame.bytes[..count],
            frame.byte_start,
            *trim_target,
            &lut,
            &mut None,
        );
        Ok((
            general,
            super::GlobalWork {
                byte_reads: read,
                transitions: 128 + 256 + read,
            },
        ))
    }

    pub fn source_revision(&self) -> u64 {
        self.revision
    }
    pub fn source_end(&self) -> usize {
        self.source_end
    }
    pub fn options(&self) -> Options {
        self.options
    }
    pub fn kind(&self) -> LeafBlockKind {
        self.kind
    }
    pub fn source_range(&self) -> Range<usize> {
        self.source_range.clone()
    }
    pub fn physical_start(&self) -> usize {
        self.physical_start
    }
    /// 原 firstpass 的返回 cursor；HTML 尾 prefix/EOF 可能使它早于 item.source_range.end。
    pub fn consumed_end(&self) -> usize {
        self.consumed_end
    }
    pub fn close_reason(&self) -> LeafBlockCloseReason {
        self.close_reason
    }
    pub fn details(&self) -> &LeafBlockDetails {
        &self.details
    }
    /// 本片不开放 scope 遍历或 prefix；深度只能作有限状态观测，不能代替原上下文。
    pub fn scope_depth(&self) -> usize {
        self.spine.len()
    }
    /// 只读取原祖先 capture；此类型没有把自身 Other 层压入票据，因此 self_role_depth 是 None。
    pub fn scope_cursor(&self) -> Result<super::CapturedScopeCursor, super::GlobalFrontierError> {
        super::CapturedScopeCursor::new(
            self.revision,
            self.source_end,
            self.source_range.start,
            None,
            &self.spine,
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PendingLeafKind {
    Atx(HeadingLevel),
    Rule,
    Fence {
        marker: u8,
        count: usize,
        indent: usize,
    },
    Html(HtmlLeafTermination),
    Indented {
        remaining: usize,
    },
    Metadata {
        marker: u8,
        opening_end: usize,
    },
}

/// 正在尝试的 closer 只保原输入锚点；探测失败时丢弃，不能把失败 run 带到后续闭合行。
#[derive(Clone, Copy, Debug)]
struct ClosingBounds {
    start: usize,
    marker: Option<u8>,
    run_end: usize,
    in_run: bool,
}

pub(super) struct PendingGlobalLeaf {
    revision: u64,
    source_end: usize,
    options: Options,
    kind: PendingLeafKind,
    start: usize,
    physical_start: usize,
    first_state_origin: usize,
    first_state: crate::line_start_stream::LineStartState,
    spine: StructuralSpineSnapshot,
    opening_end: usize,
    body_end: usize,
    first_line: bool,
    last_nonblank_end: usize,
    line_blank: bool,
    line_blank_decided: bool,
    heading_content_start: Option<usize>,
    heading: HeadingBoundsScanner,
    info: FenceInfoBoundsScanner,
    terminator_end: Option<usize>,
    closing: Option<ClosingBounds>,
    accepted_closing: Option<(Range<usize>, usize)>,
}

impl PendingGlobalLeaf {
    pub(super) fn new(
        revision: u64,
        source_end: usize,
        options: Options,
        kind: PendingLeafKind,
        start: usize,
        opening: (usize, usize, crate::line_start_stream::LineStartState),
        spine: StructuralSpineSnapshot,
    ) -> Self {
        let opening_end = match kind {
            PendingLeafKind::Metadata { opening_end, .. } => opening_end,
            _ => start,
        };
        Self {
            revision,
            source_end,
            options,
            kind,
            start,
            physical_start: opening.0,
            first_state_origin: opening.1,
            first_state: opening.2,
            spine,
            opening_end,
            body_end: opening_end,
            first_line: true,
            last_nonblank_end: start,
            line_blank: true,
            line_blank_decided: false,
            heading_content_start: None,
            heading: HeadingBoundsScanner::default(),
            info: FenceInfoBoundsScanner::default(),
            terminator_end: None,
            closing: None,
            accepted_closing: None,
        }
    }

    pub(super) fn begin_line(&mut self) {
        self.line_blank = true;
        self.line_blank_decided = false;
    }

    pub(super) fn observe_line(&mut self, start: usize, bytes: &[u8]) {
        for (offset, &byte) in bytes.iter().enumerate() {
            let absolute = start + offset;
            // 原 scan_blank_line 在首 CR/LF 就完成，raw-LF 正文可能仍继续；两种边界不能混淆。
            if !self.line_blank_decided && !is_ascii_whitespace_no_nl(byte) {
                self.line_blank = matches!(byte, b'\r' | b'\n');
                self.line_blank_decided = true;
            }
            if !self.first_line {
                continue;
            }
            match self.kind {
                PendingLeafKind::Atx(level) if absolute >= self.start + level as usize => {
                    // 只跳过原 heading 前导 no_nl 空白；后续 kernel 与 firstpass 完全共用。
                    if self.heading_content_start.is_none() && is_ascii_whitespace_no_nl(byte) {
                        continue;
                    }
                    self.heading_content_start.get_or_insert(absolute);
                    self.heading.observe(&[byte]);
                }
                PendingLeafKind::Fence { count, .. } if absolute >= self.start + count => {
                    self.info.observe(&[byte])
                }
                _ => (), // HTML/缩进/metadata 的正文只保物理包络，不在此解释 inline。
            }
        }
    }

    pub(super) fn finish_line(&mut self, line: LineFacts) {
        match self.kind {
            PendingLeafKind::Fence { .. } if self.first_line => self.opening_end = line.end,
            PendingLeafKind::Indented { .. } if !self.line_blank => {
                self.last_nonblank_end = line.end
            }
            _ => (),
        }
        self.body_end = line.end;
        self.first_line = false;
    }

    pub(super) fn note_terminator(&mut self, end: usize) {
        self.terminator_end = Some(end);
    }

    pub(super) fn begin_closing(&mut self, start: usize) {
        self.closing = Some(ClosingBounds {
            start,
            marker: None,
            run_end: start,
            in_run: true,
        });
    }

    pub(super) fn observe_closing(&mut self, start: usize, bytes: &[u8]) {
        // 只有当前原 closer 的尝试能拥有临时锚点；普通正文读入不会创建新的 marker 权限。
        let Some(bounds) = self.closing.as_mut() else {
            return;
        };
        for (offset, &byte) in bytes.iter().enumerate() {
            let absolute = start + offset;
            if bounds.marker.is_none() {
                bounds.marker = Some(byte);
            }
            if bounds.in_run && Some(byte) == bounds.marker {
                // 范围只扩展真实相邻的同字节 run；原 scanner 稍后独立决定它是否为合法 closer。
                bounds.run_end = absolute + 1;
            } else {
                // 末尾空白与 EOL 不属于 marker；后续相同字符也不能跨空白重新加入这个 run。
                bounds.in_run = false;
            }
        }
    }

    pub(super) fn accept_closing(&mut self, end: usize) {
        let Some(bounds) = self.closing.take() else {
            return;
        };
        // 原 fence scanner 对真空 EOF 返回 Closed(0)，此时没有显式 marker。
        if bounds.run_end > bounds.start {
            self.accepted_closing = Some((bounds.start..bounds.run_end, end));
        }
    }

    /// 原 scanner 拒绝后只丢临时锚点；正文包络由随后真实重读更新，不能吞掉前瞻行。
    pub(super) fn reject_closing(&mut self) {
        self.closing = None;
    }
    pub(super) fn closing_end(&self) -> Option<usize> {
        self.accepted_closing.as_ref().map(|(_, end)| *end)
    }
    pub(super) fn body_end(&self) -> usize {
        self.body_end
    }
    pub(super) fn last_nonblank_end(&self) -> usize {
        self.last_nonblank_end
    }

    pub(super) fn finish(
        self,
        semantic_end: usize,
        consumed_end: usize,
        close_reason: LeafBlockCloseReason,
    ) -> Option<VerifiedGlobalLeafBlock> {
        if self.physical_start > self.start
            || self.start > semantic_end
            || semantic_end > self.source_end
            || consumed_end > self.source_end
            || consumed_end < self.physical_start
        {
            return None; // 所有端点先独立验证，不能把不同语义的 end 强行相等来掩盖错误。
        }
        let (kind, details) = match self.kind {
            PendingLeafKind::Atx(level) => {
                // 样式边界只来自与原 firstpass 共用的 kernel；属性值和 inline 仍未在此解释。
                let start = self
                    .heading_content_start
                    .unwrap_or(self.start + level as usize);
                let bounds = self
                    .heading
                    .finish(self.options.contains(Options::ENABLE_HEADING_ATTRIBUTES));
                (
                    LeafBlockKind::AtxHeading(level),
                    LeafBlockDetails::AtxHeading {
                        opening_marker: self.start..self.start + level as usize,
                        lexical_content: start..start + bounds.content_end,
                        trim_target: start + bounds.trim_end,
                        closing_hashes: bounds
                            .closing_hashes
                            .map(|range| start + range.start..start + range.end),
                        attributes: bounds
                            .attributes
                            .map(|range| start + range.start..start + range.end),
                    },
                )
            }
            PendingLeafKind::Rule => (
                LeafBlockKind::ThematicBreak,
                LeafBlockDetails::ThematicBreak {
                    marker_enclosure: self.start..semantic_end,
                },
            ),
            PendingLeafKind::Fence {
                marker,
                count,
                indent,
            } => {
                // info 与 marker 是真实 opening/closer 的独立源段，正文不包含这两行。
                let info = self.info.finish();
                let after_marker = self.start + count;
                let (closing_marker, closing_scanner_end) = self
                    .accepted_closing
                    .map_or((None, None), |(range, end)| (Some(range), Some(end)));
                (
                    LeafBlockKind::FencedCode,
                    LeafBlockDetails::FencedCode {
                        marker,
                        count,
                        indent,
                        opening_marker: self.start..after_marker,
                        opening_line: self.start..self.opening_end,
                        info: after_marker + info.start..after_marker + info.end,
                        body_enclosure: self.opening_end..self.body_end,
                        closing_marker,
                        closing_scanner_end,
                    },
                )
            }
            PendingLeafKind::Html(termination) => {
                // 只投影同一个原 terminator 首次成功的坐标；容器退出也不会伪造缺失 terminator。
                let terminator = match (termination, self.terminator_end) {
                    (HtmlLeafTermination::Terminator(pattern), Some(end)) => {
                        Some(end - pattern.len()..end)
                    }
                    _ => None,
                };
                (
                    LeafBlockKind::Html,
                    LeafBlockDetails::Html {
                        termination,
                        body_enclosure: self.start..self.body_end,
                        terminator,
                    },
                )
            }
            PendingLeafKind::Indented { remaining } => (
                LeafBlockKind::IndentedCode,
                LeafBlockDetails::IndentedCode {
                    first_remaining_space: remaining,
                    body_enclosure: self.start..self.last_nonblank_end,
                    last_nonblank_end: self.last_nonblank_end,
                },
            ),
            PendingLeafKind::Metadata {
                marker,
                opening_end,
            } => {
                // opening 决定 metadata 种类；实际 closing 可以是 YAML dots，保留独立原 marker 范围。
                let kind = if marker == b'-' {
                    MetadataBlockKind::YamlStyle
                } else {
                    MetadataBlockKind::PlusesStyle
                };
                let (closing_marker, closing_scanner_end) = self
                    .accepted_closing
                    .map_or((None, None), |(range, end)| (Some(range), Some(end)));
                (
                    LeafBlockKind::Metadata(kind),
                    LeafBlockDetails::Metadata {
                        kind,
                        opening_marker: self.start..self.start + 3,
                        opening_line: self.start..opening_end,
                        body_enclosure: opening_end..self.body_end,
                        closing_marker,
                        closing_scanner_end,
                    },
                )
            }
        };
        // 固定数量的端点逐项检查；不以 clamp/max 把丢失的正文或 marker 范围伪装成有效。
        if !details.valid_for(semantic_end) {
            return None;
        }
        Some(VerifiedGlobalLeafBlock {
            revision: self.revision,
            source_end: self.source_end,
            options: self.options,
            kind,
            source_range: self.start..semantic_end,
            physical_start: self.physical_start,
            first_state_origin: self.first_state_origin,
            first_state: self.first_state,
            consumed_end,
            close_reason,
            details,
            spine: self.spine,
        })
    }
}

impl LeafBlockDetails {
    fn valid_for(&self, source_end: usize) -> bool {
        let valid = |range: &Range<usize>| range.start <= range.end && range.end <= source_end;
        let valid_optional = |range: &Option<Range<usize>>| range.as_ref().is_none_or(valid);
        match self {
            Self::AtxHeading {
                opening_marker,
                lexical_content,
                trim_target,
                closing_hashes,
                attributes,
            } => {
                valid(opening_marker)
                    && valid(lexical_content)
                    && lexical_content.start <= *trim_target
                    && *trim_target <= lexical_content.end
                    && valid_optional(closing_hashes)
                    && valid_optional(attributes)
            }
            Self::ThematicBreak { marker_enclosure } => valid(marker_enclosure),
            Self::FencedCode {
                opening_marker,
                opening_line,
                info,
                body_enclosure,
                closing_marker,
                closing_scanner_end,
                ..
            } => {
                valid(opening_marker)
                    && valid(opening_line)
                    && valid(info)
                    && valid(body_enclosure)
                    && opening_marker.end <= info.start
                    && info.end <= opening_line.end
                    && opening_line.end == body_enclosure.start
                    && valid_optional(closing_marker)
                    && closing_scanner_end.is_none_or(|end| end <= source_end)
            }
            Self::Html {
                body_enclosure,
                terminator,
                ..
            } => valid(body_enclosure) && valid_optional(terminator),
            Self::IndentedCode {
                body_enclosure,
                last_nonblank_end,
                ..
            } => valid(body_enclosure) && body_enclosure.end == *last_nonblank_end,
            Self::Metadata {
                opening_marker,
                opening_line,
                body_enclosure,
                closing_marker,
                closing_scanner_end,
                ..
            } => {
                valid(opening_marker)
                    && valid(opening_line)
                    && valid(body_enclosure)
                    && opening_marker.end <= opening_line.end
                    && opening_line.end == body_enclosure.start
                    && valid_optional(closing_marker)
                    && closing_scanner_end.is_none_or(|end| end <= source_end)
            }
        }
    }
}

#[cfg(test)]
mod tests;
