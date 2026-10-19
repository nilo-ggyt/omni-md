// Copyright 2015 Google Inc. All rights reserved.
// Copyright 2018 Google LLC
// 本文件的原始扫描规则从固定pulldown-cmark0.13.4的parse.rs/firstpass.rs机械提取；
// 使用原项目MIT许可，完整条款保留在本目录LICENSE。新增view只拆开状态读取，不另造语法。

//! AST builder与有限结构frontier共享的容器扫描/paragraph中断规则。

use crate::linklabel::scan_link_label_rest;
use crate::parse::{Item, ItemBody};
use crate::scanners::*;
use crate::tree::Tree;
use crate::{BlockQuoteKind, Options};

#[derive(Clone, Copy, Debug)]
/// footnote候选已判定之后的新容器决策；不分配节点，也不承担未决多行label的权限。
pub(crate) enum ContainerOpening {
    List {
        marker: u8,
        index: u64,
        indent: usize,
    },
    Definition {
        indent: usize,
    },
    Quote {
        kind: Option<BlockQuoteKind>,
    },
    None,
}

/// 机械共享原parse_block的list→definition→quote顺序；eligibility保持惰性读取。
pub(crate) fn scan_container_opening(
    line_start: &mut LineStart<'_>,
    outer_indent: usize,
    options: Options,
    definition_eligible: impl FnOnce() -> bool,
) -> ContainerOpening {
    if let Some((marker, index, indent)) = line_start.scan_list_marker_with_indent(outer_indent) {
        // list优先；有有效marker时不读取definition上下文，保留原短路行为。
        return ContainerOpening::List {
            marker,
            index,
            indent,
        };
    }
    if options.contains(Options::ENABLE_DEFINITION_LIST) && definition_eligible() {
        if let Some(indent) =
            line_start.scan_definition_list_definition_marker_with_indent(outer_indent)
        {
            // 原有前一child资格由各sink提供；不能仅凭冒号建立definition容器。
            return ContainerOpening::Definition { indent };
        }
    }
    if line_start.scan_blockquote_marker() {
        let kind = if options.contains(Options::ENABLE_GFM) {
            line_start.scan_blockquote_tag()
        } else {
            None
        };
        return ContainerOpening::Quote { kind };
    }
    // 调用者仍恢复进入本轮前的LineStart；失败扫描不得吃掉paragraph原文空格。
    ContainerOpening::None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 仅保扫描会读取的结构角色；不带alignment/footnote/Cow的AST分配索引。
pub(crate) enum ContainerRole {
    Quote,
    Item(usize),
    Definition(usize),
    Footnote,
    Other,
}

/// 按真实spine顺序借用角色；iterator不分配，不以局部Vec下标替代层级。
pub(crate) trait ContainerSpine {
    fn visit_roles(&self, visit: impl FnMut(ContainerRole) -> bool);
}

/// 中断判断按原短路顺序取最小状态，不提前遍历spine制造默认额外成本。
pub(crate) trait ParagraphContext {
    fn is_in_table(&self) -> bool;
    fn parent_is_paragraph(&self) -> bool;
    fn spine_entry_is_definition(&self, index: usize) -> bool;
}

impl ContainerSpine for Tree<Item> {
    fn visit_roles(&self, mut visit: impl FnMut(ContainerRole) -> bool) {
        for index in self.walk_spine() {
            let role = match self[*index].item.body {
                // 只共享结构角色，不读Cow/alignment索引；每层只访问一次，避免nth导致重复遍历。
                ItemBody::BlockQuote(..) => ContainerRole::Quote,
                ItemBody::ListItem(indent) => ContainerRole::Item(indent),
                ItemBody::DefinitionListDefinition(indent) => ContainerRole::Definition(indent),
                ItemBody::FootnoteDefinition(..) => ContainerRole::Footnote,
                _ => ContainerRole::Other,
            };
            if !visit(role) {
                break;
            }
        }
    }
}
impl ParagraphContext for Tree<Item> {
    fn is_in_table(&self) -> bool {
        Tree::is_in_table(self)
    }
    fn parent_is_paragraph(&self) -> bool {
        self.peek_up().is_some_and(|cur| {
            matches!(
                self[cur].item.body,
                ItemBody::Paragraph | ItemBody::TightParagraph | ItemBody::MaybeDefinitionListTitle
            )
        })
    }
    fn spine_entry_is_definition(&self, index: usize) -> bool {
        self.walk_spine().nth(index).is_some_and(|cur| {
            matches!(self[*cur].item.body, ItemBody::DefinitionListDefinition(_))
        })
    }
}

pub(crate) fn scan_containers(
    tree: &impl ContainerSpine,
    line_start: &mut LineStart<'_>,
    options: Options,
) -> usize {
    let mut count = 0;
    tree.visit_roles(|role| {
        // 泛型visitor静态分发，首个不匹配层立即停止；无dyn callback、额外分配或MSRV提升。
        if scan_container_role(role, line_start, options) {
            count += 1;
            true
        } else {
            false
        }
    });
    count
}

#[derive(Clone, Copy, Debug)]
/// 原scan_container_role读取的唯一扫描计划；分段续体与AST wrapper共用本match。
pub(crate) enum ContainerRolePlan {
    Quote,
    Indent(usize),
    Pass,
}

pub(crate) fn container_role_plan(role: ContainerRole, options: Options) -> ContainerRolePlan {
    match role {
        ContainerRole::Quote => ContainerRolePlan::Quote,
        ContainerRole::Item(indent) | ContainerRole::Definition(indent) => {
            ContainerRolePlan::Indent(indent)
        }
        ContainerRole::Footnote if options.has_gfm_footnotes() => ContainerRolePlan::Indent(4),
        _ => ContainerRolePlan::Pass, // list/leaf与旧footnote本身不消费prefix，但仍计真实spine层数。
    }
}

fn scan_container_role(
    role: ContainerRole,
    line_start: &mut LineStart<'_>,
    options: Options,
) -> bool {
    match container_role_plan(role, options) {
        ContainerRolePlan::Quote => {
            let save = line_start.clone();
            let _ = line_start.scan_space(3);
            if !line_start.scan_blockquote_marker() {
                *line_start = save;
                return false;
            }
        }
        ContainerRolePlan::Indent(indent) => {
            let save = line_start.clone();
            if !line_start.scan_space(indent) && !line_start.is_at_eol() {
                *line_start = save;
                return false;
            }
        }
        ContainerRolePlan::Pass => (),
    }
    true
}

/// Checks whether we should break a paragraph on the given input.
///
/// Use `FirstPass::scan_paragraph_interrupt` in any context that allows
/// tables to interrupt the paragraph.
pub(crate) fn scan_paragraph_interrupt_no_table(
    bytes: &[u8],
    current_container: bool,
    has_footnote: bool,
    definition_list: bool,
    tree: &impl ParagraphContext,
    tree_position: usize,
) -> bool {
    scan_eol(bytes).is_some()
        || scan_hrule(bytes).is_ok()
        || scan_atx_heading(bytes).is_some()
        || scan_code_fence(bytes).is_some()
        || scan_blockquote_start(bytes).is_some()
        || scan_listitem(bytes).map_or(false, |(ix, delim, index, _)| {
            ! current_container ||
            tree.is_in_table() ||
            // we don't allow interruption by either empty lists or
            // numbered lists starting at an index other than 1
            (delim == b'*' || delim == b'-' || delim == b'+' || index == 1)
                && (scan_blank_line(&bytes[ix..]).is_none())
        })
        || bytes.starts_with(b"<")
            && (get_html_end_tag(&bytes[1..]).is_some() || starts_html_block_type_6(&bytes[1..]))
        || definition_list
            && ((current_container && tree.parent_is_paragraph())
                || tree.spine_entry_is_definition(tree_position))
            && bytes.starts_with(b":")
        || (has_footnote
            && bytes.starts_with(b"[^")
            && scan_link_label_rest(
                std::str::from_utf8(&bytes[2..]).unwrap(),
                &|_| None,
                tree.is_in_table(),
            )
            .map_or(false, |(len, _)| bytes.get(2 + len) == Some(&b':')))
}

/// Assumes `text_bytes` is preceded by `<`.
pub(crate) fn get_html_end_tag(text_bytes: &[u8]) -> Option<&'static str> {
    static BEGIN_TAGS: &[&[u8]; 4] = &[b"pre", b"style", b"script", b"textarea"];
    static ST_BEGIN_TAGS: &[&[u8]; 3] = &[b"!--", b"?", b"![CDATA["];

    for (beg_tag, end_tag) in BEGIN_TAGS
        .iter()
        .zip(["</pre>", "</style>", "</script>", "</textarea>"].iter())
    {
        let tag_len = beg_tag.len();

        if text_bytes.len() < tag_len {
            // begin tags are increasing in size
            break;
        }

        if !text_bytes[..tag_len].eq_ignore_ascii_case(beg_tag) {
            continue;
        }

        // Must either be the end of the line...
        if text_bytes.len() == tag_len {
            return Some(end_tag);
        }

        // ...or be followed by whitespace, newline, or '>'.
        let s = text_bytes[tag_len];
        if is_ascii_whitespace(s) || s == b'>' {
            return Some(end_tag);
        }
    }

    for (beg_tag, end_tag) in ST_BEGIN_TAGS.iter().zip(["-->", "?>", "]]>"].iter()) {
        if text_bytes.starts_with(beg_tag) {
            return Some(end_tag);
        }
    }

    if text_bytes.len() > 1 && text_bytes[0] == b'!' && text_bytes[1].is_ascii_alphabetic() {
        Some(">")
    } else {
        None
    }
}

/// 原paragraph的heavy-table两行判定；调用者必须提供真实完整header/delimiter上下文，不能以frame EOF替代。
pub(crate) fn scan_heavy_table_interrupt(
    bytes: &[u8],
    tree: &impl ContainerSpine,
    spine_len: usize,
    options: Options,
) -> bool {
    // pulldown-cmark allows heavy tables, that have a `|` on the header row,
    // to interrupt paragraphs.
    //
    // ```markdown
    // This is a table
    // | a | b | c |
    // |---|---|---|
    // | d | e | f |
    //
    // This is not a table
    //  a | b | c
    // ---|---|---
    //  d | e | f
    // ```
    if !options.contains(Options::ENABLE_TABLES) || !bytes.starts_with(b"|") {
        return false;
    }

    // Checking if something's a valid table or not requires looking at two lines.
    // First line, count unescaped pipes.
    let mut pipes = 0;
    let mut next_line_ix = 0;
    let mut bsesc = false;
    let mut last_pipe_ix = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'\\' => {
                bsesc = true;
                continue;
            }
            b'|' if !bsesc => {
                pipes += 1;
                last_pipe_ix = i;
            }
            b'\r' | b'\n' => {
                next_line_ix = i + scan_eol(&bytes[i..]).unwrap();
                break;
            }
            _ => {}
        }
        bsesc = false;
    }

    // scan_eol can't return 0, so this can't be zero
    if next_line_ix == 0 {
        return false;
    }

    // Scan the table head. The part that looks like:
    //
    //     |---|---|---|
    //
    // Also scan any containing items, since it's on its own line, and
    // might be nested inside a block quote or something
    //
    //     > Table: First
    //     > | first col | second col |
    //     > |-----------|------------|
    //     ^
    //     | need to skip over the `>` when checking for the table
    let mut line_start = LineStart::new(&bytes[next_line_ix..]);
    if scan_containers(tree, &mut line_start, options) != spine_len {
        return false;
    }
    let table_head_ix = next_line_ix + line_start.bytes_scanned();
    let (table_head_bytes, alignment) = scan_table_head(&bytes[table_head_ix..]);

    if table_head_bytes == 0 {
        return false;
    }

    // computing header count from number of pipes
    let header_count = crate::table_scan::count_header_cols(bytes, pipes, 0, last_pipe_ix);

    // make sure they match the number of columns we find in separator line
    alignment.len() == header_count
}
