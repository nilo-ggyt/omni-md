//! The first pass resolves all block structure, generating an AST. Within a block, items
//! are in a linear chain with potential inline markup identified.

use crate::block_context::{
    scan_container_opening, scan_paragraph_interrupt_no_table, ContainerOpening,
};
use crate::leaf_scan::{html_line_contains_end_tag, non_metadata_leaf_start, LeafStart};
use crate::math_context::MathContext;
use crate::table_scan::{
    backslash_transition, count_header_cols, is_table_pipe, BackslashTransition,
    MAX_AUTOCOMPLETED_CELLS,
};

use std::cmp::max;
use std::ops::Range;

use crate::parse::{
    scan_containers, Allocations, FootnoteDef, HeadingAttributes, Item, ItemBody, LinkDef,
    LINK_MAX_NESTED_PARENS,
};
use crate::strings::CowStr;
use crate::tree::{Tree, TreeIndex};
use crate::Options;
use crate::{
    linklabel::{scan_link_label_rest, LinkLabel},
    HeadingLevel,
};
use crate::{scanners::*, MetadataBlockKind};

#[path = "global_frontier/leaf_block/line_facts.rs"]
pub(crate) mod leaf_line_facts;

use unicase::UniCase;

/// Runs the first pass, which resolves the block structure of the document,
/// and returns the resulting tree.
pub(crate) fn run_first_pass(text: &str, options: Options) -> (Tree<Item>, Allocations<'_>) {
    let (tree, allocations, _) = run_first_pass_with_math_seeds(text, options, &[]);
    (tree, allocations)
}

/// 默认路径传空seed；有限窗口只有通过入口校验后才可覆盖真实row边界的数学状态。
pub(crate) fn run_first_pass_with_math_seeds<'a>(
    text: &'a str,
    options: Options,
    #[cfg(feature = "omni-block-frontier")] seeds: &[crate::math_context::TableMathSeed],
    #[cfg(not(feature = "omni-block-frontier"))] _seeds: &[()],
) -> (Tree<Item>, Allocations<'a>, bool) {
    let lookup_table = &create_lut(&options);
    first_pass(
        text,
        options,
        lookup_table,
        #[cfg(feature = "omni-block-frontier")]
        seeds,
        #[cfg(not(feature = "omni-block-frontier"))]
        _seeds,
    )
    .run()
}

#[cfg(feature = "omni-block-frontier")]
#[path = "firstpass/table_cell.rs"]
mod table_cell;
#[cfg(feature = "omni-block-frontier")]
pub(crate) use table_cell::run as run_first_pass_table_cell;

#[cfg(feature = "omni-block-frontier")]
#[path = "firstpass/atx_leaf.rs"]
mod atx_leaf;
#[cfg(feature = "omni-block-frontier")]
pub(crate) use atx_leaf::run as run_first_pass_atx_leaf;

#[cfg(feature = "omni-block-frontier")]
#[path = "firstpass/inline_block.rs"]
mod inline_block;
#[cfg(feature = "omni-block-frontier")]
pub(crate) use inline_block::run as run_first_pass_inline_block;

/// 所有真实 firstpass 入口共享同一初始化；cell 只改变原 lexer 的入口，不建立另一份解析图。
fn first_pass<'a, 'b>(
    text: &'a str,
    options: Options,
    lookup_table: &'b LookupTable,
    #[cfg(feature = "omni-block-frontier")] seeds: &'b [crate::math_context::TableMathSeed],
    #[cfg(not(feature = "omni-block-frontier"))] _seeds: &'b [()],
) -> FirstPass<'a, 'b> {
    // This is a very naive heuristic for the number of nodes
    // we'll need.
    let start_capacity = max(128, text.len() / 32);
    FirstPass {
        text,
        tree: Tree::with_capacity(start_capacity),
        begin_list_item: None,
        last_line_blank: false,
        allocs: Allocations::new(),
        options,
        lookup_table,
        math_context: MathContext::default(),
        #[cfg(feature = "omni-block-frontier")]
        table_math_seeds: seeds,
        #[cfg(feature = "omni-block-frontier")]
        table_math_seed_used: 0,
        #[cfg(feature = "omni-block-frontier")]
        table_math_seed_invalid: false,
        #[cfg(feature = "omni-block-frontier")]
        paragraph_context: None,
        #[cfg(feature = "omni-block-frontier")]
        paragraph_seed_error: None,
        #[cfg(all(test, feature = "omni-block-frontier"))]
        captured_tables: Vec::new(),
        #[cfg(all(test, feature = "omni-block-frontier"))]
        captured_math_rows: Vec::new(),
        #[cfg(all(test, feature = "omni-block-frontier"))]
        captured_inline_blocks: Vec::new(),
        #[cfg(all(test, feature = "omni-block-frontier"))]
        captured_leaf_blocks: Vec::new(),
    }
}

/// State for the first parsing pass.
struct FirstPass<'a, 'b> {
    text: &'a str,
    tree: Tree<Item>,
    begin_list_item: Option<usize>,
    last_line_blank: bool,
    allocs: Allocations<'a>,
    options: Options,
    lookup_table: &'b LookupTable,
    /// 同原lexer共享有限brace状态；不在行/窗口切分时重置。
    math_context: MathContext,
    #[cfg(feature = "omni-block-frontier")]
    table_math_seeds: &'b [crate::math_context::TableMathSeed],
    #[cfg(feature = "omni-block-frontier")]
    table_math_seed_used: usize,
    #[cfg(feature = "omni-block-frontier")]
    table_math_seed_invalid: bool,
    #[cfg(feature = "omni-block-frontier")]
    paragraph_context: Option<&'b crate::global_frontier::InlineParagraphContext>,
    #[cfg(feature = "omni-block-frontier")]
    paragraph_seed_error: Option<crate::global_frontier::GlobalFrontierError>,
    #[cfg(all(test, feature = "omni-block-frontier"))]
    captured_tables: Vec<crate::block_frontier::tests::CapturedTableSeed>,
    #[cfg(all(test, feature = "omni-block-frontier"))]
    captured_math_rows: Vec<(usize, MathContext)>,
    #[cfg(all(test, feature = "omni-block-frontier"))]
    captured_inline_blocks: Vec<(
        crate::global_frontier::InlineBlockKind,
        Range<usize>,
        Range<usize>,
    )>,
    #[cfg(all(test, feature = "omni-block-frontier"))]
    captured_leaf_blocks: Vec<(crate::global_frontier::LeafBlockKind, Range<usize>, usize)>,
}

impl<'a, 'b> FirstPass<'a, 'b> {
    fn run(mut self) -> (Tree<Item>, Allocations<'a>, bool) {
        let mut ix = 0;
        while ix < self.text.len() {
            ix = self.parse_block(ix);
        }
        while self.tree.spine_len() > 0 {
            self.pop(ix);
        }
        #[cfg(feature = "omni-block-frontier")]
        let complete = !self.table_math_seed_invalid
            && self.table_math_seed_used == self.table_math_seeds.len();
        #[cfg(not(feature = "omni-block-frontier"))]
        let complete = true;
        (self.tree, self.allocs, complete)
    }

    /// Returns offset after block.
    fn parse_block(&mut self, mut start_ix: usize) -> usize {
        let bytes = self.text.as_bytes();
        let mut line_start = LineStart::new(&bytes[start_ix..]);

        // math spans and their braces are tracked only within a single block
        self.math_context.reset();

        let i = scan_containers(&self.tree, &mut line_start, self.options);
        for _ in i..self.tree.spine_len() {
            self.pop(start_ix);
        }

        if self.options.contains(Options::ENABLE_OLD_FOOTNOTES) {
            // finish footnote if it's still open and was preceded by blank line
            if let Some(node_ix) = self.tree.peek_up() {
                if let ItemBody::FootnoteDefinition(..) = self.tree[node_ix].item.body {
                    if self.last_line_blank {
                        self.pop(start_ix);
                    }
                }
            }
        }

        // Process new containers
        loop {
            let save = line_start.clone();
            let outer_indent = line_start.scan_space_upto(4);
            if outer_indent >= 4 {
                line_start = save;
                break;
            }
            if self.options.contains(Options::ENABLE_FOOTNOTES) {
                // Footnote definitions
                let container_start = start_ix + line_start.bytes_scanned();
                if let Some(bytecount) = self.parse_footnote(container_start) {
                    start_ix = container_start + bytecount;
                    if self.options.contains(Options::ENABLE_OLD_FOOTNOTES) {
                        // gfm footnotes need indented, but old footnotes don't
                        // handle this, old footnotes treat the next line as part of the current line
                        start_ix += scan_blank_line(&bytes[start_ix..]).unwrap_or(0);
                    }
                    line_start = LineStart::new(&bytes[start_ix..]);
                    continue;
                }
            }
            let container_start = start_ix + line_start.bytes_scanned();
            let opening =
                scan_container_opening(&mut line_start, outer_indent, self.options, || {
                    self.tree.cur().is_some_and(|cur| {
                        matches!(
                            self.tree[cur].item.body,
                            ItemBody::Paragraph
                                | ItemBody::TightParagraph
                                | ItemBody::MaybeDefinitionListTitle
                                | ItemBody::DefinitionListDefinition(_)
                        )
                    })
                });
            if let ContainerOpening::List {
                marker: ch,
                index,
                indent,
            } = opening
            {
                let after_marker_index = start_ix + line_start.bytes_scanned();
                self.continue_list(container_start - outer_indent, ch, index);
                self.tree.append(Item {
                    start: container_start - outer_indent,
                    end: after_marker_index, // will get updated later if item not empty
                    body: ItemBody::ListItem(indent),
                });
                self.tree.push();
                if let Some(n) = scan_blank_line(&bytes[after_marker_index..]) {
                    self.begin_list_item = Some(after_marker_index + n);
                    return after_marker_index + n;
                }
                if self.options.contains(Options::ENABLE_TASKLISTS) {
                    let task_list_marker =
                        line_start.scan_task_list_marker().map(|is_checked| Item {
                            start: after_marker_index,
                            end: start_ix + line_start.bytes_scanned(),
                            body: ItemBody::TaskListMarker(is_checked),
                        });
                    if let Some(task_list_marker) = task_list_marker {
                        if let Some(n) = scan_blank_line(&bytes[task_list_marker.end..]) {
                            self.tree.append(task_list_marker);
                            self.begin_list_item = Some(task_list_marker.end + n);
                            return task_list_marker.end + n;
                        } else {
                            line_start.scan_all_space();
                            let ix = start_ix + line_start.bytes_scanned();
                            return self.parse_paragraph(ix, Some(task_list_marker));
                        }
                    }
                }
            } else if let ContainerOpening::Definition { indent } = opening {
                // eligibility已由同一共享决策读取；这里只取得原节点和child以执行原AST转换。
                let cur = self.tree.cur().unwrap();
                let child = self.tree[cur].child;
                let item = &mut self.tree[cur].item;
                match item.body {
                    ItemBody::Paragraph | ItemBody::TightParagraph => {
                        item.body = ItemBody::DefinitionList(true);
                        let Item { start, end, .. } = *item;
                        let list_idx = self.tree.cur().unwrap();
                        let title_idx = self.tree.create_node(Item {
                            start,
                            end, // will get updated later if item not empty
                            body: ItemBody::DefinitionListTitle,
                        });
                        self.tree[title_idx].child = child;
                        self.tree[list_idx].child = Some(title_idx);
                        self.tree.push();
                    }
                    ItemBody::MaybeDefinitionListTitle => {
                        item.body = ItemBody::DefinitionListTitle;
                    }
                    ItemBody::DefinitionListDefinition(_) => {}
                    _ => unreachable!(),
                }
                let after_marker_index = start_ix + line_start.bytes_scanned();
                self.tree.append(Item {
                    start: container_start - outer_indent,
                    end: after_marker_index, // will get updated later if item not empty
                    body: ItemBody::DefinitionListDefinition(indent),
                });
                if let Some(ItemBody::DefinitionList(ref mut is_tight)) =
                    self.tree.peek_up().map(|cur| &mut self.tree[cur].item.body)
                {
                    if self.last_line_blank {
                        *is_tight = false;
                        self.last_line_blank = false;
                    }
                }
                self.tree.push();
                if let Some(n) = scan_blank_line(&bytes[after_marker_index..]) {
                    self.begin_list_item = Some(after_marker_index + n);
                    return after_marker_index + n;
                }
            } else if let ContainerOpening::Quote { kind } = opening {
                self.finish_list(start_ix);
                self.tree.append(Item {
                    start: container_start,
                    end: 0, // will get set later
                    body: ItemBody::BlockQuote(kind),
                });
                self.tree.push();
                if kind.is_some() {
                    // blockquote tag leaves us at the end of the line
                    // we need to scan through all the container syntax for the next line
                    // and break out if we can't re-scan all of them
                    let ix = start_ix + line_start.bytes_scanned();
                    let mut lazy_line_start = LineStart::new(&bytes[ix..]);
                    let tree_position =
                        scan_containers(&self.tree, &mut lazy_line_start, self.options);
                    let current_container = tree_position == self.tree.spine_len();
                    if !lazy_line_start.scan_space(4)
                        && self.scan_paragraph_interrupt(
                            &bytes[ix + lazy_line_start.bytes_scanned()..],
                            current_container,
                            tree_position,
                        )
                    {
                        return ix;
                    } else {
                        // blockquote tags act as if they were nested in a paragraph
                        // so you can lazily continue the imaginary paragraph off of them
                        line_start = lazy_line_start;
                        line_start.scan_all_space();
                        start_ix = ix;
                        break;
                    }
                }
            } else {
                line_start = save;
                break;
            }
        }

        let ix = start_ix + line_start.bytes_scanned();

        if let Some(n) = scan_blank_line(&bytes[ix..]) {
            if let Some(node_ix) = self.tree.peek_up() {
                match &mut self.tree[node_ix].item.body {
                    ItemBody::BlockQuote(..) => (),
                    ItemBody::ListItem(indent) | ItemBody::DefinitionListDefinition(indent)
                        if self.begin_list_item.is_some() =>
                    {
                        self.last_line_blank = true;
                        // This is a blank list item.
                        // While the list itself can be continued no matter how many blank lines
                        // there are between this one and the next one, it cannot nest anything
                        // else, so its indentation should not be subtracted from the line start.
                        *indent = 0;
                    }
                    _ => {
                        self.last_line_blank = true;
                    }
                }
            } else {
                self.last_line_blank = true;
            }
            return ix + n;
        }

        // Save `remaining_space` here to avoid needing to backtrack `line_start` for HTML blocks
        let remaining_space = line_start.remaining_space();

        let indent = line_start.scan_space_upto(4);
        if indent == 4 {
            self.finish_list(start_ix);
            let ix = start_ix + line_start.bytes_scanned();
            let remaining_space = line_start.remaining_space();
            return self.parse_indented_code_block(ix, remaining_space);
        }

        let ix = start_ix + line_start.bytes_scanned();

        // metadata blocks cannot be indented
        if indent == 0 {
            if let Some((_n, metadata_block_ch)) = scan_metadata_block(
                &bytes[ix..],
                self.options
                    .contains(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS),
                self.options
                    .contains(Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS),
            ) {
                self.finish_list(start_ix);
                return self.parse_metadata_block(ix, metadata_block_ch);
            }
        }

        match non_metadata_leaf_start(&bytes[ix..]) {
            LeafStart::HtmlTerminated(html_end_tag) => {
                self.finish_list(start_ix);
                return self.parse_html_block_type_1_to_5(
                    ix,
                    html_end_tag,
                    remaining_space,
                    indent,
                );
            }
            LeafStart::HtmlBlankTerminated => {
                self.finish_list(start_ix);
                return self.parse_html_block_type_6_or_7(ix, remaining_space, indent);
            }
            LeafStart::Rule(n) => {
                self.finish_list(start_ix);
                return self.parse_hrule(n, ix);
            }
            LeafStart::Heading(atx_size) => {
                self.finish_list(start_ix);
                return self.parse_atx_heading(ix, atx_size);
            }
            LeafStart::Fence { marker, count } => {
                self.finish_list(start_ix);
                return self.parse_fenced_code_block(ix, indent, marker, count);
            }
            // refdef/paragraph原动作继续在下方执行；共享函数不提前提交未决多行定义。
            LeafStart::DefinitionOrParagraph => {}
        }

        // parse refdef
        while let Some((bytecount, label, link_def)) =
            self.parse_refdef_total(start_ix + line_start.bytes_scanned())
        {
            self.allocs.refdefs.0.entry(label).or_insert(link_def);
            let container_start = start_ix + line_start.bytes_scanned();
            let mut ix = container_start + bytecount;
            // Refdefs act as if they were contained within a paragraph, for purposes of lazy
            // continuations. For example:
            //
            // ```
            // > [foo]: http://example.com
            // bar
            // ```
            //
            // is equivalent to
            //
            // ```
            // > bar
            //
            // [foo]: http://example.com
            // ```
            if let Some(nl) = scan_blank_line(&bytes[ix..]) {
                ix += nl;
            } else {
                self.finish_list(start_ix);
                return ix;
            }
            if let Some(lazy_line_start) = self.scan_next_line_or_lazy_continuation(&bytes[ix..]) {
                line_start = lazy_line_start;
                start_ix = ix;
            } else {
                self.finish_list(start_ix);
                return ix;
            }
        }

        let ix = start_ix + line_start.bytes_scanned();

        self.parse_paragraph(ix, None)
    }

    /// footnote definitions and GFM quote markers can be "interrupted"
    /// like paragraphs, but otherwise can't have other blocks after them.
    ///
    /// Call this at the end of the line to parse that. If it succeeeds,
    /// this returns the LineStart for the new line.
    fn scan_next_line_or_lazy_continuation<'input>(
        &mut self,
        bytes: &'input [u8],
    ) -> Option<LineStart<'input>> {
        let mut line_start = LineStart::new(bytes);
        let tree_position = scan_containers(&self.tree, &mut line_start, self.options);
        let current_container = tree_position == self.tree.spine_len();
        if !line_start.scan_space(4)
            && self.scan_paragraph_interrupt(
                &bytes[line_start.bytes_scanned()..],
                current_container,
                tree_position,
            )
        {
            None
        } else {
            line_start.scan_all_space();
            Some(line_start)
        }
    }

    /// Returns the offset of the first line after the table.
    /// Assumptions: current focus is a table element and the table header
    /// matches the separator line (same number of columns).
    fn parse_table(
        &mut self,
        table_cols: usize,
        head_start: usize,
        body_start: usize,
    ) -> Option<usize> {
        #[cfg(all(test, feature = "omni-block-frontier"))]
        {
            // 纯测从真实firstpass表入口捕获spine；不暴露任意调用者构造verified种子的API。
            use crate::block_context::ContainerSpine;
            self.captured_tables
                .push(crate::block_frontier::tests::CapturedTableSeed {
                    roles: {
                        let mut roles = Vec::new();
                        self.tree.visit_roles(|role| {
                            roles.push(role);
                            true
                        });
                        roles
                    },
                    columns: table_cols,
                    header_byte_start: head_start,
                    body_start,
                });
        }
        // filled empty cells are limited to protect against quadratic growth
        // https://github.com/raphlinus/pulldown-cmark/issues/832
        let mut missing_empty_cells = 0;
        // parse header. this shouldn't fail because we made sure the table header is ok
        let (_sep_start, thead_ix) =
            self.parse_table_row_inner(head_start, table_cols, &mut missing_empty_cells)?;
        self.tree[thead_ix].item.body = ItemBody::TableHead;

        // parse body
        let mut ix = body_start;
        while let Some((next_ix, _row_ix)) =
            self.parse_table_row(ix, table_cols, &mut missing_empty_cells)
        {
            ix = next_ix;
        }

        self.pop(ix);
        Some(ix)
    }

    /// Call this when containers are taken care of.
    /// Returns bytes scanned, row_ix
    fn parse_table_row_inner(
        &mut self,
        mut ix: usize,
        row_cells: usize,
        missing_empty_cells: &mut usize,
    ) -> Option<(usize, TreeIndex)> {
        #[cfg(feature = "omni-block-frontier")]
        if let Ok(index) = self
            .table_math_seeds
            .binary_search_by_key(&ix, |seed| seed.row_start())
        {
            // 只覆写当前真实行起点；其余行继续原table内部状态，绝不更改cell切分或树节点。
            if index != self.table_math_seed_used {
                // 发生跳过或重复消费说明seed不是当前真实row入口序列，最终拒绝整份候选。
                self.table_math_seed_invalid = true;
            }
            self.table_math_seed_used += 1;
            self.math_context = self.table_math_seeds[index].context();
        }
        #[cfg(all(test, feature = "omni-block-frontier"))]
        self.captured_math_rows.push((ix, self.math_context));
        // Limit to prevent a malicious input from causing a denial of service.

        let bytes = self.text.as_bytes();
        let mut cells = 0;
        let mut final_cell_ix = None;

        let old_cur = self.tree.cur();
        let row_ix = self.tree.append(Item {
            start: ix,
            end: 0, // set at end of this function
            body: ItemBody::TableRow,
        });
        self.tree.push();

        loop {
            ix += scan_ch(&bytes[ix..], b'|');
            let start_ix = ix;
            ix += scan_whitespace_no_nl(&bytes[ix..]);

            if let Some(eol_bytes) = scan_eol(&bytes[ix..]) {
                ix += eol_bytes;
                break;
            }

            let cell_ix = self.tree.append(Item {
                start: start_ix,
                end: ix,
                body: ItemBody::TableCell,
            });
            self.tree.push();
            let (next_ix, _brk) = self.parse_line(ix, None, TableParseMode::Active);

            self.tree[cell_ix].item.end = next_ix;
            self.tree.pop();

            ix = next_ix;
            cells += 1;

            if cells == row_cells {
                final_cell_ix = Some(cell_ix);
            }
        }

        if let (Some(cur), 0) = (old_cur, cells) {
            self.pop(ix);
            self.tree[cur].next = None;
            return None;
        }

        // fill empty cells if needed
        // note: this is where GFM and commonmark-extra diverge. we follow
        // GFM here
        for _ in cells..row_cells {
            if *missing_empty_cells >= MAX_AUTOCOMPLETED_CELLS {
                return None;
            }
            *missing_empty_cells += 1;
            self.tree.append(Item {
                start: ix,
                end: ix,
                body: ItemBody::TableCell,
            });
        }

        // drop excess cells
        if let Some(cell_ix) = final_cell_ix {
            self.tree[cell_ix].next = None;
        }

        self.pop(ix);

        Some((ix, row_ix))
    }

    /// Returns first offset after the row and the tree index of the row.
    fn parse_table_row(
        &mut self,
        mut ix: usize,
        row_cells: usize,
        missing_empty_cells: &mut usize,
    ) -> Option<(usize, TreeIndex)> {
        let bytes = self.text.as_bytes();
        let mut line_start = LineStart::new(&bytes[ix..]);
        let tree_position = scan_containers(&self.tree, &mut line_start, self.options);
        let current_container = tree_position == self.tree.spine_len();
        if !current_container {
            return None;
        }
        line_start.scan_all_space();
        ix += line_start.bytes_scanned();
        if scan_paragraph_interrupt_no_table(
            &bytes[ix..],
            current_container,
            self.options.contains(Options::ENABLE_FOOTNOTES),
            self.options.contains(Options::ENABLE_DEFINITION_LIST),
            &self.tree,
            tree_position,
        ) {
            return None;
        }

        let (ix, row_ix) = self.parse_table_row_inner(ix, row_cells, missing_empty_cells)?;
        Some((ix, row_ix))
    }

    /// Returns offset of line start after paragraph.
    fn parse_paragraph(&mut self, start_ix: usize, tasklist_marker: Option<Item>) -> usize {
        let body = if let Some(ItemBody::DefinitionList(_)) =
            self.tree.peek_up().map(|idx| self.tree[idx].item.body)
        {
            if self.tree.cur().map_or(true, |idx| {
                matches!(
                    &self.tree[idx].item.body,
                    ItemBody::DefinitionListDefinition(..)
                )
            }) {
                // blank lines between the previous definition and this one don't count
                self.last_line_blank = false;
                ItemBody::MaybeDefinitionListTitle
            } else {
                self.finish_list(start_ix);
                ItemBody::Paragraph
            }
        } else {
            self.finish_list(start_ix);
            ItemBody::Paragraph
        };
        let node_ix = self.tree.append(Item {
            start: start_ix,
            end: 0, // will get set later
            body,
        });
        self.tree.push();

        if let Some(item) = tasklist_marker {
            self.tree.append(item);
        }

        let bytes = self.text.as_bytes();
        let mut ix = start_ix;
        loop {
            let scan_tables = self.options.contains(Options::ENABLE_TABLES) && ix == start_ix;
            #[cfg(feature = "omni-block-frontier")]
            let scan_tables = scan_tables && self.paragraph_context.is_none();
            let scan_mode = if scan_tables {
                TableParseMode::Scan
            } else {
                TableParseMode::Disabled
            };
            let (next_ix, brk) = self.parse_line(ix, None, scan_mode);

            // break out when we find a table
            if let Some(Item {
                body: ItemBody::Table(alignment_ix),
                ..
            }) = brk
            {
                let table_cols = self.allocs[alignment_ix].len();
                self.tree[node_ix].item.body = ItemBody::Table(alignment_ix);
                // this clears out any stuff we may have appended - but there may
                // be a cleaner way
                self.tree[node_ix].child = None;
                self.tree.pop();
                if body == ItemBody::MaybeDefinitionListTitle {
                    self.finish_list(ix);
                }
                self.tree.push();
                if let Some(ix) = self.parse_table(table_cols, ix, next_ix) {
                    return ix;
                }
            }

            ix = next_ix;
            #[cfg(feature = "omni-block-frontier")]
            if self.paragraph_context.is_some() && ix == bytes.len() {
                if let Some(Item {
                    start,
                    body: ItemBody::HardBreak(true),
                    ..
                }) = brk
                {
                    if bytes[start] == b'\\' {
                        self.tree.append_text(start, start + 1, false);
                    }
                } // 与原终行 is_at_eol 路径相同：未被提交为行内 break 的末尾反斜杠仍是字面文本。
                break; // 原闭合 seed 的末端已经由 Global 证明，不把有限 frame 尾或邻块当作虚拟 EOF。
            }
            let Some((mut line_start, tree_position, current_container)) =
                self.paragraph_line_start(ix, bytes.len())
            else {
                break;
            };
            let trailing_backslash_pos = match brk {
                Some(Item {
                    start,
                    body: ItemBody::HardBreak(true),
                    ..
                }) if bytes[start] == b'\\' => Some(start),
                _ => None,
            };
            if !line_start.scan_space(4) {
                let ix_new = ix + line_start.bytes_scanned();
                if current_container {
                    if let Some(ix_setext) =
                        self.parse_setext_heading(ix_new, node_ix, trailing_backslash_pos.is_some())
                    {
                        if let Some(pos) = trailing_backslash_pos {
                            self.tree.append_text(pos, pos + 1, false);
                        }
                        #[cfg(all(test, feature = "omni-block-frontier"))]
                        if let ItemBody::Heading(level, _) = self.tree[node_ix].item.body {
                            // 原firstpass自身已完成Setext改型；oracle不另识别输入marker或重跑本地grammar。
                            self.captured_inline_blocks.push((
                                crate::global_frontier::InlineBlockKind::SetextHeading(level),
                                start_ix..ix_setext,
                                start_ix..ix,
                            ));
                        }
                        self.pop(ix_setext);
                        if body == ItemBody::MaybeDefinitionListTitle {
                            self.finish_list(ix);
                        }
                        return ix_setext;
                    }
                }
                // first check for non-empty lists, then for other interrupts
                let suffix = &bytes[ix_new..];
                if self.scan_paragraph_interrupt(suffix, current_container, tree_position) {
                    if let Some(pos) = trailing_backslash_pos {
                        self.tree.append_text(pos, pos + 1, false);
                    }
                    break;
                }
            }
            line_start.scan_all_space();
            if line_start.is_at_eol() {
                if let Some(pos) = trailing_backslash_pos {
                    self.tree.append_text(pos, pos + 1, false);
                }
                break;
            }
            ix = next_ix + line_start.bytes_scanned();
            if let Some(item) = brk {
                self.tree.append(item);
            }
        }

        #[cfg(all(test, feature = "omni-block-frontier"))]
        if matches!(body, ItemBody::Paragraph | ItemBody::TightParagraph) {
            // 捕获发生在原parse_paragraph完成点，tight-list后处理不会改变真实源范围。
            self.captured_inline_blocks.push((
                crate::global_frontier::InlineBlockKind::Paragraph,
                start_ix..ix,
                start_ix..ix,
            ));
        }
        self.pop(ix);
        ix
    }

    /// Returns end ix of setext_heading on success.
    fn parse_setext_heading(
        &mut self,
        ix: usize,
        node_ix: TreeIndex,
        has_trailing_content: bool,
    ) -> Option<usize> {
        let bytes = self.text.as_bytes();
        let (n, level) = scan_setext_heading(&bytes[ix..])?;
        let mut attrs = None;

        if let Some(cur_ix) = self.tree.cur() {
            let parent_ix = self.tree.peek_up().unwrap();
            let header_start = self.tree[parent_ix].item.start;
            // Note that `self.tree[parent_ix].item.end` might be zero at this point.
            // Use the end position of the current node (i.e. the last known child
            // of the parent) instead.
            let header_end = self.tree[cur_ix].item.end;

            // extract the trailing attribute block
            let (content_end, attrs_) =
                self.extract_and_parse_heading_attribute_block(header_start, header_end);
            attrs = attrs_;

            // strip trailing whitespace
            let new_end = if has_trailing_content {
                content_end
            } else {
                let mut last_line_start = header_start;
                if attrs.is_some() {
                    loop {
                        let next_line_start =
                            last_line_start + scan_nextline(&bytes[last_line_start..content_end]);
                        if next_line_start >= content_end {
                            break;
                        }
                        let (line_start, _, current_container) =
                            self.paragraph_line_start(next_line_start, content_end)?;
                        if !current_container {
                            break;
                        }
                        last_line_start = next_line_start + line_start.bytes_scanned();
                    }
                }
                let trailing_ws = scan_rev_while(
                    &bytes[last_line_start..content_end],
                    is_ascii_whitespace_no_nl,
                );
                content_end - trailing_ws
            };

            if attrs.is_some() {
                // remove trailing block attributes
                self.tree.truncate_siblings(new_end);
            }

            if let Some(cur_ix) = self.tree.cur() {
                self.tree[cur_ix].item.end = new_end;
            }
        }

        self.tree[node_ix].item.body = ItemBody::Heading(
            level,
            attrs.map(|attrs| self.allocs.allocate_heading(attrs)),
        );

        Some(ix + n)
    }

    /// Parse a line of input, appending text and items to tree.
    ///
    /// Returns: index after line and an item representing the break.
    fn parse_line(
        &mut self,
        start: usize,
        end: Option<usize>,
        mode: TableParseMode,
    ) -> (usize, Option<Item>) {
        let bytes = self.text.as_bytes();
        let bytes = match end {
            Some(end) => &bytes[..end],
            None => bytes,
        };
        let bytes_len = bytes.len();
        let mut pipes = 0;
        let mut last_pipe_ix = start;
        let mut begin_text = start;
        let mut backslash_escaped = false;

        let (final_ix, brk) = iterate_special_bytes(self.lookup_table, bytes, start, |ix, byte| {
            match byte {
                b'\n' | b'\r' => {
                    if let TableParseMode::Active = mode {
                        return LoopInstruction::BreakAtWith(ix, None);
                    }

                    let mut i = ix;
                    let eol_bytes = scan_eol(&bytes[ix..]).unwrap();

                    let end_ix = ix + eol_bytes;
                    let trailing_backslashes = scan_rev_while(&bytes[..ix], |b| b == b'\\');
                    if trailing_backslashes % 2 == 1 && end_ix < bytes_len {
                        i -= 1;
                        self.tree.append_text(begin_text, i, backslash_escaped);
                        backslash_escaped = false;
                        return LoopInstruction::BreakAtWith(
                            end_ix,
                            Some(Item {
                                start: i,
                                end: end_ix,
                                body: ItemBody::HardBreak(true),
                            }),
                        );
                    }

                    if mode == TableParseMode::Scan && pipes > 0 {
                        // check if we may be parsing a table
                        let next_line_ix = ix + eol_bytes;
                        let mut line_start = LineStart::new(&bytes[next_line_ix..]);
                        if scan_containers(&self.tree, &mut line_start, self.options)
                            == self.tree.spine_len()
                        {
                            let table_head_ix = next_line_ix + line_start.bytes_scanned();
                            let (table_head_bytes, alignment) =
                                scan_table_head(&bytes[table_head_ix..]);

                            if table_head_bytes > 0 {
                                // computing header count from number of pipes
                                let header_count =
                                    count_header_cols(bytes, pipes, start, last_pipe_ix);

                                // make sure they match the number of columns we find in separator line
                                if alignment.len() == header_count {
                                    let alignment_ix = self.allocs.allocate_alignment(alignment);
                                    let end_ix = table_head_ix + table_head_bytes;
                                    return LoopInstruction::BreakAtWith(
                                        end_ix,
                                        Some(Item {
                                            start: i,
                                            end: end_ix, // must update later
                                            body: ItemBody::Table(alignment_ix),
                                        }),
                                    );
                                }
                            }
                        }
                    }

                    let trailing_whitespace =
                        scan_rev_while(&bytes[..ix], is_ascii_whitespace_no_nl);
                    if trailing_whitespace >= 2 {
                        i -= trailing_whitespace;
                        self.tree.append_text(begin_text, i, backslash_escaped);
                        backslash_escaped = false;
                        return LoopInstruction::BreakAtWith(
                            end_ix,
                            Some(Item {
                                start: i,
                                end: end_ix,
                                body: ItemBody::HardBreak(false),
                            }),
                        );
                    }

                    self.tree
                        .append_text(begin_text, ix - trailing_whitespace, backslash_escaped);
                    backslash_escaped = false;

                    LoopInstruction::BreakAtWith(
                        end_ix,
                        Some(Item {
                            start: i,
                            end: end_ix,
                            body: ItemBody::SoftBreak,
                        }),
                    )
                }
                b'\\' => {
                    let transition =
                        backslash_transition(bytes, ix, TableParseMode::Active == mode);
                    if transition != BackslashTransition::Literal {
                        // 保留原公共前置动作：所有有效punctuation转义都先提交之前的普通文本。
                        self.tree.append_text(begin_text, ix, backslash_escaped);
                    }
                    match transition {
                        BackslashTransition::CodeRun(count) => {
                            self.tree.append(Item {
                                start: ix + 1,
                                end: ix + count + 1,
                                body: ItemBody::MaybeCode(count, true),
                            });
                            begin_text = ix + 1 + count;
                            backslash_escaped = false;
                            LoopInstruction::ContinueAndSkip(count)
                        }
                        BackslashTransition::Pipe => {
                            begin_text = ix + 1;
                            backslash_escaped = false;
                            LoopInstruction::ContinueAndSkip(1)
                        }
                        BackslashTransition::DoubleBackslashPipe => {
                            begin_text = ix + 2;
                            backslash_escaped = true;
                            LoopInstruction::ContinueAndSkip(2)
                        }
                        BackslashTransition::Punctuation => {
                            begin_text = ix + 1;
                            backslash_escaped = true;
                            LoopInstruction::ContinueAndSkip(1)
                        }
                        BackslashTransition::Literal => LoopInstruction::ContinueAndSkip(0),
                    }
                }
                c @ b'*' | c @ b'_' | c @ b'~' | c @ b'^' => {
                    let string_suffix = &self.text[ix..];
                    let count = 1 + scan_ch_repeat(&string_suffix.as_bytes()[1..], c);
                    let can_open = delim_run_can_open(
                        &self.text[start..],
                        string_suffix,
                        count,
                        ix - start,
                        mode,
                        self.options,
                    );
                    let can_close = delim_run_can_close(
                        &self.text[start..],
                        string_suffix,
                        count,
                        ix - start,
                        mode,
                        self.options,
                    );
                    let is_valid_seq = (c != b'~' || count <= 2) || (c == b'~' && count == 2);

                    if (can_open || can_close) && is_valid_seq {
                        self.tree.append_text(begin_text, ix, backslash_escaped);
                        backslash_escaped = false;
                        for i in 0..count {
                            self.tree.append(Item {
                                start: ix + i,
                                end: ix + i + 1,
                                body: ItemBody::MaybeEmphasis(count - i, can_open, can_close),
                            });
                        }
                        begin_text = ix + count;
                    }
                    LoopInstruction::ContinueAndSkip(count - 1)
                }
                b'$' => {
                    let byte_suffix = &bytes[ix..];
                    let can_open = !byte_suffix[1..]
                        .first()
                        .copied()
                        .map_or(true, is_ascii_whitespace);
                    let can_close = ix > start
                        && !bytes[..ix]
                            .last()
                            .copied()
                            .map_or(true, is_ascii_whitespace);

                    // 状态标签与分段窗口共享同一原始转移；事件构造及open/close判定保留原顺序。
                    let brace_context = self.math_context.dollar();

                    self.tree.append_text(begin_text, ix, backslash_escaped);
                    self.tree.append(Item {
                        start: ix,
                        end: ix + 1,
                        body: ItemBody::MaybeMath(can_open, can_close, brace_context),
                    });
                    begin_text = ix + 1;
                    LoopInstruction::ContinueAndSkip(0)
                }
                b'{' => {
                    // 原brace状态先更新后继续扫描，不新建AST节点。
                    self.math_context.open();
                    LoopInstruction::ContinueAndSkip(0)
                }
                b'}' => {
                    // 不平衡close同样使用原root标签回绕语义。
                    self.math_context.close();
                    LoopInstruction::ContinueAndSkip(0)
                }
                b'`' => {
                    self.tree.append_text(begin_text, ix, backslash_escaped);
                    backslash_escaped = false;
                    let count = 1 + scan_ch_repeat(&bytes[(ix + 1)..], b'`');
                    self.tree.append(Item {
                        start: ix,
                        end: ix + count,
                        body: ItemBody::MaybeCode(count, false),
                    });
                    begin_text = ix + count;
                    LoopInstruction::ContinueAndSkip(count - 1)
                }
                b'<' if bytes.get(ix + 1) != Some(&b'\\') => {
                    // Note: could detect some non-HTML cases and early escape here, but not
                    // clear that's a win.
                    self.tree.append_text(begin_text, ix, backslash_escaped);
                    backslash_escaped = false;
                    self.tree.append(Item {
                        start: ix,
                        end: ix + 1,
                        body: ItemBody::MaybeHtml,
                    });
                    begin_text = ix + 1;
                    LoopInstruction::ContinueAndSkip(0)
                }
                b'!' => {
                    if ix + 1 < bytes_len && bytes[ix + 1] == b'[' {
                        self.tree.append_text(begin_text, ix, backslash_escaped);
                        backslash_escaped = false;
                        self.tree.append(Item {
                            start: ix,
                            end: ix + 2,
                            body: ItemBody::MaybeImage,
                        });
                        begin_text = ix + 2;
                        LoopInstruction::ContinueAndSkip(1)
                    } else {
                        LoopInstruction::ContinueAndSkip(0)
                    }
                }
                b'[' => {
                    self.tree.append_text(begin_text, ix, backslash_escaped);
                    backslash_escaped = false;
                    self.tree.append(Item {
                        start: ix,
                        end: ix + 1,
                        body: ItemBody::MaybeLinkOpen,
                    });
                    begin_text = ix + 1;
                    LoopInstruction::ContinueAndSkip(0)
                }
                b']' => {
                    self.tree.append_text(begin_text, ix, backslash_escaped);
                    backslash_escaped = false;
                    self.tree.append(Item {
                        start: ix,
                        end: ix + 1,
                        body: ItemBody::MaybeLinkClose(true),
                    });
                    begin_text = ix + 1;
                    LoopInstruction::ContinueAndSkip(0)
                }
                b'&' => match scan_entity(&bytes[ix..]) {
                    (n, Some(value)) => {
                        self.tree.append_text(begin_text, ix, backslash_escaped);
                        backslash_escaped = false;
                        self.tree.append(Item {
                            start: ix,
                            end: ix + n,
                            body: ItemBody::SynthesizeText(self.allocs.allocate_cow(value)),
                        });
                        begin_text = ix + n;
                        LoopInstruction::ContinueAndSkip(n - 1)
                    }
                    _ => LoopInstruction::ContinueAndSkip(0),
                },
                b'|' => {
                    if !is_table_pipe(bytes, ix) {
                        LoopInstruction::ContinueAndSkip(0)
                    } else if let TableParseMode::Active = mode {
                        LoopInstruction::BreakAtWith(ix, None)
                    } else {
                        last_pipe_ix = ix;
                        pipes += 1;
                        LoopInstruction::ContinueAndSkip(0)
                    }
                }
                b'.' => {
                    if ix + 2 < bytes.len() && bytes[ix + 1] == b'.' && bytes[ix + 2] == b'.' {
                        self.tree.append_text(begin_text, ix, backslash_escaped);
                        backslash_escaped = false;
                        self.tree.append(Item {
                            start: ix,
                            end: ix + 3,
                            body: ItemBody::SynthesizeChar('…'),
                        });
                        begin_text = ix + 3;
                        LoopInstruction::ContinueAndSkip(2)
                    } else {
                        LoopInstruction::ContinueAndSkip(0)
                    }
                }
                b'-' => {
                    let count = 1 + scan_ch_repeat(&bytes[(ix + 1)..], b'-');
                    if count == 1 {
                        LoopInstruction::ContinueAndSkip(0)
                    } else {
                        let itembody = if count == 2 {
                            ItemBody::SynthesizeChar('–')
                        } else if count == 3 {
                            ItemBody::SynthesizeChar('—')
                        } else {
                            let (ems, ens) = match count % 6 {
                                0 | 3 => (count / 3, 0),
                                2 | 4 => (0, count / 2),
                                1 => (count / 3 - 1, 2),
                                _ => (count / 3, 1),
                            };
                            // – and — are 3 bytes each in utf8
                            let mut buf = String::with_capacity(3 * (ems + ens));
                            for _ in 0..ems {
                                buf.push('—');
                            }
                            for _ in 0..ens {
                                buf.push('–');
                            }
                            ItemBody::SynthesizeText(self.allocs.allocate_cow(buf.into()))
                        };

                        self.tree.append_text(begin_text, ix, backslash_escaped);
                        backslash_escaped = false;
                        self.tree.append(Item {
                            start: ix,
                            end: ix + count,
                            body: itembody,
                        });
                        begin_text = ix + count;
                        LoopInstruction::ContinueAndSkip(count - 1)
                    }
                }
                c @ b'\'' | c @ b'"' => {
                    let string_suffix = &self.text[ix..];
                    let can_open = delim_run_can_open(
                        &self.text[start..],
                        string_suffix,
                        1,
                        ix - start,
                        mode,
                        self.options,
                    );
                    let can_close = delim_run_can_close(
                        &self.text[start..],
                        string_suffix,
                        1,
                        ix - start,
                        mode,
                        self.options,
                    );

                    self.tree.append_text(begin_text, ix, backslash_escaped);
                    backslash_escaped = false;
                    self.tree.append(Item {
                        start: ix,
                        end: ix + 1,
                        body: ItemBody::MaybeSmartQuote(c, can_open, can_close),
                    });
                    begin_text = ix + 1;

                    LoopInstruction::ContinueAndSkip(0)
                }
                _ => LoopInstruction::ContinueAndSkip(0),
            }
        });

        if brk.is_none() {
            let trailing_whitespace =
                scan_rev_while(&bytes[begin_text..final_ix], is_ascii_whitespace_no_nl);
            // need to close text at eof
            self.tree.append_text(
                begin_text,
                final_ix - trailing_whitespace,
                backslash_escaped,
            );
        }
        (final_ix, brk)
    }

    /// When start_ix is at the beginning of an HTML block of type 1 to 5,
    /// this will find the end of the block, adding the block itself to the
    /// tree and also keeping track of the lines of HTML within the block.
    ///
    /// The html_end_tag is the tag that must be found on a line to end the block.
    fn parse_html_block_type_1_to_5(
        &mut self,
        start_ix: usize,
        html_end_tag: &'static str,
        mut remaining_space: usize,
        mut indent: usize,
    ) -> usize {
        self.tree.append(Item {
            start: start_ix,
            end: 0, // set later
            body: ItemBody::HtmlBlock,
        });
        self.tree.push();

        let bytes = self.text.as_bytes();
        let mut ix = start_ix;
        let end_ix;
        loop {
            let line_start_ix = ix;
            ix += scan_nextline(&bytes[ix..]);
            self.append_html_line(remaining_space.max(indent), line_start_ix, ix);

            let mut line_start = LineStart::new(&bytes[ix..]);
            let n_containers = scan_containers(&self.tree, &mut line_start, self.options);
            if n_containers < self.tree.spine_len() {
                end_ix = ix;
                break;
            }

            if html_line_contains_end_tag(&bytes[line_start_ix..ix], html_end_tag) {
                end_ix = ix;
                break;
            }

            let next_line_ix = ix + line_start.bytes_scanned();
            if next_line_ix == self.text.len() {
                end_ix = next_line_ix;
                break;
            }
            ix = next_line_ix;
            remaining_space = line_start.remaining_space();
            indent = 0;
        }
        self.pop(end_ix);
        #[cfg(all(test, feature = "omni-block-frontier"))]
        self.captured_leaf_blocks.push((
            crate::global_frontier::LeafBlockKind::Html,
            start_ix..end_ix,
            ix,
        ));
        ix
    }

    /// When start_ix is at the beginning of an HTML block of type 6 or 7,
    /// this will consume lines until there is a blank line and keep track of
    /// the HTML within the block.
    fn parse_html_block_type_6_or_7(
        &mut self,
        start_ix: usize,
        mut remaining_space: usize,
        mut indent: usize,
    ) -> usize {
        self.tree.append(Item {
            start: start_ix,
            end: 0, // set later
            body: ItemBody::HtmlBlock,
        });
        self.tree.push();

        let bytes = self.text.as_bytes();
        let mut ix = start_ix;
        let end_ix;
        loop {
            let line_start_ix = ix;
            ix += scan_nextline(&bytes[ix..]);
            self.append_html_line(remaining_space.max(indent), line_start_ix, ix);

            let mut line_start = LineStart::new(&bytes[ix..]);
            let n_containers = scan_containers(&self.tree, &mut line_start, self.options);
            if n_containers < self.tree.spine_len() || line_start.is_at_eol() {
                end_ix = ix;
                break;
            }

            let next_line_ix = ix + line_start.bytes_scanned();
            if next_line_ix == self.text.len() || scan_blank_line(&bytes[next_line_ix..]).is_some()
            {
                end_ix = next_line_ix;
                break;
            }
            ix = next_line_ix;
            remaining_space = line_start.remaining_space();
            indent = 0;
        }
        self.pop(end_ix);
        #[cfg(all(test, feature = "omni-block-frontier"))]
        self.captured_leaf_blocks.push((
            crate::global_frontier::LeafBlockKind::Html,
            start_ix..end_ix,
            ix,
        ));
        ix
    }

    fn parse_indented_code_block(&mut self, start_ix: usize, mut remaining_space: usize) -> usize {
        self.tree.append(Item {
            start: start_ix,
            end: 0, // will get set later
            body: ItemBody::IndentCodeBlock,
        });
        self.tree.push();
        let bytes = self.text.as_bytes();
        let mut last_nonblank_child = None;
        let mut last_nonblank_ix = 0;
        let mut end_ix = 0;
        self.last_line_blank = false;

        let mut ix = start_ix;
        loop {
            let line_start_ix = ix;
            ix += scan_nextline(&bytes[ix..]);
            self.append_code_text(remaining_space, line_start_ix, ix);
            // TODO(spec clarification): should we synthesize newline at EOF?

            if !self.last_line_blank {
                last_nonblank_child = self.tree.cur();
                last_nonblank_ix = ix;
                end_ix = ix;
            }

            let mut line_start = LineStart::new(&bytes[ix..]);
            let n_containers = scan_containers(&self.tree, &mut line_start, self.options);
            if n_containers < self.tree.spine_len()
                || !(line_start.scan_space(4) || line_start.is_at_eol())
            {
                break;
            }
            let next_line_ix = ix + line_start.bytes_scanned();
            if next_line_ix == self.text.len() {
                break;
            }
            ix = next_line_ix;
            remaining_space = line_start.remaining_space();
            self.last_line_blank = scan_blank_line(&bytes[ix..]).is_some();
        }

        // Trim trailing blank lines.
        if let Some(child) = last_nonblank_child {
            self.tree[child].next = None;
            self.tree[child].item.end = last_nonblank_ix;
        }
        self.pop(end_ix);
        #[cfg(all(test, feature = "omni-block-frontier"))]
        self.captured_leaf_blocks.push((
            crate::global_frontier::LeafBlockKind::IndentedCode,
            start_ix..end_ix,
            ix,
        ));
        ix
    }

    fn parse_fenced_code_block(
        &mut self,
        start_ix: usize,
        indent: usize,
        fence_ch: u8,
        n_fence_char: usize,
    ) -> usize {
        let bytes = self.text.as_bytes();
        let after_marker = start_ix + n_fence_char;
        let mut ix = after_marker + scan_nextline(&bytes[after_marker..]);
        // 与分段 producer 共用原 info 的前后空白边界；解码与 AST 分配仍沿原路径。
        let info = leaf_line_facts::fence_info_bounds(&bytes[after_marker..ix]);
        let info_start = after_marker + info.start;
        let info_end = after_marker + info.end;
        let info_string = unescape(&self.text[info_start..info_end], self.tree.is_in_table());
        self.tree.append(Item {
            start: start_ix,
            end: 0, // will get set later
            body: ItemBody::FencedCodeBlock(self.allocs.allocate_cow(info_string)),
        });
        self.tree.push();
        loop {
            let mut line_start = LineStart::new(&bytes[ix..]);
            let n_containers = scan_containers(&self.tree, &mut line_start, self.options);
            if n_containers < self.tree.spine_len() {
                // this line will get parsed again as not being part of the code
                // if it's blank, it should be parsed as a blank line
                self.pop(ix);
                #[cfg(all(test, feature = "omni-block-frontier"))]
                self.captured_leaf_blocks.push((
                    crate::global_frontier::LeafBlockKind::FencedCode,
                    start_ix..ix,
                    ix,
                ));
                return ix;
            }
            line_start.scan_space(indent);
            let mut close_line_start = line_start.clone();
            if !close_line_start.scan_space(4 - indent) {
                let close_ix = ix + close_line_start.bytes_scanned();
                if let Some(n) = scan_closing_code_fence(&bytes[close_ix..], fence_ch, n_fence_char)
                {
                    ix = close_ix + n;
                    self.pop(ix);
                    // try to read trailing whitespace or it will register as a completely blank line
                    let consumed_end = ix + scan_blank_line(&bytes[ix..]).unwrap_or(0);
                    #[cfg(all(test, feature = "omni-block-frontier"))]
                    self.captured_leaf_blocks.push((
                        crate::global_frontier::LeafBlockKind::FencedCode,
                        start_ix..ix,
                        consumed_end,
                    ));
                    return consumed_end;
                }
            }
            let remaining_space = line_start.remaining_space();
            ix += line_start.bytes_scanned();
            let next_ix = ix + scan_nextline(&bytes[ix..]);
            self.append_code_text(remaining_space, ix, next_ix);
            ix = next_ix;
        }
    }

    fn parse_metadata_block(&mut self, start_ix: usize, metadata_block_ch: u8) -> usize {
        let bytes = self.text.as_bytes();
        let metadata_block_kind = match metadata_block_ch {
            b'-' => MetadataBlockKind::YamlStyle,
            b'+' => MetadataBlockKind::PlusesStyle,
            _ => panic!("Erroneous metadata block character when parsing metadata block"),
        };
        // 3 delimiter characters
        let mut ix = start_ix + 3 + scan_nextline(&bytes[start_ix + 3..]);
        self.tree.append(Item {
            start: start_ix,
            end: 0, // will get set later
            body: ItemBody::MetadataBlock(metadata_block_kind),
        });
        self.tree.push();
        loop {
            let mut line_start = LineStart::new(&bytes[ix..]);
            let n_containers = scan_containers(&self.tree, &mut line_start, self.options);
            if n_containers < self.tree.spine_len() {
                break;
            }
            if let (_, 0) = calc_indent(&bytes[ix..], 4) {
                if let Some(n) = scan_closing_metadata_block(&bytes[ix..], metadata_block_ch) {
                    ix += n;
                    break;
                }
            }
            let remaining_space = line_start.remaining_space();
            ix += line_start.bytes_scanned();
            let next_ix = ix + scan_nextline(&bytes[ix..]);
            self.append_code_text(remaining_space, ix, next_ix);
            ix = next_ix;
        }

        self.pop(ix);

        // try to read trailing whitespace or it will register as a completely blank line
        let consumed_end = ix + scan_blank_line(&bytes[ix..]).unwrap_or(0);
        #[cfg(all(test, feature = "omni-block-frontier"))]
        self.captured_leaf_blocks.push((
            crate::global_frontier::LeafBlockKind::Metadata(metadata_block_kind),
            start_ix..ix,
            consumed_end,
        ));
        consumed_end
    }

    fn append_code_text(&mut self, remaining_space: usize, start: usize, end: usize) {
        if remaining_space > 0 {
            let cow_ix = self.allocs.allocate_cow("   "[..remaining_space].into());
            self.tree.append(Item {
                start,
                end: start,
                body: ItemBody::SynthesizeText(cow_ix),
            });
        }
        if self.text.as_bytes()[end - 2] == b'\r' {
            // Normalize CRLF to LF
            self.tree.append_text(start, end - 2, false);
            self.tree.append_text(end - 1, end, false);
        } else {
            self.tree.append_text(start, end, false);
        }
    }

    /// Appends a line of HTML to the tree.
    fn append_html_line(&mut self, remaining_space: usize, start: usize, end: usize) {
        if remaining_space > 0 {
            let cow_ix = self.allocs.allocate_cow("   "[..remaining_space].into());
            self.tree.append(Item {
                start,
                end: start,
                body: ItemBody::SynthesizeText(cow_ix),
            });
        }
        if self.text.as_bytes()[end - 2] == b'\r' {
            // Normalize CRLF to LF
            self.tree.append(Item {
                start,
                end: end - 2,
                body: ItemBody::Html,
            });
            self.tree.append(Item {
                start: end - 1,
                end,
                body: ItemBody::Html,
            });
        } else {
            self.tree.append(Item {
                start,
                end,
                body: ItemBody::Html,
            });
        }
    }

    /// Pop a container, setting its end.
    fn pop(&mut self, ix: usize) {
        let cur_ix = self.tree.pop().unwrap();
        self.tree[cur_ix].item.end = ix;
        if let ItemBody::DefinitionList(_) = self.tree[cur_ix].item.body {
            fixup_end_of_definition_list(&mut self.tree, cur_ix);
            self.begin_list_item = None;
        }
        if let ItemBody::List(true, _, _) | ItemBody::DefinitionList(true) =
            self.tree[cur_ix].item.body
        {
            surgerize_tight_list(&mut self.tree, cur_ix);
            self.begin_list_item = None;
        }
    }

    /// Close a list if it's open. Also set loose if last line was blank
    /// and end current list if it's a lone, empty item
    fn finish_list(&mut self, ix: usize) {
        crate::container_state::finish_list(self, ix);
    }

    /// Continue an existing list or start a new one if there's not an open
    /// list that matches.
    fn continue_list(&mut self, start: usize, ch: u8, index: u64) {
        if !crate::container_state::prepare_list(self, start, ch) {
            return;
        }
        self.tree.append(Item {
            start,
            end: 0, // will get set later
            body: ItemBody::List(true, ch, index),
        });
        self.tree.push();
        self.last_line_blank = false;
    }

    /// Parse a thematic break.
    ///
    /// Returns index of start of next line.
    fn parse_hrule(&mut self, hrule_size: usize, ix: usize) -> usize {
        self.tree.append(Item {
            start: ix,
            end: ix + hrule_size,
            body: ItemBody::Rule,
        });
        #[cfg(all(test, feature = "omni-block-frontier"))]
        self.captured_leaf_blocks.push((
            crate::global_frontier::LeafBlockKind::ThematicBreak,
            ix..ix + hrule_size,
            ix + hrule_size,
        ));
        ix + hrule_size
    }

    /// Parse an ATX heading.
    ///
    /// Returns index of start of next line.
    fn parse_atx_heading(&mut self, start: usize, atx_level: HeadingLevel) -> usize {
        let mut ix = start;
        let heading_ix = self.tree.append(Item {
            start,
            end: 0,                    // set later
            body: ItemBody::default(), // set later
        });
        ix += atx_level as usize;
        // next char is space or eol (guaranteed by scan_atx_heading)
        let bytes = self.text.as_bytes();
        if let Some(eol_bytes) = scan_eol(&bytes[ix..]) {
            self.tree[heading_ix].item.end = ix + eol_bytes;
            self.tree[heading_ix].item.body = ItemBody::Heading(atx_level, None);
            #[cfg(all(test, feature = "omni-block-frontier"))]
            self.captured_leaf_blocks.push((
                crate::global_frontier::LeafBlockKind::AtxHeading(atx_level),
                start..ix + eol_bytes,
                ix + eol_bytes,
            ));
            return ix + eol_bytes;
        }
        // skip leading spaces
        let skip_spaces = scan_whitespace_no_nl(&bytes[ix..]);
        ix += skip_spaces;

        // now handle the header text
        let header_start = ix;
        let header_node_idx = self.tree.push(); // so that we can set the endpoint later

        // trim the trailing attribute block before parsing the entire line, if necessary
        let (end, content_end, attrs) = if self.options.contains(Options::ENABLE_HEADING_ATTRIBUTES)
        {
            // the start of the next line is the end of the header since the
            // header cannot have line breaks
            let header_end = header_start + scan_nextline(&bytes[header_start..]);
            let (content_end, attrs) =
                self.extract_and_parse_heading_attribute_block(header_start, header_end);
            self.parse_line(ix, Some(content_end), TableParseMode::Disabled);
            (header_end, content_end, attrs)
        } else {
            let (line_ix, line_brk) = self.parse_line(ix, None, TableParseMode::Disabled);
            ix = line_ix;
            // Backslash at end is actually hard line break
            if let Some(Item {
                start,
                end,
                body: ItemBody::HardBreak(true),
            }) = line_brk
            {
                self.tree.append_text(start, end, false);
            }
            (ix, ix, None)
        };
        self.tree[header_node_idx].item.end = end;

        // remove trailing matter from header text
        let mut empty_text_node = false;
        if let Some(cur_ix) = self.tree.cur() {
            // remove closing of the ATX heading
            // 同一个有限标量 kernel 保留原 closing # 与 ASCII space 的精确修剪规则。
            let limit =
                leaf_line_facts::heading_bounds(&bytes[header_start..content_end], false).trim_end;
            // if text is only spaces, then remove them
            self.tree[cur_ix].item.end = limit + header_start;

            // limit = 0 when text is empty after removing spaces
            if limit == 0 {
                empty_text_node = true;
            }
        }

        if empty_text_node {
            self.tree.remove_node();
        } else {
            self.tree.pop();
        }
        self.tree[heading_ix].item.body = ItemBody::Heading(
            atx_level,
            attrs.map(|attrs| self.allocs.allocate_heading(attrs)),
        );

        #[cfg(all(test, feature = "omni-block-frontier"))]
        self.captured_leaf_blocks.push((
            crate::global_frontier::LeafBlockKind::AtxHeading(atx_level),
            start..end,
            end,
        ));

        end
    }

    /// Returns the number of bytes scanned on success.
    fn parse_footnote(&mut self, start: usize) -> Option<usize> {
        let bytes = &self.text.as_bytes()[start..];
        if !bytes.starts_with(b"[^") {
            return None;
        }
        let (mut i, label) = if self.options.has_gfm_footnotes() {
            // GitHub doesn't allow footnote definition labels to contain line breaks.
            // It actually does allow this for link definitions under certain circumstances,
            // but for this it's simpler to avoid it.
            scan_link_label_rest(&self.text[start + 2..], &|_| None, self.tree.is_in_table())?
        } else {
            self.parse_refdef_label(start + 2)?
        };
        if self.options.has_gfm_footnotes() && label.bytes().any(|b| b == b'\r' || b == b'\n') {
            // GitHub doesn't allow footnote definition labels to contain line breaks,
            // even if they're escaped.
            return None;
        }
        i += 2;
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        self.finish_list(start);
        if let Some(node_ix) = self.tree.peek_up() {
            if let ItemBody::FootnoteDefinition(..) = self.tree[node_ix].item.body {
                // finish previous footnote if it's still open
                self.pop(start);
            }
        }
        if self.options.has_gfm_footnotes() {
            i += scan_whitespace_no_nl(&bytes[i..]);
        }
        self.allocs
            .footdefs
            .0
            .insert(UniCase::new(label.clone()), FootnoteDef { use_count: 0 });
        self.tree.append(Item {
            start,
            end: 0, // will get set later
            // TODO: check whether the label here is strictly necessary
            body: ItemBody::FootnoteDefinition(self.allocs.allocate_cow(label)),
        });
        self.tree.push();
        Some(i)
    }

    /// Tries to parse a reference label, which can be interrupted by new blocks.
    /// On success, returns the number of bytes of the label and the label itself.
    fn parse_refdef_label(&self, start: usize) -> Option<(usize, CowStr<'a>)> {
        scan_link_label_rest(
            &self.text[start..],
            &|bytes| {
                let mut line_start = LineStart::new(bytes);
                let tree_position = scan_containers(&self.tree, &mut line_start, self.options);
                let current_container = tree_position == self.tree.spine_len();
                if line_start.scan_space(4) {
                    return Some(line_start.bytes_scanned());
                }
                let bytes_scanned = line_start.bytes_scanned();
                let suffix = &bytes[bytes_scanned..];
                if self.scan_paragraph_interrupt(suffix, current_container, tree_position)
                    || (current_container && scan_setext_heading(suffix).is_some())
                {
                    None
                } else {
                    Some(bytes_scanned)
                }
            },
            self.tree.is_in_table(),
        )
    }

    /// Returns number of bytes scanned, label and definition on success.
    fn parse_refdef_total(&mut self, start: usize) -> Option<(usize, LinkLabel<'a>, LinkDef<'a>)> {
        let bytes = &self.text.as_bytes()[start..];
        if bytes.get(0) != Some(&b'[') {
            return None;
        }
        let (mut i, label) = self.parse_refdef_label(start + 1)?;
        i += 1;
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        let (bytecount, link_def) = self.scan_refdef(start, start + i)?;
        Some((bytecount + i, UniCase::new(label), link_def))
    }

    /// Returns number of bytes and number of newlines
    fn scan_refdef_space(&self, bytes: &[u8], mut i: usize) -> Option<(usize, usize)> {
        use crate::definition_scan::{RefdefSpaceScanner, SpaceStep};
        let start = i;
        let mut scanner = RefdefSpaceScanner::new();
        loop {
            let (consumed, step) = scanner.feed(&bytes[i..], true);
            i += consumed;
            match step {
                SpaceStep::NeedLine { after_eol } => {
                    debug_assert_eq!(i, start + after_eol);
                    let mut line_start = LineStart::new(&bytes[i..]);
                    let tree_position = scan_containers(&self.tree, &mut line_start, self.options);
                    let current_container = tree_position == self.tree.spine_len();
                    if !line_start.scan_space(4) {
                        let suffix = &bytes[i + line_start.bytes_scanned()..];
                        if self.scan_paragraph_interrupt(suffix, current_container, tree_position)
                            || scan_setext_heading(suffix).is_some()
                        {
                            return None;
                        }
                    }
                    i += line_start.bytes_scanned();
                    scanner.continue_line(line_start.bytes_scanned());
                }
                SpaceStep::Complete {
                    byte_count,
                    newlines,
                } => return Some((start + byte_count, newlines)),
                SpaceStep::Rejected => return None,
                SpaceStep::NeedInput => unreachable!("原full-slice空白扫描拥有真实EOF权限"),
            }
        }
    }

    // returns (bytelength, title_str)
    fn scan_refdef_title<'t>(&self, text: &'t str) -> Option<(usize, CowStr<'t>)> {
        use crate::definition_scan::{TitleScanner, TitleStep};
        let bytes = text.as_bytes();
        let mut scanner = TitleScanner::new();
        let mut offset = 0;
        let mut linebuf = None;
        loop {
            let (consumed, step) = scanner.feed(&bytes[offset..], true);
            offset += consumed;
            match step {
                TitleStep::NeedLine {
                    after_eol,
                    segment_start,
                    segment_end,
                } => {
                    // 原AST路径仍按原时点构造多行title值；结构frontier只消费范围，不保存这份正文。
                    let linebuf = linebuf.get_or_insert_with(String::new);
                    linebuf.push_str(&text[segment_start..segment_end]);
                    linebuf.push('\n');
                    debug_assert_eq!(offset, after_eol);
                    let mut line_start = LineStart::new(&bytes[offset..]);
                    let tree_position = scan_containers(&self.tree, &mut line_start, self.options);
                    let current_container = tree_position == self.tree.spine_len();
                    if !line_start.scan_space(4) {
                        let suffix = &bytes[offset + line_start.bytes_scanned()..];
                        if self.scan_paragraph_interrupt(suffix, current_container, tree_position)
                            || scan_setext_heading(suffix).is_some()
                        {
                            return None;
                        }
                    }
                    line_start.scan_all_space();
                    offset += line_start.bytes_scanned();
                    if scan_blank_line(&bytes[offset..]).is_some() {
                        return None;
                    }
                    scanner.continue_line(line_start.bytes_scanned());
                }
                TitleStep::Complete {
                    byte_count,
                    segment_start,
                    segment_end,
                } => {
                    let cow = if let Some(mut linebuf) = linebuf {
                        linebuf.push_str(&text[segment_start..segment_end]);
                        CowStr::from(linebuf)
                    } else {
                        CowStr::from(&text[segment_start..segment_end])
                    };
                    return Some((byte_count, cow));
                }
                TitleStep::Rejected => return None,
                TitleStep::NeedInput => unreachable!("原full-slice title调用拥有真实EOF权限"),
            }
        }
    }

    /// Returns # of bytes and definition.
    /// Assumes the label of the reference including colon has already been scanned.
    fn scan_refdef(&self, span_start: usize, start: usize) -> Option<(usize, LinkDef<'a>)> {
        let bytes = self.text.as_bytes();

        // whitespace between label and url (including up to one newline)
        let (mut i, _newlines) = self.scan_refdef_space(bytes, start)?;

        // scan link dest
        let (dest_length, dest) = scan_link_dest(self.text, i, LINK_MAX_NESTED_PARENS)?;
        if dest_length == 0 {
            return None;
        }
        let dest = unescape(dest, self.tree.is_in_table());
        i += dest_length;

        // no title
        let mut backup = (
            i - start,
            LinkDef {
                dest,
                title: None,
                span: span_start..i,
            },
        );

        // scan whitespace between dest and label
        let (mut i, newlines) =
            if let Some((new_i, mut newlines)) = self.scan_refdef_space(bytes, i) {
                if i == self.text.len() {
                    newlines += 1;
                }
                if new_i == i && newlines == 0 {
                    return None;
                }
                if newlines > 1 {
                    return Some(backup);
                };
                (new_i, newlines)
            } else {
                return Some(backup);
            };

        // scan title
        // if this fails but newline == 1, return also a refdef without title
        if let Some((title_length, title)) = self.scan_refdef_title(&self.text[i..]) {
            i += title_length;
            if scan_blank_line(&bytes[i..]).is_some() {
                backup.0 = i - start;
                backup.1.span = span_start..i;
                backup.1.title = Some(unescape(title, self.tree.is_in_table()));
                return Some(backup);
            }
        }
        if newlines > 0 {
            Some(backup)
        } else {
            None
        }
    }

    /// Checks whether we should break a paragraph on the given input.
    fn scan_paragraph_interrupt(
        &self,
        bytes: &[u8],
        current_container: bool,
        tree_position: usize,
    ) -> bool {
        #[cfg(feature = "omni-block-frontier")]
        if let Some(context) = self.paragraph_context {
            // 原 Global 的最终 Paragraph/Setext Fact 已排除 table 改型；这里仍沿原非 table 中断规则。
            // context 的角色已经真实核为 Quote/List/Item/Paragraph，不借没有祖先的局部 Tree 猜层级。
            return scan_paragraph_interrupt_no_table(
                bytes,
                current_container,
                self.options.contains(Options::ENABLE_FOOTNOTES),
                self.options.contains(Options::ENABLE_DEFINITION_LIST),
                context,
                tree_position,
            );
        }
        if scan_paragraph_interrupt_no_table(
            bytes,
            current_container,
            self.options.contains(Options::ENABLE_FOOTNOTES),
            self.options.contains(Options::ENABLE_DEFINITION_LIST),
            &self.tree,
            tree_position,
        ) {
            return true;
        }
        crate::block_context::scan_heavy_table_interrupt(
            bytes,
            &self.tree,
            self.tree.spine_len(),
            self.options,
        )
    }

    /// 原 Root Tree 与真实 scoped context 共用同一 paragraph 控制流；只替换容器事实来源。
    fn paragraph_line_start(
        &mut self,
        start: usize,
        end: usize,
    ) -> Option<(LineStart<'a>, usize, bool)> {
        let bytes = &self.text.as_bytes()[start..end];
        #[cfg(feature = "omni-block-frontier")]
        if let Some(context) = self.paragraph_context {
            return match context.line_state_at(start) {
                Ok((state, matched, depth)) => Some((
                    LineStart::from_stream_state(bytes, state),
                    matched,
                    matched == depth,
                )),
                Err(error) => {
                    self.paragraph_seed_error = Some(error);
                    None // 真 context 失配拒绝整个有限 Parser，不重扫 Root 或发布部分 AST。
                }
            };
        }
        let mut line = LineStart::new(bytes);
        let matched = scan_containers(&self.tree, &mut line, self.options);
        Some((line, matched, matched == self.tree.spine_len()))
    }

    /// Extracts and parses a heading attribute block if exists.
    ///
    /// Returns `(end_offset_of_heading_content, (id, classes))`.
    ///
    /// If `header_end` is less than or equal to `header_start`, the given
    /// input is considered as empty.
    fn extract_and_parse_heading_attribute_block(
        &mut self,
        header_start: usize,
        header_end: usize,
    ) -> (usize, Option<HeadingAttributes<'a>>) {
        if !self.options.contains(Options::ENABLE_HEADING_ATTRIBUTES) {
            return (header_end, None);
        }

        // extract the trailing attribute block
        let header_bytes = &self.text.as_bytes()[header_start..header_end];
        let (content_len, attr_block_range_rel) =
            extract_attribute_block_content_from_header_text(header_bytes);
        let content_end = header_start + content_len;
        let attrs = attr_block_range_rel.and_then(|r| {
            parse_inside_attribute_block(
                &self.text[(header_start + r.start)..(header_start + r.end)],
            )
        });
        (content_end, attrs)
    }
}

// 此view只映射结构字段；共享状态转移不会触及已完成inline child或新增副本。
impl crate::container_state::ListState for FirstPass<'_, '_> {
    fn parent_kind(&self) -> crate::container_state::ParentKind {
        use crate::container_state::ParentKind;
        match self.tree.peek_up().map(|index| self.tree[index].item.body) {
            Some(ItemBody::List(_, marker, _)) => ParentKind::List { marker },
            Some(ItemBody::DefinitionList(_)) => ParentKind::DefinitionList,
            Some(ItemBody::ListItem(_) | ItemBody::DefinitionListDefinition(_)) => ParentKind::Item,
            Some(_) => ParentKind::Other,
            None => ParentKind::Root,
        }
    }
    fn begin_list_item(&self) -> Option<usize> {
        self.begin_list_item
    }
    fn set_begin_list_item(&mut self, start: Option<usize>) {
        self.begin_list_item = start;
    }
    fn last_line_blank(&self) -> bool {
        self.last_line_blank
    }
    fn set_last_line_blank(&mut self, blank: bool) {
        self.last_line_blank = blank;
    }
    fn pop_container(&mut self, end: usize) {
        self.pop(end);
    }
    fn loosen_parent_list(&mut self) {
        if let Some(index) = self.tree.peek_up() {
            if let ItemBody::List(ref mut tight, _, _) = self.tree[index].item.body {
                *tight = false;
            }
        }
    }
    fn loosen_grandparent_list(&mut self) {
        if let Some(index) = self.tree.peek_grandparent() {
            if let ItemBody::List(ref mut tight, _, _) | ItemBody::DefinitionList(ref mut tight) =
                self.tree[index].item.body
            {
                *tight = false;
            }
        }
    }
}

/// Scanning modes for `Parser`'s `parse_line` method.
#[derive(PartialEq, Eq, Copy, Clone)]
enum TableParseMode {
    /// Inside a paragraph, scanning for table headers.
    Scan,
    /// Inside a table.
    Active,
    /// Inside a paragraph, not scanning for table headers.
    Disabled,
}

fn surgerize_tight_list(tree: &mut Tree<Item>, list_ix: TreeIndex) {
    let mut list_item = tree[list_ix].child;
    while let Some(listitem_ix) = list_item {
        let mut node_ix = tree[listitem_ix].child;
        while let Some(node) = node_ix {
            if let ItemBody::Paragraph = tree[node].item.body {
                tree[node].item.body = ItemBody::TightParagraph;
            }
            node_ix = tree[node].next;
        }

        list_item = tree[listitem_ix].next;
    }
}

fn fixup_end_of_definition_list(tree: &mut Tree<Item>, list_ix: TreeIndex) {
    let mut list_item = tree[list_ix].child;
    let mut previous_list_item = None;
    while let Some(listitem_ix) = list_item {
        match &mut tree[listitem_ix].item.body {
            ItemBody::DefinitionListTitle | ItemBody::DefinitionListDefinition(_) => {
                previous_list_item = list_item;
                list_item = tree[listitem_ix].next;
            }
            body @ ItemBody::MaybeDefinitionListTitle => {
                *body = ItemBody::Paragraph;
                break;
            }
            _ => break,
        }
    }
    if let Some(previous_list_item) = previous_list_item {
        tree.truncate_to_parent(previous_list_item);
    }
}

/// Determines whether the delimiter run starting at given index is
/// left-flanking, as defined by the commonmark spec (and isn't intraword
/// for _ delims).
/// suffix is &s[ix..], which is passed in as an optimization, since taking
/// a string subslice is O(n).
fn delim_run_can_open(
    s: &str,
    suffix: &str,
    run_len: usize,
    ix: usize,
    mode: TableParseMode,
    options: Options,
) -> bool {
    // 原 firstpass 只提取真实源邻接；分段 Root 游标与本入口共用同一 Unicode/table/flanking 内核。
    crate::inline_emphasis::can_open(
        s[..ix].chars().last(),
        suffix[run_len..].chars().next(),
        suffix.as_bytes()[0],
        run_len,
        mode == TableParseMode::Active,
        s.as_bytes()[..ix].ends_with(b"|") && !s.as_bytes()[..ix].ends_with(br"\|"),
        options,
    )
}

/// 原关闭规则同样只在真实块边界传 None；源 frame 尾部不属于这个调用合同。
fn delim_run_can_close(
    s: &str,
    suffix: &str,
    run_len: usize,
    ix: usize,
    mode: TableParseMode,
    options: Options,
) -> bool {
    crate::inline_emphasis::can_close(
        s[..ix].chars().last(),
        suffix[run_len..].chars().next(),
        suffix.as_bytes()[0],
        run_len,
        mode == TableParseMode::Active,
        s.as_bytes()[..ix].ends_with(b"|") && !s.as_bytes()[..ix].ends_with(br"\|"),
        options,
    )
}

fn create_lut(options: &Options) -> LookupTable {
    #[cfg(all(target_arch = "x86_64", feature = "simd"))]
    {
        LookupTable {
            simd: simd::compute_lookup(options),
            scalar: special_bytes(options),
        }
    }
    #[cfg(not(all(target_arch = "x86_64", feature = "simd")))]
    {
        special_bytes(options)
    }
}

// 原 firstpass 与真实闭合块的 Plain 证明共享同一张表；这里不授予调用方块分类权限。
pub(crate) fn special_bytes(options: &Options) -> [bool; 256] {
    let mut bytes = [false; 256];
    let standard_bytes = [
        b'\n', b'\r', b'*', b'_', b'&', b'\\', b'[', b']', b'<', b'!', b'`',
    ];

    for &byte in &standard_bytes {
        bytes[byte as usize] = true;
    }
    if options.contains(Options::ENABLE_TABLES) {
        bytes[b'|' as usize] = true;
    }
    if options.contains(Options::ENABLE_STRIKETHROUGH)
        || options.contains(Options::ENABLE_SUBSCRIPT)
    {
        bytes[b'~' as usize] = true;
    }
    if options.contains(Options::ENABLE_SUPERSCRIPT) {
        bytes[b'^' as usize] = true;
    }
    if options.contains(Options::ENABLE_MATH) {
        bytes[b'$' as usize] = true;
        bytes[b'{' as usize] = true;
        bytes[b'}' as usize] = true;
    }
    if options.contains(Options::ENABLE_SMART_PUNCTUATION) {
        for &byte in &[b'.', b'-', b'"', b'\''] {
            bytes[byte as usize] = true;
        }
    }

    bytes
}

enum LoopInstruction<T> {
    /// Continue looking for more special bytes, but skip next few bytes.
    ContinueAndSkip(usize),
    /// Break looping immediately, returning with the given index and value.
    BreakAtWith(usize, T),
}

#[cfg(all(target_arch = "x86_64", feature = "simd"))]
struct LookupTable {
    simd: [u8; 16],
    scalar: [bool; 256],
}

#[cfg(not(all(target_arch = "x86_64", feature = "simd")))]
type LookupTable = [bool; 256];

/// This function walks the byte slices from the given index and
/// calls the callback function on all bytes (and their indices) that are in the following set:
/// `` ` ``, `\`, `&`, `*`, `_`, `~`, `!`, `<`, `[`, `]`, `|`, `\r`, `\n`
/// It is guaranteed not call the callback on other bytes.
/// Whenever `callback(ix, byte)` returns a `ContinueAndSkip(n)` value, the callback
/// will not be called with an index that is less than `ix + n + 1`.
/// When the callback returns a `BreakAtWith(end_ix, opt+val)`, no more callbacks will be
/// called and the function returns immediately with the return value `(end_ix, opt_val)`.
/// If `BreakAtWith(..)` is never returned, this function will return the first
/// index that is outside the byteslice bound and a `None` value.
fn iterate_special_bytes<F, T>(
    lut: &LookupTable,
    bytes: &[u8],
    ix: usize,
    callback: F,
) -> (usize, Option<T>)
where
    F: FnMut(usize, u8) -> LoopInstruction<Option<T>>,
{
    #[cfg(all(target_arch = "x86_64", feature = "simd"))]
    {
        simd::iterate_special_bytes(lut, bytes, ix, callback)
    }
    #[cfg(not(all(target_arch = "x86_64", feature = "simd")))]
    {
        scalar_iterate_special_bytes(lut, bytes, ix, callback)
    }
}

fn scalar_iterate_special_bytes<F, T>(
    lut: &[bool; 256],
    bytes: &[u8],
    mut ix: usize,
    mut callback: F,
) -> (usize, Option<T>)
where
    F: FnMut(usize, u8) -> LoopInstruction<Option<T>>,
{
    while ix < bytes.len() {
        let b = bytes[ix];
        if lut[b as usize] {
            match callback(ix, b) {
                LoopInstruction::ContinueAndSkip(skip) => {
                    ix += skip;
                }
                LoopInstruction::BreakAtWith(ix, val) => {
                    return (ix, val);
                }
            }
        }
        ix += 1;
    }

    (ix, None)
}

/// Split the usual heading content range and the content inside the trailing attribute block.
///
/// Returns `(leading_content_len, Option<trailing_attr_block_range>)`.
///
/// Note that `trailing_attr_block_range` will be empty range when the block
/// is `{}`, since the range is content inside the wrapping `{` and `}`.
///
/// The closing `}` of an attribute block can have trailing whitespaces.
/// They are automatically trimmed when the attribute block is being searched.
///
/// However, this method does not trim the trailing whitespaces of heading content.
/// It is callers' responsibility to trim them if necessary.
fn extract_attribute_block_content_from_header_text(
    heading: &[u8],
) -> (usize, Option<Range<usize>>) {
    // 同一 kernel 只返回原 attributes enclosure；值解析与有效性仍由调用者原函数决定。
    let bounds = leaf_line_facts::heading_bounds(heading, true);
    (bounds.content_end, bounds.attributes)
}

/// Parses an attribute block content, such as `.class1 #id .class2`.
///
/// Returns `(id, classes)`.
///
/// It is callers' responsibility to find opening and closing characters of the attribute
/// block. Usually [`extract_attribute_block_content_from_header_text`] function does it for you.
///
/// Note that this parsing requires explicit whitespace separators between
/// attributes. This is intentional design with the reasons below:
///
/// * to keep conversion simple and easy to understand for any possible input,
/// * to avoid adding less obvious conversion rule that can reduce compatibility
///   with other implementations more, and
/// * to follow the major design of implementations with the support for the
///   attribute blocks extension (as of this writing).
///
/// See also: [`Options::ENABLE_HEADING_ATTRIBUTES`].
///
/// [`Options::ENABLE_HEADING_ATTRIBUTES`]: `crate::Options::ENABLE_HEADING_ATTRIBUTES`
fn parse_inside_attribute_block(inside_attr_block: &str) -> Option<HeadingAttributes<'_>> {
    let mut id = None;
    let mut classes = Vec::new();
    let mut attrs = Vec::new();

    for attr in inside_attr_block.split_ascii_whitespace() {
        // iterator returned by `str::split_ascii_whitespace` never emits empty
        // strings, so taking first byte won't panic.
        if attr.len() > 1 {
            let first_byte = attr.as_bytes()[0];
            if first_byte == b'#' {
                id = Some(attr[1..].into());
            } else if first_byte == b'.' {
                classes.push(attr[1..].into());
            } else {
                let split = attr.split_once('=');
                if let Some((key, value)) = split {
                    attrs.push((key.into(), Some(value.into())));
                } else {
                    attrs.push((attr.into(), None));
                }
            }
        }
    }

    Some(HeadingAttributes { id, classes, attrs })
}

#[cfg(all(target_arch = "x86_64", feature = "simd"))]
mod simd {
    //! SIMD byte scanning logic.
    //!
    //! This module provides functions that allow walking through byteslices, calling
    //! provided callback functions on special bytes and their indices using SIMD.
    //! The byteset is defined in `compute_lookup`.
    //!
    //! The idea is to load in a chunk of 16 bytes and perform a lookup into a set of
    //! bytes on all the bytes in this chunk simultaneously. We produce a 16 bit bitmask
    //! from this and call the callback on every index corresponding to a 1 in this mask
    //! before moving on to the next chunk. This allows us to move quickly when there
    //! are no or few matches.
    //!
    //! The table lookup is inspired by this [great overview]. However, since all of the
    //! bytes we're interested in are ASCII, we don't quite need the full generality of
    //! the universal algorithm and are hence able to skip a few instructions.
    //!
    //! [great overview]: http://0x80.pl/articles/simd-byte-lookup.html

    use super::{LookupTable, LoopInstruction};
    use crate::Options;
    use core::arch::x86_64::*;

    const VECTOR_SIZE: usize = std::mem::size_of::<__m128i>();

    /// Generates a lookup table containing the bitmaps for our
    /// special marker bytes. This is effectively a 128 element 2d bitvector,
    /// that can be indexed by a four bit row index (the lower nibble)
    /// and a three bit column index (upper nibble).
    pub(super) fn compute_lookup(options: &Options) -> [u8; 16] {
        let mut lookup = [0u8; 16];
        let standard_bytes = [
            b'\n', b'\r', b'*', b'_', b'&', b'\\', b'[', b']', b'<', b'!', b'`',
        ];

        for &byte in &standard_bytes {
            add_lookup_byte(&mut lookup, byte);
        }
        if options.contains(Options::ENABLE_TABLES) {
            add_lookup_byte(&mut lookup, b'|');
        }
        if options.contains(Options::ENABLE_STRIKETHROUGH)
            || options.contains(Options::ENABLE_SUBSCRIPT)
        {
            add_lookup_byte(&mut lookup, b'~');
        }
        if options.contains(Options::ENABLE_SUPERSCRIPT) {
            add_lookup_byte(&mut lookup, b'^');
        }
        if options.contains(Options::ENABLE_MATH) {
            add_lookup_byte(&mut lookup, b'$');
            add_lookup_byte(&mut lookup, b'{');
            add_lookup_byte(&mut lookup, b'}');
        }
        if options.contains(Options::ENABLE_SMART_PUNCTUATION) {
            for &byte in &[b'.', b'-', b'"', b'\''] {
                add_lookup_byte(&mut lookup, byte);
            }
        }

        lookup
    }

    fn add_lookup_byte(lookup: &mut [u8; 16], byte: u8) {
        lookup[(byte & 0x0f) as usize] |= 1 << (byte >> 4);
    }

    /// Computes a bit mask for the given byteslice starting from the given index,
    /// where the 16 least significant bits indicate (by value of 1) whether or not
    /// there is a special character at that byte position. The least significant bit
    /// corresponds to `bytes[ix]` and the most significant bit corresponds to
    /// `bytes[ix + 15]`.
    /// It is only safe to call this function when `bytes.len() >= ix + VECTOR_SIZE`.
    #[target_feature(enable = "ssse3")]
    #[inline]
    unsafe fn compute_mask(lut: &[u8; 16], bytes: &[u8], ix: usize) -> i32 {
        debug_assert!(bytes.len() >= ix + VECTOR_SIZE);

        let bitmap = _mm_loadu_si128(lut.as_ptr() as *const __m128i);
        // Small lookup table to compute single bit bitshifts
        // for 16 bytes at once.
        let bitmask_lookup =
            _mm_setr_epi8(1, 2, 4, 8, 16, 32, 64, -128, -1, -1, -1, -1, -1, -1, -1, -1);

        // Load input from memory.
        let raw_ptr = bytes.as_ptr().add(ix) as *const __m128i;
        let input = _mm_loadu_si128(raw_ptr);
        // Compute the bitmap using the bottom nibble as an index
        // into the lookup table. Note that non-ascii bytes will have
        // their most significant bit set and will map to lookup[0].
        let bitset = _mm_shuffle_epi8(bitmap, input);
        // Compute the high nibbles of the input using a 16-bit rightshift of four
        // and a mask to prevent most-significant bit issues.
        let higher_nibbles = _mm_and_si128(_mm_srli_epi16(input, 4), _mm_set1_epi8(0x0f));
        // Create a bitmask for the bitmap by perform a left shift of the value
        // of the higher nibble. Bytes with their most significant set are mapped
        // to -1 (all ones).
        let bitmask = _mm_shuffle_epi8(bitmask_lookup, higher_nibbles);
        // Test the bit of the bitmap by AND'ing the bitmap and the mask together.
        let tmp = _mm_and_si128(bitset, bitmask);
        // Check whether the result was not null. NEQ is not a SIMD intrinsic,
        // but comparing to the bitmask is logically equivalent. This also prevents us
        // from matching any non-ASCII bytes since none of the bitmaps were all ones
        // (-1).
        let result = _mm_cmpeq_epi8(tmp, bitmask);

        // Return the resulting bitmask.
        _mm_movemask_epi8(result)
    }

    /// Calls callback on byte indices and their value.
    /// Breaks when callback returns LoopInstruction::BreakAtWith(ix, val). And skips the
    /// number of bytes in callback return value otherwise.
    /// Returns the final index and a possible break value.
    pub(super) fn iterate_special_bytes<F, T>(
        lut: &LookupTable,
        bytes: &[u8],
        ix: usize,
        callback: F,
    ) -> (usize, Option<T>)
    where
        F: FnMut(usize, u8) -> LoopInstruction<Option<T>>,
    {
        if is_x86_feature_detected!("ssse3") && bytes.len() >= VECTOR_SIZE {
            unsafe { simd_iterate_special_bytes(&lut.simd, bytes, ix, callback) }
        } else {
            super::scalar_iterate_special_bytes(&lut.scalar, bytes, ix, callback)
        }
    }

    /// Calls the callback function for every 1 in the given bitmask with
    /// the index `offset + ix`, where `ix` is the position of the 1 in the mask.
    /// Returns `Ok(ix)` to continue from index `ix`, `Err((end_ix, opt_val)` to break with
    /// final index `end_ix` and optional value `opt_val`.
    unsafe fn process_mask<F, T>(
        mut mask: i32,
        bytes: &[u8],
        mut offset: usize,
        callback: &mut F,
    ) -> Result<usize, (usize, Option<T>)>
    where
        F: FnMut(usize, u8) -> LoopInstruction<Option<T>>,
    {
        while mask != 0 {
            let mask_ix = mask.trailing_zeros() as usize;
            offset += mask_ix;
            match callback(offset, *bytes.get_unchecked(offset)) {
                LoopInstruction::ContinueAndSkip(skip) => {
                    offset += skip + 1;
                    let shift = skip + 1 + mask_ix;
                    if shift >= 32 {
                        break;
                    }
                    mask >>= shift;
                }
                LoopInstruction::BreakAtWith(ix, val) => return Err((ix, val)),
            }
        }
        Ok(offset)
    }

    #[target_feature(enable = "ssse3")]
    /// Important: only call this function when `bytes.len() >= 16`. Doing
    /// so otherwise may exhibit undefined behaviour.
    unsafe fn simd_iterate_special_bytes<F, T>(
        lut: &[u8; 16],
        bytes: &[u8],
        mut ix: usize,
        mut callback: F,
    ) -> (usize, Option<T>)
    where
        F: FnMut(usize, u8) -> LoopInstruction<Option<T>>,
    {
        debug_assert!(bytes.len() >= VECTOR_SIZE);
        let upperbound = bytes.len() - VECTOR_SIZE;

        while ix < upperbound {
            let mask = compute_mask(lut, bytes, ix);
            let block_start = ix;
            ix = match process_mask(mask, bytes, ix, &mut callback) {
                Ok(ix) => std::cmp::max(ix, VECTOR_SIZE + block_start),
                Err((end_ix, val)) => return (end_ix, val),
            };
        }

        if bytes.len() > ix {
            // shift off the bytes at start we have already scanned
            let mask = compute_mask(lut, bytes, upperbound) >> ix - upperbound;
            if let Err((end_ix, val)) = process_mask(mask, bytes, ix, &mut callback) {
                return (end_ix, val);
            }
        }

        (bytes.len(), None)
    }

    #[cfg(test)]
    mod simd_test {
        use super::super::create_lut;
        use super::{iterate_special_bytes, LoopInstruction};
        use crate::Options;

        fn check_expected_indices(bytes: &[u8], expected: &[usize], skip: usize) {
            let mut opts = Options::empty();
            opts.insert(Options::ENABLE_MATH);
            opts.insert(Options::ENABLE_TABLES);
            opts.insert(Options::ENABLE_FOOTNOTES);
            opts.insert(Options::ENABLE_STRIKETHROUGH);
            opts.insert(Options::ENABLE_SUPERSCRIPT);
            opts.insert(Options::ENABLE_TASKLISTS);

            let lut = create_lut(&opts);
            let mut indices = vec![];

            iterate_special_bytes::<_, i32>(&lut, bytes, 0, |ix, _byte_ty| {
                indices.push(ix);
                LoopInstruction::ContinueAndSkip(skip)
            });

            assert_eq!(&indices[..], expected);
        }

        #[test]
        fn simple_no_match() {
            check_expected_indices("abcdef0123456789".as_bytes(), &[], 0);
        }

        #[test]
        fn simple_match() {
            check_expected_indices("*bcd&f0123456789".as_bytes(), &[0, 4], 0);
        }

        #[test]
        fn single_open_fish() {
            check_expected_indices("<".as_bytes(), &[0], 0);
        }

        #[test]
        fn long_match() {
            check_expected_indices("0123456789abcde~*bcd&f0".as_bytes(), &[15, 16, 20], 0);
        }

        #[test]
        fn border_skip() {
            check_expected_indices("0123456789abcde~~~~d&f0".as_bytes(), &[15, 20], 3);
        }

        #[test]
        fn exhaustive_search() {
            let chars = [
                b'\n', b'\r', b'*', b'_', b'~', b'^', b'|', b'&', b'\\', b'[', b']', b'<', b'!',
                b'`', b'$', b'{', b'}',
            ];

            for &c in &chars {
                for i in 0u8..=255 {
                    if !chars.contains(&i) {
                        // full match
                        let mut buf = [i; 18];
                        buf[3] = c;
                        buf[6] = c;

                        check_expected_indices(&buf[..], &[3, 6], 0);
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "omni-block-frontier"))]
/// 仅纯测prefix seed：输入先受64KiB预算约束，不把完整文档交给旧firstpass求上下文。
pub(crate) fn capture_table_seeds(
    text: &str,
    options: Options,
) -> Vec<crate::block_frontier::tests::CapturedTableSeed> {
    assert!(text.len() <= 64 * 1024);
    let lookup_table = &create_lut(&options);
    let mut pass = first_pass(text, options, lookup_table, &[]);
    let mut ix = 0;
    while ix < text.len() {
        ix = pass.parse_block(ix);
    }
    pass.captured_tables
}

#[cfg(all(test, feature = "omni-block-frontier"))]
/// 从真实完整firstpass捕获行入口状态，只在≤64KiB纯测oracle启用，不进入产品。
pub(crate) fn capture_math_rows(text: &str, options: Options) -> Vec<(usize, MathContext)> {
    assert!(text.len() <= 64 * 1024);
    let lookup_table = &create_lut(&options);
    let mut pass = first_pass(text, options, lookup_table, &[]);
    let mut ix = 0;
    while ix < text.len() {
        ix = pass.parse_block(ix);
    }
    pass.captured_math_rows
}

#[cfg(all(test, feature = "omni-block-frontier"))]
/// 纯差分直接调用原firstpass完成路径；完整AST只属于合成oracle，不进入产品producer。
pub(crate) fn capture_closed_inline_blocks(
    text: &str,
    options: Options,
) -> Vec<(
    crate::global_frontier::InlineBlockKind,
    Range<usize>,
    Range<usize>,
)> {
    let lookup_table = &create_lut(&options);
    let mut pass = first_pass(text, options, lookup_table, &[]);
    let mut ix = 0;
    while ix < text.len() {
        ix = pass.parse_block(ix);
    }
    pass.captured_inline_blocks
}

#[cfg(all(test, feature = "omni-block-frontier"))]
/// 从原叶块函数的真实 pop 与返回点捕获源范围和 cursor；完整 AST 只属于合成差分 oracle。
pub(crate) fn capture_closed_leaf_blocks(
    text: &str,
    options: Options,
) -> Vec<(crate::global_frontier::LeafBlockKind, Range<usize>, usize)> {
    let lookup_table = &create_lut(&options);
    let mut pass = first_pass(text, options, lookup_table, &[]);
    let mut ix = 0;
    while ix < text.len() {
        ix = pass.parse_block(ix);
    }
    pass.captured_leaf_blocks
}
