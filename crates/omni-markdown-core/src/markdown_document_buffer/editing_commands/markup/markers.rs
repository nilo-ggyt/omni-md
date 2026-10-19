//! Exact container-marker, interruption, thematic-break and fence grammar without copied source lines.

use super::context::Marker;
use super::foreign_blocks::html_start;
use super::*;

/// Read a list/quote marker at a container-relative boundary using scalar lookahead only.
pub(super) fn marker_at(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    start: usize,
    start_column: usize,
    rejected_thematic: &mut [usize; 3],
) -> Result<Option<Marker>, MarkdownDocumentBufferError> {
    let mut cursor = line.ascii_cursor(document, start);
    let mut spaces = 0;
    let from = start;
    let mut column = start_column;
    while let Some((scalar, _, unit)) = cursor
        .peek(document)?
        .filter(|value| matches!(value.0, ' ' | '\t'))
    {
        // Tabs may overflow an ancestor's content boundary; keep source offsets and visual columns distinct.
        cursor.next(document)?;
        if scalar == '\t' {
            // A list syntax node begins after a consumed indentation tab, rather than copying it into spaceBefore.
            column += 4 - column % 4;
            // Source node starts remain at the ancestor boundary; an unconsumed tab makes the regex ineligible.
            let _ = unit;
            spaces = 0;
        } else {
            // Literal spaces following the current ancestor/tab remain the marker's source spelling.
            column += 1;
            spaces += 1;
        }
    }
    let Some((first, _, unit)) = cursor.next(document)? else {
        // A whitespace-only source suffix has no explicit marker.
        return Ok(None);
    };
    let punctuation = unit - line.units.0;
    let mut context = Context {
        kind: Kind::Quote,
        from,
        to: start,
        before: spaces,
        after: 0,
        number: 0,
        task: 0,
        item_line: line.number,
        node_start: line.units.0 + start,
        item_index: 0,
        loose: false,
        blank_seen: false,
        eligible: true,
        indent: 0,
    };
    if first == '>' {
        // Quote nodes begin at their punctuation, excluding optional preceding indentation.
        context.from = punctuation;
        context.before = 0;
        context.to = punctuation + 1;
        if cursor.peek(document)?.is_some_and(|value| value.0 == ' ') {
            // The Markdown quote command preserves only one optional following literal space.
            context.after = 1;
            context.to += 1;
        }
        let mut syntax_to = context.to;
        let mut to_column = column + context.to - punctuation;
        if cursor.peek(document)?.is_some_and(|value| value.0 == '\t') && context.after == 0 {
            // Structural quote markup consumes one tab even though the command regex preserves only literal space.
            syntax_to += 1;
            to_column += 4 - to_column % 4;
        }
        return Ok(Some(Marker {
            context,
            punctuation,
            punctuation_column: column,
            to_column,
            syntax_to,
            source_column: to_column,
        }));
    }
    if matches!(first, '-' | '+' | '*') {
        // A repeated same-bullet chain shares one exact failed-suffix scan, avoiding quadratic nesting work.
        let slot = match first {
            '-' => 0, // Minus chains share only minus-delimited thematic evidence.
            '*' => 1, // Star chains use their own independent suffix rejection boundary.
            _ => 2,   // Plus cannot form a thematic break and never acquires delimiter authority.
        };
        let thematic = if start < rejected_thematic[slot] || first == '+' {
            // The same later disqualifying scalar remains ahead of every nested marker in this interval.
            false
        } else {
            // A new candidate interval is streamed exactly once, retaining only its finite failure boundary.
            let (matches, through) = scan_thematic(document, line, start)?;
            rejected_thematic[slot] = through;
            matches
        };
        if thematic {
            // A complete thematic break wins over marker-looking nested bullet syntax.
            return Ok(None);
        }
        // The command's list-context grammar accepts literal spaces, matching the preserved regex.
        context.kind = Kind::Bullet(first as u8);
        let mut after = 0;
        while cursor.peek(document)?.is_some_and(|value| value.0 == ' ') {
            // Count all padding before deciding whether it introduces a task checkbox or code indentation.
            cursor.next(document)?;
            after += 1;
        }
        // Retain structural padding before task/command normalization can change the regex-owned marker width.
        let literal_padding = after;
        let following_padding = cursor.peek(document)?.map(|value| value.0);
        if after == 0 {
            // Tab-only/empty padding still owns list syntax, but the original command regex omits this ancestor.
            if cursor.peek(document)?.is_some_and(|value| value.0 != '\t') {
                // A nonspacing body immediately after punctuation is ordinary prose, not structural list markup.
                return Ok(None);
            }
            context.eligible = false;
        }
        if after <= 4 && cursor.peek(document)?.is_some_and(|value| value.0 == '[') {
            // A task marker is recognized only after one-to-four spaces and a complete checkbox spelling.
            let mut task_cursor = line.ascii_cursor(document, punctuation + 1 + after);
            let task = [
                task_cursor.next(document)?,
                task_cursor.next(document)?,
                task_cursor.next(document)?,
                task_cursor.next(document)?,
            ];
            if task[0].is_some_and(|value| value.0 == '[')
                && task[1].is_some_and(|value| matches!(value.0, ' ' | 'x' | 'X'))
                && task[2].is_some_and(|value| value.0 == ']')
                && task[3].is_some_and(|value| value.0 == ' ')
            {
                // Checked/unchecked source spellings share one resettable marker-width contract.
                context.task = after + 3;
                cursor = line.ascii_cursor(document, punctuation + 1 + context.task);
                after = 0;
                while cursor.peek(document)?.is_some_and(|value| value.0 == ' ') {
                    // Padding after the closing checkbox belongs to the complete task marker.
                    cursor.next(document)?;
                    after += 1;
                }
            }
        }
        if after > 4 {
            // Four excess spaces are the indented code child's padding rather than list marker width.
            after -= 4;
        }
        context.after = after;
        context.to = punctuation + 1 + context.task + after;
        let (syntax_to, to_column, source_column) = list_boundary(
            document,
            line,
            punctuation + 1,
            column + 1,
            literal_padding,
            following_padding,
        )?; // 缩进窗口的真实读取/回收失败必须中止原命令，不能发布未认证的标记边界。
        return Ok(Some(Marker {
            context,
            punctuation,
            punctuation_column: column,
            to_column,
            syntax_to,
            source_column,
        }));
    }
    if first.is_ascii_digit() {
        // Ordered list syntax permits at most nine decimal digits before one delimiter punctuation.
        let mut number = first.to_digit(10).expect("ASCII decimal") as u64;
        let mut digits = 1;
        while let Some((scalar, _, _)) = cursor
            .peek(document)?
            .filter(|value| value.0.is_ascii_digit())
        {
            // Early rejection bounds marker-number arithmetic without retaining decimal text.
            if digits == 9 {
                // A tenth digit is numeric prose under the upstream list grammar, never a widened list number.
                return Ok(None);
            }
            cursor.next(document)?;
            number = number * 10 + scalar.to_digit(10).expect("decimal") as u64;
            digits += 1;
        }
        let Some((delimiter, _, _)) = cursor
            .next(document)?
            .filter(|value| matches!(value.0, '.' | ')'))
        else {
            // Numeric prose without a list delimiter is ordinary paragraph source.
            return Ok(None);
        };
        let mut after = 0;
        while cursor.peek(document)?.is_some_and(|value| value.0 == ' ') {
            // Literal padding is retained in marker counts without a line-sized string.
            cursor.next(document)?;
            after += 1;
        }
        if after == 0 && cursor.peek(document)?.is_some_and(|value| value.0 != '\t') {
            // A nonempty ordered item requires whitespace after its punctuation in the Markdown grammar.
            return Ok(None);
        }
        // Structural list padding still owns complete tabs even when the command regex preserves zero spaces.
        let literal_padding = after;
        let following_padding = cursor.peek(document)?.map(|value| value.0);
        if after >= 4 {
            // The original ordered matcher assigns four padding columns to a code child.
            after -= 4;
        }
        context.kind = Kind::Ordered(delimiter as u8);
        context.number = number;
        context.after = after;
        context.to = punctuation + digits + 1 + after;
        let (syntax_to, to_column, source_column) = list_boundary(
            document,
            line,
            punctuation + digits + 1,
            column + digits + 1,
            literal_padding,
            following_padding,
        )?; // 有序列表同样透传原源错误，不把失败误认为可用的空白宽度。
        return Ok(Some(Marker {
            context,
            punctuation,
            punctuation_column: column,
            to_column,
            syntax_to,
            source_column,
        }));
    }
    // The first body scalar is neither an explicit quote nor list marker.
    Ok(None)
}

/// 独立解析实际内容缩进；所有窗口读取与回收失败均透传，不能使用部分边界提交命令。
fn list_boundary(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    after_marker: usize,
    marker_column: usize,
    literal_padding: usize,
    following_padding: Option<char>,
) -> Result<(usize, usize, usize), MarkdownDocumentBufferError> {
    if following_padding != Some('\t') || literal_padding >= 5 {
        // Already-counted literal spaces resolve ordinary list items without another cursor/allocation per sibling.
        let width = if literal_padding >= 5 || following_padding.is_none() {
            // Excess or empty padding gives one logical content column under the upstream block grammar.
            1
        } else {
            // One-to-four literal spaces are the complete known structural padding.
            literal_padding
        };
        let consumed = width.min(literal_padding);
        return Ok((
            after_marker + consumed,
            marker_column + width,
            marker_column + consumed,
        ));
    }
    let mut cursor = line.ascii_cursor(document, after_marker);
    let mut column = marker_column;
    while let Some((scalar, _, _)) = cursor
        .peek(document)?
        .filter(|value| matches!(value.0, ' ' | '\t'))
    {
        // Exact visual padding includes whole tabs; only finite coordinates survive this source pass.
        cursor.next(document)?;
        column += if scalar == '\t' {
            // Keep tab-expanded source columns separate from physical native offsets.
            4 - column % 4
        } else {
            // A non-tab complete source scalar advances one visual column.
            1
        };
    }
    let target = if column >= marker_column + 5 || cursor.peek(document)?.is_none() {
        // Excess padding or an empty item assigns one content column, leaving the rest to code/body syntax.
        marker_column + 1
    } else {
        // One-to-four padding columns establish the exact structural content indentation.
        column
    };
    let mut cursor = line.ascii_cursor(document, after_marker);
    let mut actual = marker_column;
    let mut offset = after_marker;
    while actual < target {
        // A physical tab may overflow the logical content boundary; preserve both coordinates explicitly.
        let Some((scalar, _, unit)) = cursor.next(document)? else {
            // An empty marker at end of line has logical indentation beyond its last actual source scalar.
            break;
        };
        actual += if scalar == '\t' {
            // A whole physical tab may cross the logical content boundary.
            4 - actual % 4
        } else {
            // Literal spacing consumes one source unit and one visual column.
            1
        };
        offset = unit - line.units.0 + scalar.len_utf16();
    }
    Ok((offset, target, actual))
}

/// Interrupting block prefixes prevent a non-paragraph child from borrowing unmarked lazy prose.
pub(super) fn line_interrupts(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    offset: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    let mut cursor = line.ascii_cursor(document, offset);
    let mut spaces = 0;
    while cursor.peek(document)?.is_some_and(|value| value.0 == ' ') {
        // Only relative ASCII indentation participates in block interruption.
        cursor.next(document)?;
        spaces += 1;
    }
    if spaces >= 4 {
        // Indented code breaks explicit continuation rather than borrowing an unmarked lazy paragraph.
        return Ok(true);
    }
    let Some((scalar, _, _)) = cursor.next(document)? else {
        // An empty suffix supplies no new block interruption.
        return Ok(false);
    };
    if scalar == '#' {
        // Only one-to-six hashes followed by the preserved editor's literal space/end form interrupt prose.
        let mut count = 1;
        while cursor.peek(document)?.is_some_and(|value| value.0 == '#') {
            // Count the complete heading run rather than accepting a truncated six-hash prefix.
            cursor.next(document)?;
            count += 1;
        }
        return Ok(count <= 6 && cursor.peek(document)?.is_none_or(|value| value.0 == ' '));
    }
    if matches!(scalar, '`' | '~') {
        // Inline or invalid fence-like text remains paragraph content eligible for lazy continuation.
        return Ok(fence_descriptor(document, line, offset)?.is_some());
    }
    if matches!(scalar, '-' | '_' | '*') {
        // Spaced thematic markers are independent blocks even when they resemble a chain of empty items.
        return thematic_at(document, line, offset);
    }
    Ok(scalar == '>' || (scalar == '<' && html_start(document, line, offset)?.is_some()))
}

/// The preserved list context regex accepts literal spaces before punctuation, excluding raw tabs.
pub(super) fn ascii_spaces(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    start: usize,
    end: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    let mut cursor = line.ascii_cursor(document, start);
    while let Some((scalar, _, unit)) = cursor.next(document)? {
        // End coordinates refer to classified ASCII marker prefixes, never arbitrary native body text.
        if unit - line.units.0 >= end {
            // The already classified punctuation boundary excludes this scalar from premarker padding.
            break;
        }
        if scalar != ' ' {
            // Structural tabs may own syntax while the original command regex declines this exact context.
            return Ok(false);
        }
    }
    Ok(true)
}

/// Existing paragraphs are interrupted only by nonempty bullets or an ordered list starting at one.
pub(super) fn marker_interrupts(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    marker: &Marker,
) -> Result<bool, MarkdownDocumentBufferError> {
    if marker.context.kind == Kind::Quote {
        // An explicit quote marker always begins a separate block rather than lazy paragraph text.
        return Ok(true);
    }
    if matches!(marker.context.kind, Kind::Ordered(_)) && marker.context.number != 1 {
        // A non-one decimal spelling remains ordinary prose unless an existing same-level list owns it.
        return Ok(false);
    }
    Ok(!range_is_whitespace(
        document,
        line.units.0 + marker.context.to..line.units.1,
        false,
    )?)
}

/// Recognize an entire thematic-break candidate with scalar counters and no copied punctuation line.
pub(super) fn thematic_at(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    offset: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    Ok(scan_thematic(document, line, offset)?.0)
}

/// Return an exact failed suffix boundary so nested same-marker prefixes never rescan it quadratically.
fn scan_thematic(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    offset: usize,
) -> Result<(bool, usize), MarkdownDocumentBufferError> {
    let mut cursor = line.ascii_cursor(document, offset);
    let mut spaces = 0;
    while cursor.peek(document)?.is_some_and(|value| value.0 == ' ') {
        // Four relative indentation columns identify code instead of a top-level thematic delimiter.
        cursor.next(document)?;
        spaces += 1;
        if spaces > 3 {
            // A code-indented prefix provides no thematic rejection evidence for a later nested marker.
            return Ok((false, offset));
        }
    }
    let Some((marker, _, _)) = cursor
        .next(document)?
        .filter(|value| matches!(value.0, '-' | '*' | '_'))
    else {
        // Other initial punctuation/text cannot become a thematic break or justify a cached suffix interval.
        return Ok((false, offset));
    };
    let mut count = 1;
    while let Some((scalar, _, unit)) = cursor.next(document)? {
        // Only one marker identity and ASCII padding may continue this candidate suffix.
        if scalar == marker {
            // A same-marker suffix advances the count without retaining its source text.
            count += 1;
        } else if !matches!(scalar, ' ' | '\t') {
            // Every same-marker start before this exact disqualifying scalar also fails thematic syntax.
            return Ok((false, unit - line.units.0));
        }
    }
    // Fewer than three complete markers cannot become a thematic break at any later same-marker start.
    Ok((count >= 3, line.units.1 - line.units.0))
}

/// Inspect the entire delimiter/info tail with scalar metadata, never a truncated marker prefix.
pub(super) fn fence_descriptor(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    offset: usize,
) -> Result<Option<(char, usize, bool)>, MarkdownDocumentBufferError> {
    let mut cursor = line.ascii_cursor(document, offset);
    let mut spaces = 0;
    while cursor.peek(document)?.is_some_and(|value| value.0 == ' ') {
        // Fence indentation is relative to the already consumed container content boundary.
        cursor.next(document)?;
        spaces += 1;
    }
    if spaces > 3 {
        // Four relative spaces establish code indentation rather than an opening/closing fence.
        return Ok(None);
    }
    let Some((marker, _, _)) = cursor
        .next(document)?
        .filter(|value| matches!(value.0, '`' | '~'))
    else {
        // Other punctuation cannot own a fenced opaque body.
        return Ok(None);
    };
    let mut count = 1;
    while cursor
        .peek(document)?
        .is_some_and(|value| value.0 == marker)
    {
        // The exact complete run length remains authoritative even for a giant delimiter-only line.
        cursor.next(document)?;
        count += 1;
    }
    if count < 3 {
        // Inline code/tilde spelling must not make surrounding Markdown containers opaque.
        return Ok(None);
    }
    let mut closing = true;
    while let Some((scalar, _, _)) = cursor.next(document)? {
        // Backticks anywhere in opening info invalidate that opener; closing tails accept ASCII padding only.
        if marker == '`' && scalar == '`' {
            // A later backtick invalidates the opener even after an arbitrarily long information string.
            return Ok(None);
        }
        closing &= matches!(scalar, ' ' | '\t');
    }
    Ok(Some((marker, count, closing)))
}
