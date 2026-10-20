//! 真实Parser容器的有限源前缀定位；不保存正文，也不按整List逐行猜测item。

use super::*;

#[derive(Clone, Debug)]
pub(super) enum MarkdownMarkerOwner {
    /// 普通inline仅由选区端点所在的真实结构及祖先显露。
    Structure(Range<usize>),
    /// quote与围栏只由选区端点所在物理行显露；带EOL的末端属于下一行。
    PhysicalLine {
        range: Range<usize>,
        includes_end: bool,
    },
}

impl MarkdownMarkerOwner {
    /// 两类owner共用一次byte→UTF16边界收集，但保留不同的活动规则。
    pub(super) fn range(&self) -> &Range<usize> {
        match self {
            Self::Structure(range) | Self::PhysicalLine { range, .. } => range,
        }
    }

    /// 物理行owner不把跨行选区中间的行误作正在编辑的端点行。
    pub(super) fn is_active(&self, selection: &Range<usize>, range: &Range<usize>) -> bool {
        match self {
            Self::Structure(_) => markdown_selection_intersects(selection, range),
            Self::PhysicalLine { includes_end, .. } => {
                markdown_physical_line_is_active(selection, range, *includes_end)
            }
        }
    }
}

/// 普通envelope与S2真实物理行共用端点规则；EOL末端属于下一行，真实无EOL末行可含EOF。
pub(crate) fn markdown_physical_line_is_active(
    selection: &Range<usize>,
    line: &Range<usize>,
    includes_end: bool,
) -> bool {
    let contains = |position| {
        line.start <= position && (position < line.end || (includes_end && position == line.end))
    };
    contains(selection.start) || contains(selection.end)
}

#[derive(Clone, Debug)]
pub(super) struct MarkdownByteListMarker {
    pub(super) range: Range<usize>,
    pub(super) owner: Range<usize>,
    pub(super) kind: MarkdownPreviewListMarkerKind,
}

/// 每个容器仅保留Parser确认的范围和首行缩进；Item任务状态由真实event补齐。
struct Container {
    range: Range<usize>,
    kind: ContainerKind,
}

#[derive(Clone)]
enum ContainerKind {
    Quote,
    Footnote,
    Item {
        marker: Option<Range<usize>>,
        indent: usize,
        bullet: Option<MarkdownBulletMarker>,
        checked: Option<bool>,
    },
}

#[derive(Default)]
pub(super) struct MarkdownPrefixBuilder {
    containers: Vec<Container>,
    content_starts: Vec<usize>,
    tasks: Vec<MarkdownByteListMarker>,
    quote_depth: usize,
}

impl MarkdownPrefixBuilder {
    /// 临时容器、内容屏障与输出token也计入同一8192记录预算，先准入再分配。
    fn admit(
        &self,
        decorations: &[MarkdownByteDecoration],
        additional: usize,
    ) -> Result<(), MarkdownViewportPresentationError> {
        if decorations.len()
            + self.containers.len()
            + self.content_starts.len()
            + self.tasks.len()
            + additional
            > MAX_MARKDOWN_PRESENTATION_DECORATIONS
        {
            return Err(MarkdownViewportPresentationError::DecorationLimitExceeded);
        }
        Ok(())
    }

    /// quote权限仅来自真实Start事件；字面`>`不会创建容器。
    pub(super) fn quote(
        &mut self,
        range: Range<usize>,
        decorations: &[MarkdownByteDecoration],
    ) -> Result<(), MarkdownViewportPresentationError> {
        self.admit(decorations, 1)?;
        self.quote_depth += 1;
        self.containers.push(Container {
            range,
            kind: ContainerKind::Quote,
        });
        Ok(())
    }

    /// 对应End事件关闭当前quote活动域，后续普通正文不保留无用叶屏障。
    pub(super) fn end_quote(&mut self) -> Result<(), MarkdownViewportPresentationError> {
        self.quote_depth = self
            .quote_depth
            .checked_sub(1)
            .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
        Ok(())
    }

    /// GFM真实Footnote容器的续行消耗四列；不保存或重新识别label内容。
    pub(super) fn footnote(
        &mut self,
        range: Range<usize>,
        decorations: &[MarkdownByteDecoration],
    ) -> Result<(), MarkdownViewportPresentationError> {
        self.admit(decorations, 1)?;
        self.containers.push(Container {
            range,
            kind: ContainerKind::Footnote,
        });
        Ok(())
    }

    /// Item首行只定位Parser已确认的opening，不据后续行中的`-`制造第二个item。
    pub(super) fn item(
        &mut self,
        source: &str,
        range: Range<usize>,
        decorations: &[MarkdownByteDecoration],
    ) -> Result<usize, MarkdownViewportPresentationError> {
        self.admit(decorations, 1)?;
        source
            .get(range.clone())
            .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
        // raw Item.start可能因quote后tab的虚拟列回到祖先prefix；opening延后到同一物理行容器游标定位。
        let index = self.containers.len();
        self.containers.push(Container {
            range,
            kind: ContainerKind::Item {
                marker: None,
                indent: 0,
                bullet: None,
                checked: None,
            },
        });
        Ok(index)
    }

    /// 叶起点仅约束它所在物理行；跨行Code的raw span可包含后续真实quote前缀。
    pub(super) fn content(
        &mut self,
        start: usize,
        decorations: &[MarkdownByteDecoration],
    ) -> Result<(), MarkdownViewportPresentationError> {
        if self.quote_depth != 0 {
            self.admit(decorations, 1)?;
            self.content_starts.push(start);
        }
        Ok(())
    }

    /// TaskListMarker必须对应当前真实Item，且原范围恰为三个ASCII字符。
    pub(super) fn task(
        &mut self,
        source: &str,
        range: Range<usize>,
        checked: bool,
        item: usize,
        decorations: &[MarkdownByteDecoration],
    ) -> Result<(), MarkdownViewportPresentationError> {
        self.admit(decorations, 2)?;
        let container = self
            .containers
            .get_mut(item)
            .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
        let ContainerKind::Item { checked: task, .. } = &mut container.kind else {
            return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
        };
        let fragment = source
            .get(range.clone())
            .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
        let token_start = range
            .end
            .checked_sub(3)
            .filter(|start| *start >= range.start)
            .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
        let expected = fragment
            .get(fragment.len() - 3..)
            .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
        // Parser的event可含checkbox前最多三列空白；只有末尾真实三字符交给glyph消费者。
        if !source.as_bytes()[range.start..token_start]
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t'))
        {
            return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
        }
        let range = token_start..range.end;
        let valid_token = match expected.as_bytes() {
            [b'[', b'x' | b'X', b']'] => checked,
            [b'[', b' ' | b'\t' | 0x0b | 0x0c, b']'] => !checked,
            _ => false,
        };
        if range.start < container.range.start
            || range.end > container.range.end
            || !valid_token
            || task.is_some()
        {
            // 不能拿相似字面checkbox或第二个event给原item补造权限。
            return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
        }
        *task = Some(checked);
        self.tasks.push(MarkdownByteListMarker {
            range,
            owner: container.range.clone(),
            kind: MarkdownPreviewListMarkerKind::TaskBox { checked },
        });
        Ok(())
    }

    /// 单次物理行扫描按真实容器次序分配quote；每个源token最多输出一次。
    pub(super) fn finish(
        mut self,
        source: &str,
        decorations: &mut Vec<MarkdownByteDecoration>,
    ) -> Result<Vec<MarkdownByteListMarker>, MarkdownViewportPresentationError> {
        // 保留Parser真实前序：tab虚拟列可能使子Item的raw.start早于同一行祖先，不能按raw.start重排。
        self.content_starts.sort_unstable();
        self.content_starts.dedup();
        let mut active: Vec<usize> = Vec::new();
        let mut next = 0;
        let mut content_index = 0;
        let mut line_start = 0;
        while line_start < source.len() {
            // CRLF是一个物理EOL；独立CR与LF也都结束当前行，不能激活上一行末端。
            let content_end = source.as_bytes()[line_start..]
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .map_or(source.len(), |offset| line_start + offset);
            let line_end = if source.as_bytes().get(content_end) == Some(&b'\r')
                && source.as_bytes().get(content_end + 1) == Some(&b'\n')
            {
                content_end + 2
            } else if content_end < source.len() {
                content_end + 1
            } else {
                content_end
            };
            while active
                .last()
                .is_some_and(|index| self.containers[*index].range.end <= line_start)
            {
                active.pop();
            }
            while next < self.containers.len() && self.containers[next].range.start < line_end {
                // 相邻item结束后移出栈；活跃祖先仍保留，嵌套quote不重复重扫外层。
                while active.last().is_some_and(|index| {
                    self.containers[*index].range.end <= self.containers[next].range.start
                }) {
                    active.pop();
                }
                active.push(next);
                next += 1;
            }
            while self
                .content_starts
                .get(content_index)
                .is_some_and(|start| *start < line_start)
            {
                content_index += 1;
            }
            let barrier = self
                .content_starts
                .get(content_index)
                .copied()
                .filter(|start| *start < content_end)
                .unwrap_or(content_end);
            let mut cursor = PrefixCursor::new(source.as_bytes(), line_start, content_end);
            for index in &active {
                // 缺失任一外层prefix就是lazy continuation，不能继续在正文内追找内层`>`。
                match self.containers[*index].kind.clone() {
                    ContainerKind::Quote => {
                        let opening = self.containers[*index].range.start;
                        if opening >= line_start && opening > cursor.offset {
                            let skipped = source
                                .get(cursor.offset..opening)
                                .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
                            if skipped.bytes().any(|byte| !matches!(byte, b' ' | b'\t')) {
                                // 本行新quote的Parser确切anchor可越过未投影的Footnote opening，不能在label里搜`>`。
                                // GFM parse_footnote已消费全部分隔空白并重置LineStart，故新容器列基点在此anchor。
                                if source.as_bytes().get(opening) != Some(&b'>') {
                                    return Err(
                                        MarkdownViewportPresentationError::InvalidParserBoundary,
                                    );
                                }
                                cursor.offset = opening;
                                cursor.column = 0;
                                cursor.remaining = 0;
                            }
                        }
                        cursor.spaces(3);
                        if cursor.remaining != 0
                            || cursor.offset >= barrier
                            || cursor.byte() != Some(b'>')
                        {
                            break;
                        }
                        let start = cursor.offset;
                        cursor.take();
                        let mut marker_end = cursor.offset;
                        if cursor.byte() == Some(b' ') {
                            // 旧QuoteMark只把紧随的一个ASCII空格一起隐藏。
                            cursor.take();
                            marker_end = cursor.offset;
                        } else if cursor.byte() == Some(b'\t') {
                            // Parser仍把tab的一列作为容器分隔消费，余列留给内层；tab本身保持字面宽度。
                            cursor.spaces(1);
                        }
                        self.admit(decorations, 1)?;
                        decorations.push(MarkdownByteDecoration {
                            byte_range: start..marker_end,
                            styles: MarkdownPresentationStyles::MARKER,
                            marker_owner: Some(MarkdownMarkerOwner::PhysicalLine {
                                range: line_start..line_end,
                                includes_end: content_end == source.len(),
                            }),
                        });
                    }
                    ContainerKind::Footnote => {
                        if self.containers[*index].range.start < line_start && cursor.spaces(4) != 4
                        {
                            // 首行label由后继真实opening的anchor越过；续行缺四列时不能深入其quote/body。
                            break;
                        }
                    }
                    ContainerKind::Item {
                        marker,
                        indent,
                        checked,
                        ..
                    } => {
                        if marker.is_none() {
                            let opening = self.containers[*index].range.start;
                            if opening > cursor.offset {
                                let skipped = source.get(cursor.offset..opening).ok_or(
                                    MarkdownViewportPresentationError::InvalidParserBoundary,
                                )?;
                                if skipped.bytes().any(|byte| !matches!(byte, b' ' | b'\t')) {
                                    // Footnote后的真实Item首anchor与quote一样来自Parser；不在label中搜索bullet。
                                    cursor.offset = opening;
                                    cursor.column = 0;
                                    cursor.remaining = 0;
                                }
                            }
                            // 沿真实祖先消费后的同一游标定位opening，保留tab余列而不搜索任意bullet。
                            let (marker, indent, bullet) = item_opening(&mut cursor)
                                .ok_or(MarkdownViewportPresentationError::InvalidParserBoundary)?;
                            if marker.start < self.containers[*index].range.start
                                || marker.end > self.containers[*index].range.end
                            {
                                return Err(
                                    MarkdownViewportPresentationError::InvalidParserBoundary,
                                );
                            }
                            self.admit(decorations, 1)?;
                            push_markdown_visible_marker(
                                decorations,
                                marker.clone(),
                                &self.containers[*index].range,
                            )?;
                            self.containers[*index].kind = ContainerKind::Item {
                                marker: Some(marker),
                                indent,
                                bullet,
                                checked,
                            };
                        } else if cursor.spaces(indent) != indent {
                            // 原item可有lazy正文，但缺少其缩进时不得继续消费内层quote。
                            break;
                        }
                    }
                }
            }
            line_start = line_end;
        }
        if self
            .containers
            .iter()
            .any(|container| matches!(container.kind, ContainerKind::Item { marker: None, .. }))
        {
            // 真正Item必须在其首行沿原祖先路径定位成功，不能只发布孤立TaskBox或缺失bullet的半批。
            return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
        }
        let mut markers = std::mem::take(&mut self.tasks);
        for container in &self.containers {
            let ContainerKind::Item {
                marker: Some(marker),
                bullet: Some(bullet),
                checked,
                ..
            } = &container.kind
            else {
                continue;
            };
            // ordered数字不产生glyph metadata；任务bullet和box共享同一真实item owner。
            if decorations.len() + self.containers.len() + self.content_starts.len() + markers.len()
                >= MAX_MARKDOWN_PRESENTATION_DECORATIONS
            {
                return Err(MarkdownViewportPresentationError::DecorationLimitExceeded);
            }
            markers.push(MarkdownByteListMarker {
                range: marker.start..marker.start + 1,
                owner: container.range.clone(),
                kind: checked.map_or(MarkdownPreviewListMarkerKind::Bullet(*bullet), |checked| {
                    MarkdownPreviewListMarkerKind::TaskBullet {
                        source: *bullet,
                        checked,
                    }
                }),
            });
        }
        markers.sort_unstable_by_key(|marker| marker.range.start);
        if markers
            .windows(2)
            .any(|pair| pair[0].range.end > pair[1].range.start)
        {
            // 重叠token不能交给Native二分缓存，整批拒绝而非发布部分glyph。
            return Err(MarkdownViewportPresentationError::InvalidParserBoundary);
        }
        Ok(markers)
    }
}

/// 只在真实Item当前容器位置定位opening，不把raw.start的tab回退误当正文语法。
fn item_opening(
    cursor: &mut PrefixCursor<'_>,
) -> Option<(Range<usize>, usize, Option<MarkdownBulletMarker>)> {
    let leading = cursor.spaces(3);
    if cursor.remaining != 0 {
        return None;
    }
    let start = cursor.offset;
    let bullet = match cursor.byte()? {
        b'-' => Some(MarkdownBulletMarker::Dash),
        b'+' => Some(MarkdownBulletMarker::Plus),
        b'*' => Some(MarkdownBulletMarker::Star),
        b'0'..=b'9' => None,
        _ => return None,
    };
    if bullet.is_some() {
        cursor.take();
    } else {
        // Parser已确认ordered item，仍核数字终止符，避免错误offset越过正文。
        while cursor.byte().is_some_and(|byte| byte.is_ascii_digit()) {
            cursor.take();
        }
        if !matches!(cursor.byte(), Some(b'.' | b')')) {
            return None;
        }
        cursor.take();
    }
    let marker_end = cursor.offset;
    let saved_offset = cursor.offset;
    let saved_column = cursor.column;
    let padding = cursor.spaces(5);
    let blank = cursor.offset == cursor.end;
    if padding == 0 && !blank {
        return None;
    }
    let padding = if padding == 0 || padding > 4 || blank {
        // 与Parser保持相同回退：空item或过量缩进只授予一列item padding。
        cursor.offset = saved_offset;
        cursor.column = saved_column;
        cursor.remaining = 0;
        cursor.spaces(1);
        1
    } else {
        padding
    };
    let visible_end = if cursor
        .bytes
        .get(marker_end)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        marker_end + 1
    } else {
        marker_end
    };
    Some((
        start..visible_end,
        leading + marker_end - start + padding,
        bullet,
    ))
}

/// 仅定位已授予容器prefix的ASCII列；tab余宽可跨item缩进与quote前空白共享。
struct PrefixCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
    end: usize,
    column: usize,
    remaining: usize,
}

impl<'a> PrefixCursor<'a> {
    fn new(bytes: &'a [u8], offset: usize, end: usize) -> Self {
        Self {
            bytes,
            offset,
            end,
            column: 0,
            remaining: 0,
        }
    }
    fn byte(&self) -> Option<u8> {
        (self.offset < self.end).then(|| self.bytes[self.offset])
    }
    fn take(&mut self) {
        // 此处仅越过已证明的ASCII前缀，tab仍保留真实物理列宽。
        self.column += if self.byte() == Some(b'\t') {
            4 - self.column % 4
        } else {
            1
        };
        self.offset += 1;
    }
    fn spaces(&mut self, count: usize) -> usize {
        let mut consumed = self.remaining.min(count);
        self.remaining -= consumed;
        while consumed < count {
            // tab按照当前物理行列展开，未消费余宽继续留给下一个真实容器。
            let width = match self.byte() {
                Some(b' ') => 1,
                Some(b'\t') => 4 - self.column % 4,
                _ => break,
            };
            self.offset += 1;
            self.column += width;
            let used = width.min(count - consumed);
            consumed += used;
            self.remaining = width - used;
        }
        consumed
    }
}
