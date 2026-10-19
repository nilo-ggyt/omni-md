//! CommonMark/GFM parsing, decoration spans, tables, and UTF-16 projection.

use super::*;

mod heading_markers;
pub(crate) use heading_markers::HeadingMarkerTail;

#[derive(Default)]
/// Completed bounded projection output transferred into the public presentation cache.
pub(super) struct MarkdownPresentationBuild {
    pub(super) runs: Vec<MarkdownPresentationRun>,
    pub(super) tables: Vec<MarkdownTablePrototype>,
    pub(super) list_markers: Vec<MarkdownPreviewListMarker>,
}

#[derive(Clone, Debug)]
/// Parser-local source span and optional owner used before one shared UTF-16 projection pass.
pub(super) struct MarkdownByteDecoration {
    pub(super) byte_range: Range<usize>,
    pub(super) styles: MarkdownPresentationStyles,
    pub(super) marker_owner: Option<super::prefixes::MarkdownMarkerOwner>,
}

#[derive(Clone, Debug)]
/// Parser-local cell extent with resolved column index and alignment.
pub(super) struct MarkdownByteTableCellPrototype {
    pub(super) byte_range: Range<usize>,
    pub(super) column_index: usize,
    pub(super) alignment: MarkdownTableAlignment,
}

#[derive(Clone, Debug)]
/// Parser-local row extent indexing a flat cell vector.
pub(super) struct MarkdownByteTableRowPrototype {
    pub(super) byte_range: Range<usize>,
    pub(super) cell_index_range: Range<usize>,
    pub(super) is_header: bool,
}

#[derive(Clone, Debug)]
/// Completed parser-local table structure before absolute-coordinate conversion.
pub(super) struct MarkdownByteTablePrototype {
    pub(super) byte_range: Range<usize>,
    pub(super) header_byte_range: Range<usize>,
    pub(super) delimiter_byte_range: Range<usize>,
    pub(super) rows: Vec<MarkdownByteTableRowPrototype>,
    pub(super) cells: Vec<MarkdownByteTableCellPrototype>,
}

/// Table state assembled in one pulldown-cmark event pass without reparsing source.
pub(super) struct MarkdownByteTableBuilder {
    pub(super) byte_range: Range<usize>,
    pub(super) alignments: Vec<Alignment>,
    pub(super) header_byte_range: Option<Range<usize>>,
    pub(super) rows: Vec<MarkdownByteTableRowPrototype>,
    pub(super) cells: Vec<MarkdownByteTableCellPrototype>,
    pub(super) active_row_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One style occurrence entering or leaving an absolute UTF-16 position.
pub(super) struct MarkdownStyleTransition {
    pub(super) utf16_offset: usize,
    pub(super) style_bit: usize,
    pub(super) delta: i8,
}

/// 普通完整envelope保持既有解析选项、引用范围和underline配对，不为窗口候选增加额外缓存。
pub(super) fn build_markdown_presentation(
    source: &str,
    absolute_utf16_start: usize,
    active_selection: &Range<usize>,
) -> Result<MarkdownPresentationBuild, MarkdownViewportPresentationError> {
    build_markdown_presentation_with_context(
        source,
        absolute_utf16_start,
        active_selection,
        absolute_utf16_start == 0,
        None,
        true,
    )
}

/// 属性与有限引用查询使用完全相同的Parser选项，防止两条路径识别不同的inline标签。
pub(crate) fn markdown_presentation_options(actual_document_start: bool) -> Options {
    let mut options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_FOOTNOTES;
    if actual_document_start {
        // 投影起点不等于真实文档起点；只有调用方提供的真实首行才启用YAML规则。
        options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    }
    options
}

#[derive(Clone, Copy)]
pub(super) enum MarkdownPresentationReferenceContext<'a> {
    Definitions(&'a str),
    #[cfg(feature = "s2-table-window-prototype")]
    Resolved(&'a table_window::references::MarkdownTableReferenceQueries),
}

/// 有限投影复用相同public Parser；外部定义只用于查找，跨源片段underline必须由真实源证明。
pub(super) fn build_markdown_presentation_with_context<'a>(
    source: &'a str,
    absolute_utf16_start: usize,
    active_selection: &Range<usize>,
    actual_document_start: bool,
    reference_context: Option<&'a str>,
    local_underline: bool,
) -> Result<MarkdownPresentationBuild, MarkdownViewportPresentationError> {
    build_markdown_presentation_with_reference_context(
        source,
        absolute_utf16_start,
        active_selection,
        actual_document_start,
        reference_context.map(MarkdownPresentationReferenceContext::Definitions),
        local_underline,
    )
}

/// resolver只借当前有限查询和值；普通envelope保留原有局部RefDefs行为。
pub(super) fn build_markdown_presentation_with_reference_context<'a>(
    source: &'a str,
    absolute_utf16_start: usize,
    active_selection: &Range<usize>,
    actual_document_start: bool,
    reference_context: Option<MarkdownPresentationReferenceContext<'a>>,
    local_underline: bool,
) -> Result<MarkdownPresentationBuild, MarkdownViewportPresentationError> {
    build_markdown_presentation_with_seeded_reference_context(
        source,
        absolute_utf16_start,
        active_selection,
        actual_document_start,
        reference_context,
        local_underline,
        #[cfg(feature = "s2-table-window-prototype")]
        None,
    )
}

/// 真实source producer提供每个投影row的数学入口状态；普通路径不提供seed，沿原Parser行为。
pub(super) fn build_markdown_presentation_with_seeded_reference_context<'a>(
    source: &'a str,
    absolute_utf16_start: usize,
    active_selection: &Range<usize>,
    actual_document_start: bool,
    reference_context: Option<MarkdownPresentationReferenceContext<'a>>,
    local_underline: bool,
    #[cfg(feature = "s2-table-window-prototype")] math_seeds: Option<
        &[pulldown_cmark::math_context::TableMathSeed],
    >,
) -> Result<MarkdownPresentationBuild, MarkdownViewportPresentationError> {
    let mut decorations = Vec::new();
    let options = markdown_presentation_options(actual_document_start);
    let mut item_ranges = Vec::new();
    let mut item_prefixes = Vec::new();
    let mut prefixes = super::prefixes::MarkdownPrefixBuilder::default();
    let mut table_prototypes = Vec::new();
    let mut active_table = None;

    // 普通路径为None，不构造第二Parser；窗口路径的定义文本已由入口单独计入有限预算。
    let references = match reference_context {
        Some(MarkdownPresentationReferenceContext::Definitions(text)) => {
            Some(Parser::new_ext(text, options))
        }
        #[cfg(feature = "s2-table-window-prototype")]
        Some(MarkdownPresentationReferenceContext::Resolved(_)) | None => None,
        #[cfg(not(feature = "s2-table-window-prototype"))]
        None => None,
    };
    let mut resolve_reference = |link: pulldown_cmark::BrokenLink<'a>| {
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(MarkdownPresentationReferenceContext::Resolved(queries)) = reference_context {
            // 已验证的有限值活到此Parser结束；callback借用URL/title，不为每个链接再复制值。
            return queries
                .resolve(&link.reference)
                .map(|(destination, title)| (destination.into(), title.into()));
        }
        // 使用固定依赖自己的label规范化/first-wins查找，不重写Unicode大小写或返回伪造URL。
        references.as_ref().and_then(|parser| {
            parser
                .reference_definitions()
                .get(&link.reference)
                .map(|definition| {
                    // resolver的局部Parser寿命更短，返回有限独立metadata；外层Parser的expansion fuel限制总量。
                    (
                        definition.dest.clone().into_static(),
                        definition
                            .title
                            .clone()
                            .map_or_else(|| "".into(), |title| title.into_static()),
                    )
                })
        })
    };
    let callback = reference_context.map(|_| &mut resolve_reference);
    #[cfg(feature = "s2-table-window-prototype")]
    let parser = if let Some(seeds) = math_seeds {
        // seed预算在任何AST创建前核验；失败不退回无seed的错误语义。
        Parser::new_with_table_math_seeds(source, options, callback, seeds)
            .map_err(|_| MarkdownViewportPresentationError::InvalidParserBoundary)?
    } else {
        Parser::new_with_broken_link_callback(source, options, callback)
    };
    #[cfg(not(feature = "s2-table-window-prototype"))]
    let parser = Parser::new_with_broken_link_callback(source, options, callback);
    for (event, byte_range) in parser.into_offset_iter() {
        // Consume parser events once while retaining only active list and table construction state.
        match event {
            // Start events establish source spans or nested structures used by later end events.
            Event::Start(tag) => match tag {
                // A heading receives its level style and separately tracked editable delimiter spans.
                Tag::Heading { level, .. } => {
                    push_markdown_decoration(
                        &mut decorations,
                        byte_range.clone(),
                        markdown_heading_style(level),
                    )?;
                    add_heading_markers(source, &byte_range, &mut decorations)?;
                }
                // quote范围只证明容器归属；物理前缀在事件结束后按容器次序单次分配。
                Tag::BlockQuote(_) => {
                    push_markdown_decoration(
                        &mut decorations,
                        byte_range.clone(),
                        MarkdownPresentationStyles::BLOCK_QUOTE,
                    )?;
                    prefixes.quote(byte_range, &decorations)?;
                }
                // Both indented and fenced code receive code styling without executing their contents.
                Tag::CodeBlock(kind) => {
                    push_markdown_decoration(
                        &mut decorations,
                        byte_range.clone(),
                        MarkdownPresentationStyles::FENCED_CODE,
                    )?;
                    if matches!(kind, CodeBlockKind::Fenced(_)) {
                        // Only fenced code has explicit opening and closing marker lines to decorate.
                        add_fenced_code_markers(source, &byte_range, &mut decorations)?;
                    }
                }
                // List container metadata distinguishes ordered from unordered source styling.
                Tag::List(start) => {
                    let style = if start.is_some() {
                        // A supplied start number denotes an ordered list.
                        MarkdownPresentationStyles::ORDERED_LIST
                    } else {
                        // An absent start number denotes an unordered list.
                        MarkdownPresentationStyles::UNORDERED_LIST
                    };
                    push_markdown_decoration(&mut decorations, byte_range.clone(), style)?;
                }
                // 只记录真实Item opening；整List中的lazy正文或代码不得凭行首字符变成marker。
                Tag::Item => {
                    let prefix = prefixes.item(source, byte_range.clone(), &decorations)?;
                    item_ranges.push(byte_range);
                    item_prefixes.push(prefix);
                }
                // A table start owns one local builder and its alignment metadata until the matching end event.
                Tag::Table(alignments) => {
                    push_markdown_decoration(
                        &mut decorations,
                        byte_range.clone(),
                        MarkdownPresentationStyles::TABLE,
                    )?;
                    active_table = Some(MarkdownByteTableBuilder {
                        byte_range,
                        alignments: alignments.to_vec(),
                        header_byte_range: None,
                        rows: Vec::new(),
                        cells: Vec::new(),
                        active_row_index: None,
                    });
                }
                // The header event starts the unique header row and records its cell-index interval.
                Tag::TableHead => {
                    begin_markdown_table_row(
                        source,
                        active_table.as_mut(),
                        byte_range,
                        true,
                        &mut decorations,
                    )?;
                }
                // A body-row event starts a nonheader row in the active table.
                Tag::TableRow => {
                    begin_markdown_table_row(
                        source,
                        active_table.as_mut(),
                        byte_range,
                        false,
                        &mut decorations,
                    )?;
                }
                // Cell events append a source range and its column alignment to the current row.
                Tag::TableCell => add_markdown_table_cell(
                    source,
                    active_table.as_mut(),
                    byte_range,
                    &mut decorations,
                )?,
                // Emphasis tracks paired single delimiters independently of styled content.
                Tag::Emphasis => {
                    add_delimited_inline_structure(
                        source,
                        &byte_range,
                        MarkdownPresentationStyles::EMPHASIS,
                        &["*", "_"],
                        &mut decorations,
                    )?;
                }
                // Strong emphasis tracks paired double delimiters.
                Tag::Strong => {
                    add_delimited_inline_structure(
                        source,
                        &byte_range,
                        MarkdownPresentationStyles::STRONG,
                        &["**", "__"],
                        &mut decorations,
                    )?;
                }
                // 双波浪号保留删除线；单波浪号由启用后的真实Subscript标签单独解释。
                Tag::Strikethrough => {
                    add_delimited_inline_structure(
                        source,
                        &byte_range,
                        MarkdownPresentationStyles::STRIKETHROUGH,
                        &["~~"],
                        &mut decorations,
                    )?;
                }
                // 只消费Parser确认的单波浪号配对；沿原owner/活动选区规则隐藏或显露定界符。
                Tag::Subscript => {
                    add_delimited_inline_structure(
                        source,
                        &byte_range,
                        MarkdownPresentationStyles::SUBSCRIPT,
                        &["~"],
                        &mut decorations,
                    )?;
                }
                // Superscript tracks its paired caret markers.
                Tag::Superscript => {
                    add_delimited_inline_structure(
                        source,
                        &byte_range,
                        MarkdownPresentationStyles::SUPERSCRIPT,
                        &["^"],
                        &mut decorations,
                    )?;
                }
                // Links and images share link styling and editable source-destination marker spans.
                Tag::Link { .. } | Tag::Image { .. } => {
                    push_markdown_decoration(
                        &mut decorations,
                        byte_range.clone(),
                        MarkdownPresentationStyles::LINK,
                    )?;
                    add_link_markers(source, &byte_range, &mut decorations)?;
                }
                // These containers preserve literal source without adding an independent decoration style.
                Tag::FootnoteDefinition(_) => {
                    // GFM脚注真实容器只为后续quote/list前缀提供四列continuation，不把label当marker。
                    prefixes.footnote(byte_range, &decorations)?;
                }
                Tag::Paragraph
                | Tag::HtmlBlock
                | Tag::DefinitionList
                | Tag::DefinitionListTitle
                | Tag::DefinitionListDefinition
                | Tag::MetadataBlock(_) => {}
            },
            // An item end releases its nested task-marker ownership range.
            Event::End(TagEnd::Item) => {
                item_ranges.pop();
                item_prefixes.pop();
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                // 与真实quote Start配平，仅影响后续叶屏障的有限收集。
                prefixes.end_quote()?;
            }
            // Row-end events finish cell accumulation for the active row.
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if let Some(table) = active_table.as_mut() {
                    // Only an active table can own the row state being cleared.
                    table.active_row_index = None;
                }
            }
            // Table end transfers the completed builder into an immutable prototype.
            Event::End(TagEnd::Table) => {
                let table = active_table
                    .take()
                    .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
                table_prototypes.push(finish_markdown_table(source, table, &mut decorations)?);
            }
            // Ordinary text and breaks already inherit their enclosing source-span styles.
            Event::End(_) | Event::SoftBreak | Event::HardBreak => {}
            Event::Text(_) => {
                // 原始Text起点由Parser越过真实容器后授予，只屏蔽该起点所在行的字面内容。
                prefixes.content(byte_range.start, &decorations)?;
            }
            // Inline code receives code styling plus matching backtick delimiter spans.
            Event::Code(_) => {
                prefixes.content(byte_range.start, &decorations)?;
                add_backtick_code_structure(source, &byte_range, &mut decorations)?;
            }
            // Math remains source text with formula styling and tracked dollar delimiters.
            Event::InlineMath(_) | Event::DisplayMath(_) => {
                add_math_structure(source, &byte_range, &mut decorations)?;
            }
            // A thematic break receives rule styling and a source-marker owner.
            Event::Rule => {
                push_markdown_decoration(
                    &mut decorations,
                    byte_range.clone(),
                    MarkdownPresentationStyles::HORIZONTAL_RULE,
                )?;
                push_markdown_marker(&mut decorations, byte_range.clone(), &byte_range)?;
            }
            // Task checkboxes retain visible marker styling and the current item ownership range.
            Event::TaskListMarker(checked) => {
                push_markdown_decoration(
                    &mut decorations,
                    byte_range.clone(),
                    MarkdownPresentationStyles::TASK_LIST,
                )?;
                let item = item_prefixes
                    .last()
                    .copied()
                    .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
                prefixes.task(source, byte_range.clone(), checked, item, &decorations)?;
                let owner = item_ranges.last().unwrap_or(&byte_range);
                push_markdown_visible_marker(&mut decorations, byte_range.clone(), owner)?;
            }
            // Raw HTML stays literal source. A separate exact allowlist below
            // handles <u>; this boundary never generates HTML or executes URLs/scripts.
            // Raw HTML and footnote references remain literal; only exact underline tags receive the separate allowlist pass.
            Event::Html(_) | Event::InlineHtml(_) | Event::FootnoteReference(_) => {
                // HTML与引用字面仍不执行；起点同样阻止把正文中的`>`误作容器前缀。
                prefixes.content(byte_range.start, &decorations)?;
            }
        }
    }

    let list_markers = prefixes.finish(source, &mut decorations)?;
    if local_underline {
        // 完整真实envelope仍保原配对；窗口gap不能拿省略行之后的标签制造新配对。
        add_sanitized_underline_structures(source, &mut decorations, list_markers.len())?;
    }
    if decorations.len() + list_markers.len() > MAX_MARKDOWN_PRESENTATION_DECORATIONS {
        // underline最后补入的记录也参与总预算，超限不能半发布列表glyph或引用隐藏。
        return Err(MarkdownViewportPresentationError::DecorationLimitExceeded);
    }
    markdown_byte_presentation_to_viewport(
        source,
        absolute_utf16_start,
        active_selection,
        decorations,
        table_prototypes,
        list_markers,
    )
}

/// Map each parser heading level onto its stable presentation bit.
pub(super) fn markdown_heading_style(level: HeadingLevel) -> MarkdownPresentationStyles {
    match level {
        // The largest heading uses the level-one style.
        HeadingLevel::H1 => MarkdownPresentationStyles::HEADING_1,
        // The second heading level retains a distinct style bit.
        HeadingLevel::H2 => MarkdownPresentationStyles::HEADING_2,
        // The third heading level retains a distinct style bit.
        HeadingLevel::H3 => MarkdownPresentationStyles::HEADING_3,
        // The fourth heading level retains a distinct style bit.
        HeadingLevel::H4 => MarkdownPresentationStyles::HEADING_4,
        // The fifth heading level retains a distinct style bit.
        HeadingLevel::H5 => MarkdownPresentationStyles::HEADING_5,
        // The sixth heading level retains a distinct style bit.
        HeadingLevel::H6 => MarkdownPresentationStyles::HEADING_6,
    }
}

/// Append a nonempty byte decoration while enforcing the per-envelope decoration budget.
pub(super) fn push_markdown_decoration(
    decorations: &mut Vec<MarkdownByteDecoration>,
    byte_range: Range<usize>,
    styles: MarkdownPresentationStyles,
) -> Result<(), MarkdownViewportPresentationError> {
    if byte_range.start >= byte_range.end || styles.is_empty() {
        // Empty ranges and empty styles cannot produce a visible presentation run.
        return Ok(());
    }
    if decorations.len() >= MAX_MARKDOWN_PRESENTATION_DECORATIONS {
        // An excessive decoration count triggers source fallback rather than unbounded viewport work.
        return Err(MarkdownViewportPresentationError::DecorationLimitExceeded);
    }
    decorations.push(MarkdownByteDecoration {
        byte_range,
        styles,
        marker_owner: None,
    });
    Ok(())
}

/// Record an owned marker that may hide only while its complete structure is inactive.
pub(super) fn push_markdown_marker(
    decorations: &mut Vec<MarkdownByteDecoration>,
    marker_range: Range<usize>,
    owner_range: &Range<usize>,
) -> Result<(), MarkdownViewportPresentationError> {
    if marker_range.start >= marker_range.end {
        // Empty marker spans have nothing to hide or expose.
        return Ok(());
    }
    if owner_range.start > owner_range.end
        || marker_range.start < owner_range.start
        || marker_range.end > owner_range.end
    {
        // A marker outside its owner indicates an invalid parser/source boundary.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    }
    if decorations.len() >= MAX_MARKDOWN_PRESENTATION_DECORATIONS {
        // Hidden-marker metadata shares the same bounded decoration budget as ordinary styles.
        return Err(MarkdownViewportPresentationError::DecorationLimitExceeded);
    }
    decorations.push(MarkdownByteDecoration {
        byte_range: marker_range,
        styles: MarkdownPresentationStyles::MARKER,
        marker_owner: Some(super::prefixes::MarkdownMarkerOwner::Structure(
            owner_range.clone(),
        )),
    });
    Ok(())
}

/// Validate marker ownership while retaining marker visibility for native list and task rendering.
pub(super) fn push_markdown_visible_marker(
    decorations: &mut Vec<MarkdownByteDecoration>,
    marker_range: Range<usize>,
    owner_range: &Range<usize>,
) -> Result<(), MarkdownViewportPresentationError> {
    if marker_range.start >= marker_range.end {
        // Empty marker spans produce no decoration.
        return Ok(());
    }
    if owner_range.start > owner_range.end
        || marker_range.start < owner_range.start
        || marker_range.end > owner_range.end
    {
        // A marker cannot extend beyond the source structure responsible for it.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    }
    push_markdown_decoration(
        decorations,
        marker_range,
        MarkdownPresentationStyles::MARKER,
    )
}

/// 判断选区是否激活原结构 owner；真实有限语义与旧投影共用同一显隐规则。
pub(crate) fn markdown_selection_intersects(
    selection: &Range<usize>,
    structure: &Range<usize>,
) -> bool {
    if selection.start == selection.end {
        // A collapsed caret activates either boundary of a structure so its syntax remains editable.
        structure.start <= selection.start && selection.start <= structure.end
    } else {
        // A nonempty selection activates structures with half-open overlap.
        selection.start < structure.end && structure.start < selection.end
    }
}

/// Style a parser-confirmed inline span and track recognized symmetric delimiter pairs.
pub(super) fn add_delimited_inline_structure(
    source: &str,
    byte_range: &Range<usize>,
    style: MarkdownPresentationStyles,
    delimiters: &[&str],
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    push_markdown_decoration(decorations, byte_range.clone(), style)?;
    let Some(fragment) = source.get(byte_range.clone()) else {
        // Parser offsets must describe a scalar-aligned slice of the supplied bounded source.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    };
    let Some(delimiter) = delimiters.iter().find(|delimiter| {
        // Only a symmetric recognized source pair may receive hidden delimiter ranges.
        fragment.starts_with(**delimiter) && fragment.ends_with(**delimiter)
    }) else {
        // A parser-confirmed span without a recognized delimiter pair keeps literal syntax visible.
        return Ok(());
    };
    let delimiter_bytes = delimiter.len();
    if fragment.len() < delimiter_bytes.saturating_mul(2) {
        // An incomplete delimiter pair cannot safely produce hidden marker ranges; retain its content style only.
        return Ok(());
    }
    push_markdown_marker(
        decorations,
        byte_range.start..byte_range.start + delimiter_bytes,
        byte_range,
    )?;
    push_markdown_marker(
        decorations,
        byte_range.end - delimiter_bytes..byte_range.end,
        byte_range,
    )
}

/// Track inline-code styling and complete matching backtick runs without changing source bytes.
pub(super) fn add_backtick_code_structure(
    source: &str,
    byte_range: &Range<usize>,
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    push_markdown_decoration(
        decorations,
        byte_range.clone(),
        MarkdownPresentationStyles::INLINE_CODE,
    )?;
    let Some(fragment) = source.get(byte_range.clone()) else {
        // Parser offsets must describe a scalar-aligned slice of the supplied bounded source.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    };
    let delimiter_bytes = fragment
        .bytes()
        .take_while(|byte| {
            // The opening backtick run sets the required width of its closing run.
            *byte == b'`'
        })
        .count();
    if delimiter_bytes == 0
        || fragment.len() < delimiter_bytes.saturating_mul(2)
        || !fragment.ends_with(&"`".repeat(delimiter_bytes))
    {
        // Unmatched or incomplete delimiter runs remain visible source rather than fabricated marker spans.
        return Ok(());
    }
    push_markdown_marker(
        decorations,
        byte_range.start..byte_range.start + delimiter_bytes,
        byte_range,
    )?;
    push_markdown_marker(
        decorations,
        byte_range.end - delimiter_bytes..byte_range.end,
        byte_range,
    )
}

/// Track formula styling and complete matching dollar delimiters as literal editable source.
pub(super) fn add_math_structure(
    source: &str,
    byte_range: &Range<usize>,
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    push_markdown_decoration(
        decorations,
        byte_range.clone(),
        MarkdownPresentationStyles::FORMULA,
    )?;
    let Some(fragment) = source.get(byte_range.clone()) else {
        // Parser offsets must describe a scalar-aligned slice of the supplied bounded source.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    };
    let delimiter_bytes = fragment
        .bytes()
        .take_while(|byte| {
            // The opening dollar run determines the formula delimiter width.
            *byte == b'$'
        })
        .count();
    if delimiter_bytes == 0
        || fragment.len() < delimiter_bytes.saturating_mul(2)
        || !fragment.ends_with(&"$".repeat(delimiter_bytes))
    {
        // Without a complete matching dollar pair only the formula style is retained.
        return Ok(());
    }
    push_markdown_marker(
        decorations,
        byte_range.start..byte_range.start + delimiter_bytes,
        byte_range,
    )?;
    push_markdown_marker(
        decorations,
        byte_range.end - delimiter_bytes..byte_range.end,
        byte_range,
    )
}

/// Track autolink or bracket-link source syntax while leaving label text visible.
pub(super) fn add_link_markers(
    source: &str,
    byte_range: &Range<usize>,
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    let Some(fragment) = source.get(byte_range.clone()) else {
        // Parser offsets must describe a scalar-aligned slice of the supplied bounded source.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    };
    if fragment.starts_with('<') && fragment.ends_with('>') {
        // Autolinks own only their enclosing angle-bracket markers.
        push_markdown_marker(
            decorations,
            byte_range.start..byte_range.start + 1,
            byte_range,
        )?;
        return push_markdown_marker(decorations, byte_range.end - 1..byte_range.end, byte_range);
    }

    let Some(open_label) = fragment.find('[') else {
        // Unsupported link source forms retain their link style with all syntax visible.
        return Ok(());
    };
    let label_source = &fragment[open_label + 1..];
    let Some(close_label_relative) = label_source.find(']') else {
        // An incomplete label cannot define a safe source marker range.
        return Ok(());
    };
    let close_label = open_label + 1 + close_label_relative;
    push_markdown_marker(
        decorations,
        byte_range.start..byte_range.start + open_label + 1,
        byte_range,
    )?;
    push_markdown_marker(
        decorations,
        byte_range.start + close_label..byte_range.end,
        byte_range,
    )
}

/// Track ATX prefixes and closers or setext underline rows within a parser-confirmed heading.
pub(super) fn add_heading_markers(
    source: &str,
    byte_range: &Range<usize>,
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    let Some(fragment) = source.get(byte_range.clone()) else {
        // Parser offsets must describe a scalar-aligned slice of the supplied bounded source.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    };
    let first_line_end = fragment.find('\n').unwrap_or(fragment.len());
    let first_line = &fragment[..first_line_end];
    let indentation = first_line
        .bytes()
        .take_while(|byte| {
            // Keep only leading ASCII spaces as heading indentation.
            *byte == b' '
        })
        .count();
    let marker_length = first_line[indentation..]
        .bytes()
        .take_while(|byte| {
            // The initial contiguous hash run determines the ATX source-marker width.
            *byte == b'#'
        })
        .count();
    if marker_length != 0 {
        // An ATX heading has a leading hash run and may have a whitespace-separated closing run.
        let mut prefix_end = indentation + marker_length;
        if first_line.as_bytes().get(prefix_end) == Some(&b' ') {
            // Include one separator space with the leading marker so hidden syntax leaves no stray gap.
            prefix_end += 1;
        }
        push_markdown_marker(
            decorations,
            byte_range.start + indentation..byte_range.start + prefix_end,
            byte_range,
        )?;

        let mut tail =
            HeadingMarkerTail::new(byte_range.start, byte_range.start + first_line.len());
        for &byte in first_line.as_bytes().iter().rev() {
            tail.observe_reverse(byte);
            if tail.done() {
                break; // 与有限来源共用原tail规则，只扫描实际决定closing显示范围的尾段。
            }
        }
        if let Some(marker) = tail.marker() {
            push_markdown_marker(decorations, marker, byte_range)?;
        }
        return Ok(());
    }

    let mut line_start = 0;
    for line in fragment.split_inclusive('\n') {
        // A non-ATX heading may contain a setext underline on a subsequent source line.
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        let trimmed = line_without_newline.trim();
        if !trimmed.is_empty()
            && (trimmed.bytes().all(|byte| {
                // A setext underline row must contain only its single marker family.
                byte == b'='
            }) || trimmed.bytes().all(|byte| {
                // Every remaining delimiter byte must be a hyphen; other content would invalidate the table row.
                byte == b'-'
            }))
        {
            // A nonempty row made entirely of one supported underline marker is hidden with its heading owner.
            let marker_start = line_start + line_without_newline.find(trimmed).unwrap_or(0);
            push_markdown_marker(
                decorations,
                byte_range.start + marker_start..byte_range.start + line_start + line.len(),
                byte_range,
            )?;
        }
        line_start += line.len();
    }
    Ok(())
}

#[cfg(feature = "s2-table-window-prototype")]
#[derive(Clone, Copy)]
/// Prefix grammar selected from a parser-confirmed block container.
pub(super) enum MarkdownLineMarkerKind {
    BlockQuote,
    UnorderedList,
    OrderedList,
}

#[cfg(feature = "s2-table-window-prototype")]
/// 已验证单个物理行复用既有marker识别；只返回当前真实byte range，不携带text或AST。
pub(crate) fn table_line_prefix_marker(
    line: &str,
    local_start: usize,
    kind: pulldown_cmark::block_frontier::TableScopeKind,
) -> Result<Option<Range<usize>>, MarkdownViewportPresentationError> {
    use pulldown_cmark::block_frontier::TableScopeKind;
    let marker = match kind {
        TableScopeKind::BlockQuote => MarkdownLineMarkerKind::BlockQuote,
        TableScopeKind::OrderedList => MarkdownLineMarkerKind::OrderedList,
        TableScopeKind::UnorderedList => MarkdownLineMarkerKind::UnorderedList,
        TableScopeKind::Other => return Ok(None),
    };
    if line.contains(['\r', '\n']) {
        // 此窄入口只接受已验证container prefix，不借它重新扫描整个container或cell正文。
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    }
    let suffix = line
        .get(local_start..)
        .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
    Ok(markdown_line_prefix_marker_range(suffix, marker)
        .map(|range| local_start + range.start..local_start + range.end))
}

#[cfg(feature = "s2-table-window-prototype")]
/// Return a complete numeric-list prefix width or zero for ordinary source text.
pub(super) fn ordered_list_marker_length(content: &str) -> usize {
    let digits = content.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0
        || !matches!(content.as_bytes().get(digits), Some(b'.' | b')'))
        || !content
            .as_bytes()
            .get(digits + 1)
            .is_some_and(u8::is_ascii_whitespace)
    {
        // A missing number, terminator, or separator prevents source text from becoming a list marker.
        0
    } else {
        // A complete numeric prefix includes the terminator and one separator byte.
        digits + 2
    }
}

/// Decorate only the first and last fence-bearing lines of a parser-confirmed code block.
pub(super) fn add_fenced_code_markers(
    source: &str,
    byte_range: &Range<usize>,
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    let Some(fragment) = source.get(byte_range.clone()) else {
        // Parser offsets must describe a scalar-aligned slice of the supplied bounded source.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    };
    let mut line_ranges = Vec::new();
    let mut line_start = 0;
    for line in fragment.split_inclusive('\n') {
        // Collect line byte spans within this already bounded source envelope.
        line_ranges.push(line_start..line_start + line.len());
        line_start += line.len();
    }
    if line_start < fragment.len() {
        // Retain an unterminated tail line if line splitting left one unrepresented.
        line_ranges.push(line_start..fragment.len());
    }
    for line_range in line_ranges.first().into_iter().chain(line_ranges.last()) {
        // Only boundary lines can serve as the opening or closing fence markers.
        let line = &fragment[line_range.clone()];
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            // A recognized backtick or tilde boundary line receives the full line marker range.
            push_markdown_marker(
                decorations,
                byte_range.start + line_range.start..byte_range.start + line_range.end,
                byte_range,
            )?;
        }
    }
    Ok(())
}

/// Begin a validated table row and link its future cells through an index interval.
pub(super) fn begin_markdown_table_row(
    source: &str,
    table: Option<&mut MarkdownByteTableBuilder>,
    byte_range: Range<usize>,
    is_header: bool,
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    let table = table.ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
    let row_range = markdown_table_line_content_range(source, byte_range)?;
    if is_header && table.header_byte_range.replace(row_range.clone()).is_some() {
        // A second header event violates the single-header table contract.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    }
    let row_index = table.rows.len();
    let cell_index = table.cells.len();
    table.rows.push(MarkdownByteTableRowPrototype {
        byte_range: row_range.clone(),
        cell_index_range: cell_index..cell_index,
        is_header,
    });
    table.active_row_index = Some(row_index);
    let styles = if is_header {
        // Header rows receive both the header role and native row-placeholder style.
        MarkdownPresentationStyles::TABLE_HEADER
            .union(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER)
    } else {
        // Body rows receive the native row-placeholder style only.
        MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER
    };
    push_markdown_decoration(decorations, row_range, styles)
}

/// Append one trimmed cell source range and its resolved column alignment to the active row.
pub(super) fn add_markdown_table_cell(
    source: &str,
    table: Option<&mut MarkdownByteTableBuilder>,
    byte_range: Range<usize>,
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    let table = table.ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
    let row_index = table
        .active_row_index
        .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
    let row = table
        .rows
        .get_mut(row_index)
        .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
    let column_index = row.cell_index_range.len();
    let alignment = table
        .alignments
        .get(column_index)
        .copied()
        .unwrap_or(Alignment::None);
    let byte_range = trim_markdown_table_cell_range(source, byte_range)?;
    table.cells.push(MarkdownByteTableCellPrototype {
        byte_range: byte_range.clone(),
        column_index,
        alignment: markdown_table_alignment(alignment),
    });
    row.cell_index_range.end = table.cells.len();
    push_markdown_decoration(
        decorations,
        byte_range,
        markdown_table_alignment_style(alignment),
    )
}

/// Consume the table builder after validating its header and recording delimiter and boundary markers.
pub(super) fn finish_markdown_table(
    source: &str,
    table: MarkdownByteTableBuilder,
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<MarkdownByteTablePrototype, MarkdownViewportPresentationError> {
    let header_byte_range = table
        .header_byte_range
        .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
    let delimiter_byte_range = markdown_table_delimiter_byte_range(source, &table.byte_range)?;
    push_markdown_decoration(
        decorations,
        delimiter_byte_range.clone(),
        MarkdownPresentationStyles::TABLE_DELIMITER
            .union(MarkdownPresentationStyles::HIDDEN_MARKER),
    )?;
    add_markdown_table_boundary_markers(source, &table.byte_range, &table.cells, decorations)?;
    Ok(MarkdownByteTablePrototype {
        byte_range: table.byte_range,
        header_byte_range,
        delimiter_byte_range,
        rows: table.rows,
        cells: table.cells,
    })
}

#[cfg(feature = "s2-table-window-prototype")]
/// 原有quote/list标记判定的无分配共享内核；完整Parser和窗口prefix必须返回相同byte范围。
fn markdown_line_prefix_marker_range(
    line: &str,
    marker_kind: MarkdownLineMarkerKind,
) -> Option<Range<usize>> {
    let line_without_newline = line.trim_end_matches(['\r', '\n']);
    let indentation = line_without_newline
        .bytes()
        .take_while(|byte| matches!(*byte, b' ' | b'\t'))
        .count();
    let content = &line_without_newline[indentation..];
    let marker_length = match marker_kind {
        // 引用只拥有真实'>'和至多一个分隔空格，不把后续正文吞成marker。
        MarkdownLineMarkerKind::BlockQuote => content
            .strip_prefix('>')
            .map(|rest| {
                // 与完整源Parser保持同一个可见marker长度规则。
                1 + usize::from(rest.starts_with(' '))
            })
            .unwrap_or(0),
        // 无序列表必须同时存在受支持的marker和其后的分隔空白。
        MarkdownLineMarkerKind::UnorderedList => content
            .as_bytes()
            .first()
            .filter(|marker| {
                // 其他标点不能借容器身份升级成列表marker。
                matches!(marker, b'-' | b'+' | b'*')
            })
            .and_then(|_| {
                // 行尾孤立marker没有分隔字节，因此不发布可见范围。
                content.as_bytes().get(1)
            })
            .filter(|separator| {
                // 分隔条件仍复用原ASCII规则，不另造投影专用列表语法。
                separator.is_ascii_whitespace()
            })
            .map_or(0, |_| {
                // 已验证前缀只拥有marker及第一个分隔字节。
                2
            }),
        // 有序列表沿用原数值前缀校验；不从scope ordinal猜写数字宽度。
        MarkdownLineMarkerKind::OrderedList => ordered_list_marker_length(content),
    };
    (marker_length != 0).then_some(indentation..indentation + marker_length)
}

/// Map parser alignment into presentation bits used by native table cells.
pub(super) fn markdown_table_alignment_style(alignment: Alignment) -> MarkdownPresentationStyles {
    match alignment {
        // Unspecified alignment follows the same left default as explicit left alignment.
        Alignment::None | Alignment::Left => MarkdownPresentationStyles::TABLE_ALIGN_LEFT,
        // Center alignment receives its distinct style bit.
        Alignment::Center => MarkdownPresentationStyles::TABLE_ALIGN_CENTER,
        // Right alignment receives its distinct style bit.
        Alignment::Right => MarkdownPresentationStyles::TABLE_ALIGN_RIGHT,
    }
}

/// Map parser alignment into the stable table prototype contract.
pub(super) fn markdown_table_alignment(alignment: Alignment) -> MarkdownTableAlignment {
    match alignment {
        // Unspecified or left alignment becomes the left default.
        Alignment::None | Alignment::Left => MarkdownTableAlignment::Left,
        // Explicit center alignment remains centered.
        Alignment::Center => MarkdownTableAlignment::Center,
        // Explicit right alignment remains right aligned.
        Alignment::Right => MarkdownTableAlignment::Right,
    }
}

/// Validate a source row range and exclude CR/LF bytes from its content span.
pub(super) fn markdown_table_line_content_range(
    source: &str,
    mut range: Range<usize>,
) -> Result<Range<usize>, MarkdownViewportPresentationError> {
    let bytes = source.as_bytes();
    if range.start > range.end || range.end > bytes.len() {
        // Invalid parser extents must be rejected before byte indexing.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    }
    while range.start < range.end
        && bytes.get(range.end - 1).is_some_and(|byte| {
            // Exclude only source line-ending bytes from the row content range.
            matches!(byte, b'\r' | b'\n')
        })
    {
        // Remove only trailing line-ending bytes, leaving all cell content unchanged.
        range.end -= 1;
    }
    Ok(range)
}

/// Locate the second source line required by a parser-confirmed table header.
pub(super) fn markdown_table_delimiter_byte_range(
    source: &str,
    table_range: &Range<usize>,
) -> Result<Range<usize>, MarkdownViewportPresentationError> {
    let fragment = source
        .get(table_range.clone())
        .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
    let mut line_start = 0;
    for (line_index, line) in fragment.split_inclusive('\n').enumerate() {
        // Inspect source lines in order; only the delimiter row is retained.
        if line_index == 1 {
            // The second line is the delimiter and excludes its CR/LF suffix.
            let content_length = line.trim_end_matches(['\r', '\n']).len();
            return Ok(
                table_range.start + line_start..table_range.start + line_start + content_length
            );
        }
        line_start += line.len();
    }
    Err(MarkdownViewportPresentationError::InvalidParserBoundary)
}

/// Decorate structural pipes outside parser-confirmed cell content ranges.
pub(super) fn add_markdown_table_boundary_markers(
    source: &str,
    table_range: &Range<usize>,
    cells: &[MarkdownByteTableCellPrototype],
    decorations: &mut Vec<MarkdownByteDecoration>,
) -> Result<(), MarkdownViewportPresentationError> {
    let fragment = source
        .get(table_range.clone())
        .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
    let mut cell_index = 0;
    for (relative_offset, byte) in fragment.bytes().enumerate() {
        // Inspect table bytes once while advancing a monotonic cell cursor.
        if byte != b'|' {
            // Only pipe bytes can become table boundaries.
            continue;
        }
        let absolute_offset = table_range.start + relative_offset;
        while cells.get(cell_index).is_some_and(|cell| {
            // Advance past parser cells that cannot contain this structural-pipe candidate.
            cell.byte_range.end <= absolute_offset
        }) {
            // Discard cells ending before this pipe so the current candidate can be tested in constant amortized time.
            cell_index += 1;
        }
        if cells.get(cell_index).is_some_and(|cell| {
            // A pipe inside authoritative parser cell content must remain literal.
            cell.byte_range.contains(&absolute_offset)
        }) {
            // Pipes inside cell source belong to literal or escaped content and must remain untouched.
            continue;
        }
        push_markdown_decoration(
            decorations,
            absolute_offset..absolute_offset + 1,
            MarkdownPresentationStyles::TABLE_BOUNDARY
                .union(MarkdownPresentationStyles::MARKER)
                .union(MarkdownPresentationStyles::HIDDEN_MARKER),
        )?;
    }
    Ok(())
}

/// Validate a parser cell extent and remove only surrounding ASCII horizontal whitespace.
pub(super) fn trim_markdown_table_cell_range(
    source: &str,
    mut range: Range<usize>,
) -> Result<Range<usize>, MarkdownViewportPresentationError> {
    let bytes = source.as_bytes();
    if range.start > range.end || range.end > bytes.len() {
        // Reject invalid source extents before trimming.
        return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
    }
    while range.start < range.end
        && bytes.get(range.start).is_some_and(|byte| {
            // Only ASCII horizontal whitespace belongs to this Markdown grammar boundary.
            matches!(byte, b' ' | b'\t')
        })
    {
        // Leading spaces and tabs are table padding rather than cell content.
        range.start += 1;
    }
    while range.start < range.end
        && bytes.get(range.end - 1).is_some_and(|byte| {
            // Only ASCII horizontal whitespace belongs to this Markdown grammar boundary.
            matches!(byte, b' ' | b'\t')
        })
    {
        // Trailing spaces and tabs are likewise excluded without altering internal whitespace.
        range.end -= 1;
    }
    Ok(range)
}

/// 只识别真实allowlist的成对underline，并为已完成列表token预留同一记录预算。
pub(super) fn add_sanitized_underline_structures(
    source: &str,
    decorations: &mut Vec<MarkdownByteDecoration>,
    reserved_list_markers: usize,
) -> Result<(), MarkdownViewportPresentationError> {
    let mut scanner = super::underline::SanitizedUnderlineScanner::default();
    for byte in source.bytes() {
        // 普通有限envelope与新table context producer共用真实first-open→next-close状态，不在gap重配对。
        let Some(pair) = scanner.push(byte) else {
            continue;
        };
        let additional = 2 + usize::from(pair.content_start < pair.close_start);
        if decorations.len() + reserved_list_markers + additional
            > MAX_MARKDOWN_PRESENTATION_DECORATIONS
        {
            // 两个delimiter与非空内容style在任何push前整体准入，不能先越过共同8192硬界再拒绝。
            return Err(MarkdownViewportPresentationError::DecorationLimitExceeded);
        }
        let structure = pair.open_start..pair.end;
        push_markdown_decoration(
            decorations,
            pair.content_start..pair.close_start,
            MarkdownPresentationStyles::UNDERLINE,
        )?;
        push_markdown_marker(decorations, pair.open_start..pair.content_start, &structure)?;
        push_markdown_marker(decorations, pair.close_start..pair.end, &structure)?;
    }
    Ok(())
}

/// Project byte decorations and tables through one UTF-16 boundary pass, then sweep style transitions.
pub(super) fn markdown_byte_presentation_to_viewport(
    source: &str,
    absolute_utf16_start: usize,
    active_selection: &Range<usize>,
    decorations: Vec<MarkdownByteDecoration>,
    table_prototypes: Vec<MarkdownByteTablePrototype>,
    list_markers: Vec<super::prefixes::MarkdownByteListMarker>,
) -> Result<MarkdownPresentationBuild, MarkdownViewportPresentationError> {
    // Collect every distinct source endpoint before decoding Unicode; allocation stays viewport-bounded.
    let table_boundary_capacity = table_prototypes.iter().fold(0_usize, |total, table| {
        // Reserve endpoint capacity from bounded table metadata without touching source bytes.
        total
            .saturating_add(6)
            .saturating_add(table.rows.len().saturating_mul(2))
            .saturating_add(table.cells.len().saturating_mul(2))
    });
    let mut byte_boundaries = Vec::with_capacity(
        decorations
            .len()
            .saturating_mul(4)
            .saturating_add(list_markers.len().saturating_mul(4))
            .saturating_add(table_boundary_capacity),
    );
    for decoration in &decorations {
        // Collect both endpoints of every style span for the single coordinate translation pass.
        byte_boundaries.push(decoration.byte_range.start);
        byte_boundaries.push(decoration.byte_range.end);
        if let Some(owner) = &decoration.marker_owner {
            let owner_range = owner.range();
            // Marker owners need their own endpoints to decide selection-dependent visibility.
            byte_boundaries.push(owner_range.start);
            byte_boundaries.push(owner_range.end);
        }
    }
    for marker in &list_markers {
        // token与真实item owner共用同一坐标表，不重复解码前缀或持有第二份正文。
        byte_boundaries.extend([
            marker.range.start,
            marker.range.end,
            marker.owner.start,
            marker.owner.end,
        ]);
    }
    for table in &table_prototypes {
        // Include all table, header, delimiter, row, and cell boundaries in the same conversion set.
        byte_boundaries.extend([
            table.byte_range.start,
            table.byte_range.end,
            table.header_byte_range.start,
            table.header_byte_range.end,
            table.delimiter_byte_range.start,
            table.delimiter_byte_range.end,
        ]);
        for row in &table.rows {
            // Each row needs two endpoints while retaining its existing cell-index interval.
            byte_boundaries.extend([row.byte_range.start, row.byte_range.end]);
        }
        for cell in &table.cells {
            // Each cell needs two endpoints while retaining its column and alignment metadata.
            byte_boundaries.extend([cell.byte_range.start, cell.byte_range.end]);
        }
    }
    byte_boundaries.sort_unstable();
    byte_boundaries.dedup();
    let utf16_boundaries = markdown_utf16_boundaries(source, &byte_boundaries)?;

    // Project decorations once, then encode overlapping styles as balanced occurrence transitions.
    let mut transitions = Vec::with_capacity(decorations.len().saturating_mul(4));
    for decoration in decorations {
        // Consume decorations into style transitions after the shared coordinate map is complete.
        let mut styles = decoration.styles;
        if let Some(owner) = &decoration.marker_owner {
            let owner_range = owner.range();
            // Only marker decorations with an owner have selection-dependent hiding behavior.
            let owner_start_index = byte_boundaries
                .binary_search(&owner_range.start)
                .expect("Markdown marker owner start boundary was collected");
            let owner_end_index = byte_boundaries
                .binary_search(&owner_range.end)
                .expect("Markdown marker owner end boundary was collected");
            let owner_utf16_range = absolute_utf16_start + utf16_boundaries[owner_start_index]
                ..absolute_utf16_start + utf16_boundaries[owner_end_index];
            if !owner.is_active(active_selection, &owner_utf16_range) {
                // An inactive owning structure hides syntax while preserving source coordinates.
                styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
            }
        }
        let start_index = byte_boundaries
            .binary_search(&decoration.byte_range.start)
            .expect("Markdown decoration start boundary was collected");
        let end_index = byte_boundaries
            .binary_search(&decoration.byte_range.end)
            .expect("Markdown decoration end boundary was collected");
        let start = absolute_utf16_start + utf16_boundaries[start_index];
        let end = absolute_utf16_start + utf16_boundaries[end_index];
        if start >= end {
            // Empty projected spans cannot contribute a style interval.
            continue;
        }
        for style_bit in styles.set_bits() {
            // Emit a start and end transition for each active bit, preserving nested occurrences by counts.
            transitions.push(MarkdownStyleTransition {
                utf16_offset: start,
                style_bit,
                delta: 1,
            });
            transitions.push(MarkdownStyleTransition {
                utf16_offset: end,
                style_bit,
                delta: -1,
            });
        }
    }
    transitions.sort_unstable_by_key(|transition| {
        // Group style transitions solely by document position so all coincident events are applied together.
        transition.utf16_offset
    });

    // A sorted sweep converts overlap counts into disjoint runs without per-character attributes.
    let mut runs: Vec<MarkdownPresentationRun> = Vec::new();
    let mut style_counts = [0_u16; u32::BITS as usize];
    let mut transition_index = 0;
    let mut segment_start = absolute_utf16_start;
    while transition_index < transitions.len() {
        // Sweep ordered boundary groups, carrying only per-style nesting counts between positions.
        let transition_offset = transitions[transition_index].utf16_offset;
        let styles = MarkdownPresentationStyles::from_presence_counts(&style_counts);
        if segment_start < transition_offset && !styles.is_empty() {
            // A positive-width styled segment becomes a run before applying transitions at its end.
            if let Some(previous) = runs.last_mut()
                && previous.utf16_range.end == segment_start
                && previous.styles == styles
            {
                // Adjacent segments with identical styles merge into one run.
                previous.utf16_range.end = transition_offset;
            } else {
                // A distinct segment starts a new disjoint presentation run.
                runs.push(MarkdownPresentationRun {
                    utf16_range: segment_start..transition_offset,
                    styles,
                });
            }
        }

        while transition_index < transitions.len()
            && transitions[transition_index].utf16_offset == transition_offset
        {
            // Apply every transition at this coordinate before beginning the next segment.
            let transition = transitions[transition_index];
            let count = &mut style_counts[transition.style_bit];
            if transition.delta > 0 {
                // A positive transition enters one occurrence of this style.
                *count = count.saturating_add(1);
            } else {
                // A negative transition exits one occurrence without underflowing the presence counter.
                *count = count.saturating_sub(1);
            }
            transition_index += 1;
        }
        segment_start = transition_offset;
    }
    let mut tables = Vec::with_capacity(table_prototypes.len());
    for table in table_prototypes {
        // Transfer table metadata into absolute UTF-16 prototypes using the same collected boundaries.
        let mut rows = Vec::with_capacity(table.rows.len());
        for row in table.rows {
            // Rows preserve their header role and cell-index intervals while translating coordinates.
            rows.push(MarkdownTableRowPrototype {
                utf16_range: markdown_absolute_utf16_range(
                    &row.byte_range,
                    absolute_utf16_start,
                    &byte_boundaries,
                    &utf16_boundaries,
                ),
                cell_index_range: row.cell_index_range,
                is_header: row.is_header,
            });
        }
        let mut cells = Vec::with_capacity(table.cells.len());
        for cell in table.cells {
            // Cells preserve their column alignment and source order while translating coordinates.
            cells.push(MarkdownTableCellPrototype {
                utf16_range: markdown_absolute_utf16_range(
                    &cell.byte_range,
                    absolute_utf16_start,
                    &byte_boundaries,
                    &utf16_boundaries,
                ),
                column_index: cell.column_index,
                alignment: cell.alignment,
            });
        }
        tables.push(MarkdownTablePrototype {
            utf16_range: markdown_absolute_utf16_range(
                &table.byte_range,
                absolute_utf16_start,
                &byte_boundaries,
                &utf16_boundaries,
            ),
            header_utf16_range: markdown_absolute_utf16_range(
                &table.header_byte_range,
                absolute_utf16_start,
                &byte_boundaries,
                &utf16_boundaries,
            ),
            delimiter_utf16_range: markdown_absolute_utf16_range(
                &table.delimiter_byte_range,
                absolute_utf16_start,
                &byte_boundaries,
                &utf16_boundaries,
            ),
            rows,
            cells,
        });
    }
    let list_markers = list_markers
        .into_iter()
        .map(|marker| {
            // item活动域仅看原选区两个端点；祖先Item自然包含嵌套端点，中间跨过的兄弟不误活动。
            let owner_utf16_range = markdown_absolute_utf16_range(
                &marker.owner,
                absolute_utf16_start,
                &byte_boundaries,
                &utf16_boundaries,
            );
            let active = [active_selection.start, active_selection.end]
                .into_iter()
                .any(|position| {
                    owner_utf16_range.start <= position && position <= owner_utf16_range.end
                });
            MarkdownPreviewListMarker {
                utf16_range: markdown_absolute_utf16_range(
                    &marker.range,
                    absolute_utf16_start,
                    &byte_boundaries,
                    &utf16_boundaries,
                ),
                owner_utf16_range,
                kind: marker.kind,
                active,
            }
        })
        .collect();
    Ok(MarkdownPresentationBuild {
        runs,
        tables,
        list_markers,
    })
}

/// Translate a previously collected byte range through the validated shared boundary map.
pub(super) fn markdown_absolute_utf16_range(
    byte_range: &Range<usize>,
    absolute_utf16_start: usize,
    byte_boundaries: &[usize],
    utf16_boundaries: &[usize],
) -> Range<usize> {
    let start_index = byte_boundaries
        .binary_search(&byte_range.start)
        .expect("Markdown prototype start boundary was collected");
    let end_index = byte_boundaries
        .binary_search(&byte_range.end)
        .expect("Markdown prototype end boundary was collected");
    absolute_utf16_start + utf16_boundaries[start_index]
        ..absolute_utf16_start + utf16_boundaries[end_index]
}

/// Convert sorted byte boundaries in one scalar pass and reject any boundary inside UTF-8 encoding.
pub(super) fn markdown_utf16_boundaries(
    source: &str,
    byte_boundaries: &[usize],
) -> Result<Vec<usize>, MarkdownViewportPresentationError> {
    let mut utf16_boundaries = Vec::with_capacity(byte_boundaries.len());
    let mut boundary_index = 0;
    let mut utf16_offset = 0;
    for (byte_offset, character) in source.char_indices() {
        // Advance source scalars once while fulfilling all equal requested byte positions.
        while byte_boundaries.get(boundary_index) == Some(&byte_offset) {
            // Every boundary at this scalar start receives the same current UTF-16 coordinate.
            utf16_boundaries.push(utf16_offset);
            boundary_index += 1;
        }
        if byte_boundaries.get(boundary_index).is_some_and(|boundary| {
            // A requested byte endpoint before this scalar ends would divide its UTF-8 encoding.
            *boundary < byte_offset + character.len_utf8()
        }) {
            // A boundary before the scalar end but after its start would divide its UTF-8 encoding.
            return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
        }
        utf16_offset += character.len_utf16();
    }
    while byte_boundaries.get(boundary_index) == Some(&source.len()) {
        // The end-of-source boundary receives the complete UTF-16 length, including duplicate requests.
        utf16_boundaries.push(utf16_offset);
        boundary_index += 1;
    }
    if boundary_index == byte_boundaries.len() {
        // Every requested boundary was represented by a real scalar boundary.
        Ok(utf16_boundaries)
    } else {
        // Any unconsumed boundary lies outside the source and invalidates the projection.
        Err(MarkdownViewportPresentationError::InvalidParserBoundary)
    }
}
