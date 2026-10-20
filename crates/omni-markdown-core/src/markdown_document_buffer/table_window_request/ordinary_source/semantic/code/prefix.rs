//! 真 Origin 前缀的视觉标记；只读已授权前缀，不能从正文或裸位置识别容器。

use super::*;
use pulldown_cmark::global_frontier::VerifiedInlinePrefix;

#[derive(Clone, Copy)]
enum Marker {
    Quote,
    Item,
    OrderedDigits,
}

/// 一个真实前缀只保当前标记和物理行活动事实；不保存行目录、完整字符串或第二份 scope。
pub(in super::super::super) struct PrefixDecoration {
    // 这枚票只来自同块真实 scanner；原始 range 约束本游标能借的全部字节。
    // 它没有 Doc/mmap 强引用，失败和取消仍随唯一 CodeJob 释放。
    proof: PrefixSource,
    next: usize,
    // None 必须向原 SourceRequest 取得同 Rc 行，不能用默认 false 提前隐藏当前输入行。
    // Some 只保存该次完整选择与真 physical/EOL 的计算结果，不是可复用到另一代的权限。
    owner_active: Option<bool>,
    // 一个视觉标记可跨小帧，最多保固定端点；不会为嵌套层数分配 token 列表。
    marker: Option<(Range<usize>, Marker)>,
}

/// 两类来源都来自原 scanner；Leaf 保真实 Origin，不能用裸 prefix range 取得显示权限。
enum PrefixSource {
    Inline(VerifiedInlinePrefix),
    Leaf(BlockOriginLine),
    Paragraph(pulldown_cmark::global_frontier::InlineParagraphLine),
}

impl PrefixSource {
    fn range(&self) -> Range<usize> {
        match self {
            Self::Inline(proof) => proof.source_range(),
            Self::Leaf(origin) => origin.prefix_range(),
            Self::Paragraph(line) => line.prefix_range(),
        }
    }
}

impl PrefixDecoration {
    pub(super) fn new(proof: VerifiedInlinePrefix) -> Self {
        Self {
            next: proof.source_range().start,
            proof: PrefixSource::Inline(proof),
            owner_active: None,
            marker: None,
        }
    }

    /// 只接真实 Leaf Origin；活动归属直接使用原物理行/EOL，不为 prefix 再扫整条正文。
    pub(in super::super::super) fn for_leaf(
        origin: BlockOriginLine,
        selection_bytes: &Range<usize>,
    ) -> Self {
        let active = crate::markdown_viewport_presentation::markdown_physical_line_is_active(
            selection_bytes,
            &origin.physical_range(),
            origin.eol_range().is_empty(),
        );
        Self {
            next: origin.prefix_range().start,
            proof: PrefixSource::Leaf(origin),
            owner_active: Some(active),
            marker: None,
        }
    }

    /// 完整短段落的原 context 已证明这条物理行；只借该前缀事实，不读取或保留整行正文。
    pub(in super::super::super) fn for_paragraph(
        line: pulldown_cmark::global_frontier::InlineParagraphLine,
        selection_bytes: &Range<usize>,
    ) -> Self {
        let active = crate::markdown_viewport_presentation::markdown_physical_line_is_active(
            selection_bytes,
            &line.physical_range(),
            line.eol_range().is_empty(),
        );
        Self {
            next: line.prefix_range().start,
            proof: PrefixSource::Paragraph(line),
            owner_active: Some(active),
            marker: None,
        }
    }

    /// Origin 已证明这段是结构前缀；此处仅沿原 QuoteMark/Item marker 的显示宽度规则分段。
    /// 额外缩进、Tab 残余列与 paragraph 空白都保留正文样式，不因位于 prefix 而整体隐藏。
    pub(in super::super::super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
        footnote_openings: &[Range<usize>],
    ) -> Result<(SemanticWork, Option<SpanProof>, bool), MarkdownTableRequestError> {
        let active = self
            .owner_active
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        let end = self.proof.range().end;
        // 没有后继字节的标记在已证 prefix 末端直接结束；source EOF 与此有限末端仍是两种事实。
        // 真正的 Source 健康检查由外层同请求执行，游标不会构造虚拟 EOF frame。
        if self.next == end {
            let marker = self
                .marker
                .take()
                .map(|(range, kind)| Self::span(range, kind, active));
            return Ok((work, marker, true));
        }
        // 原 scope 的 opening 按 producer 源序排列，彼此不重叠；只查询当前 prefix 的下一项。
        // 每个实际比较都交回原量子账；不按每个字节或每条行前缀从头重扫整个祖先目录。
        let mut comparisons = 0;
        let index = footnote_openings.partition_point(|opening| {
            comparisons += 1;
            opening.end <= self.next
        });
        work.structural_transitions += comparisons;
        let end = match footnote_openings.get(index) {
            Some(opening) if opening.start <= self.next => {
                if let Some((range, kind)) = self.marker.take() {
                    return Ok((work, Some(Self::span(range, kind, active)), false));
                } // 先归还 opening 前真正的 quote/item，不能让其 padding 跨入 label。
                self.next = opening.end.min(end);
                return Ok((work, None, self.next == end));
            }
            Some(opening) => end.min(opening.start),
            None => end,
        }; // label/colon/已消费空白保持原字面；跳过权限来自原 scanner，不能重判 [] 或数字。
        // 每帧借用仍付原树路径、字节和转换账；只读取 prefix，绝不为 owner 再扫完整长行。
        // 剩余量不足就明确失败，不能把未完成的数字或 padding 当作整段成功。
        let tree = tree_height(document);
        let wanted =
            (budget.saturating_sub(work.structural_transitions + tree) / 4).min(end - self.next);
        if wanted == 0 {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let frame = document
            .table_source_frame(self.next, wanted)
            .map_err(MarkdownTableRequestError::Source)?;
        work.source_tree_steps = tree;
        work.structural_transitions += tree;
        let mut found = None;
        let mut mismatch = false;
        for &byte in frame.bytes.iter().take(end - self.next) {
            // 一个实际 peek 与至多两个状态转换先收费；未消费的 padding lookahead 下量子仍会重读。
            work.source_bytes += 1;
            work.structural_transitions += 2;
            if let Some((range, kind)) = self.marker.as_mut() {
                match kind {
                    Marker::OrderedDigits if byte.is_ascii_digit() => {
                        range.end += 1;
                        self.next += 1;
                        continue;
                    }
                    Marker::OrderedDigits if matches!(byte, b'.' | b')') => {
                        range.end += 1;
                        self.next += 1;
                        *kind = Marker::Item;
                        continue;
                    }
                    Marker::OrderedDigits => {
                        // 来源票只证明真实 Quote/List/Item 前缀；数字没有原 delimiter 说明内部票失配。
                        mismatch = true;
                        break;
                    }
                    Marker::Quote | Marker::Item => {
                        let padding = byte == b' ' || matches!(kind, Marker::Item) && byte == b'\t';
                        if padding {
                            range.end += 1;
                            self.next += 1;
                        } // Quote 只包含紧随的一个 ASCII 空格，Tab 不在隐藏字节范围内。
                        let (range, kind) = self.marker.take().unwrap();
                        found = Some(Self::span(range, kind, active));
                        break;
                    }
                }
            }
            // 原 scope 已限定真实容器并排除 Footnote opening，scanner 已证明整个 prefix；此处不判块。
            // 数字与标点只决定已证 opening 的视觉跨度，正文中的相同字符没有进入本入口的权限。
            let at = self.next;
            self.next += 1;
            let kind = match byte {
                b'>' => Some(Marker::Quote),
                b'-' | b'+' | b'*' => Some(Marker::Item),
                b'0'..=b'9' => Some(Marker::OrderedDigits),
                _ => None,
            };
            self.marker = kind.map(|kind| (at..self.next, kind));
        }
        // 先归还真实映射再消费视觉结果；reclaim 首错优先，不以“已找到标记”掩盖源释放失败。
        // 匹配错误仅撤销本次请求，原 Canonical 文本、选择与历史都没有被写入。
        frame.finish().map_err(MarkdownTableRequestError::Source)?;
        if mismatch {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok((work, found, false))
    }

    fn span(range: Range<usize>, kind: Marker, active: bool) -> SpanProof {
        let mut styles = MarkdownPresentationStyles::MARKER;
        if matches!(kind, Marker::Quote) && !active {
            styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
        }
        // body 与 whole 相同：这里的显隐已经按真实物理行决定，不能被普通 inline owner 二次覆盖。
        SpanProof {
            whole: range.clone(),
            body: range,
            styles,
        }
    }
}

impl CodeJob {
    /// 每条前缀只从上次已消费的原记录继续；不为长表/多行窗口建立另一张来源目录。
    pub(in super::super) fn prefix_origin_record_index(&self) -> Option<usize> {
        self.prefix
            .as_ref()
            .filter(|prefix| prefix.owner_active.is_none())
            .map(|_| self.prefix_record_index)
    }

    /// 父级已核同 Rc block；本步再核 exact prefix，并只消费这一条原记录的固定事实。
    pub(in super::super) fn advance_prefix_origin(
        &mut self,
        origin: Option<&BlockOriginLine>,
        selection_bytes: &Range<usize>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        // 原页仍由父请求持有；本步只借一条记录，不把 Rc 或整行存入新的 owner 图。
        // 同块条件在父级先核，下面的起点次序与 exact-prefix 条件不能用数字相似来替代。
        let prefix = self
            .prefix
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if prefix.owner_active.is_some() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if let Some(origin) = origin {
            let PrefixSource::Inline(proof) = &prefix.proof else {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }; // CodeJob 只等待原 Inline prefix 的配对，不能用 Leaf 的相似位置代替其权限。
            let target = proof.source_range().start;
            let physical = origin.physical_range();
            if physical.start > target {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if physical.start == target {
                if !proof.matches_origin(origin) {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                // CRLF 的 EOL 末端属于下一行；只有真实无 EOL 终行才可让 EOF caret 激活该行。
                // 该谓词与旧预览共用，反向选择亦只由两个真实端点决定，不激活中间所有行。
                prefix.owner_active = Some(
                    crate::markdown_viewport_presentation::markdown_physical_line_is_active(
                        selection_bytes,
                        &physical,
                        origin.eol_range().is_empty(),
                    ),
                );
            }
        }
        // 记录来自当前 SourceRequest 的有序页；相邻前缀只向前借，不重置已消费的索引或总预算。
        self.prefix_record_index += 1;
        Ok(SemanticWork {
            structural_transitions: PLAIN_CONTROL + std::mem::size_of::<Rc<SourceRecord>>(),
            ..Default::default()
        })
    }
}
