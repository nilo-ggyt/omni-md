//! 真实scope的容器属性与源marker恢复；不从压缩投影猜测祖先类型。

use super::*;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationRun, MarkdownPresentationStyles as Styles,
};
use pulldown_cmark::block_frontier::TableScopeKind;

const MAX_RUNS: usize = 8192;
pub(super) use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota as BatchQuota;

#[derive(Default)]
pub(super) struct ScopeProjection {
    first: [Option<usize>; 3],
    header_markers: Vec<(TableScopeKind, usize)>,
    before_header: [bool; 3],
    quote_starts: Vec<usize>,
    quote_count: usize,
    quota: Option<BatchQuota>,
}
impl ScopeProjection {
    pub fn with_quota(quota: BatchQuota) -> Self {
        Self {
            quota: Some(quota),
            ..Self::default()
        }
    }
    pub fn observe(
        &mut self,
        ticket: &MarkdownTableStructuralWindow,
        kind: TableScopeKind,
        start: usize,
    ) -> Result<usize, Error> {
        let Some(index) = kind_index(kind) else {
            return Ok(0);
        };
        let header = ticket.header;
        if start > header.header_byte_start + header.header_prefix_bytes {
            // 真实祖先必须在table content前已开启，不能让未来容器得到当前行的属性权限。
            return Err(Error::InvalidSource);
        }
        self.first[index] = Some(self.first[index].map_or(start, |old| old.min(start)));
        if kind == TableScopeKind::BlockQuote {
            if self.quote_count == MAX_RUNS {
                return Err(Error::SemanticLimit);
            }
            self.quote_count += 1;
            if !ticket.include_header {
                // 离屏H不需要其opening目录：续行只有原container prefix，固定层数足够逐层消费。
                return Ok(0);
            }
            // H可见时每层真实quote都保留原起点，不能按kind合并或猜footnote label里的`>`。
            if self.quote_starts.len() + self.header_markers.len() == MAX_RUNS {
                return Err(Error::SemanticLimit);
            }
            if let Some(quota) = &self.quota {
                quota.admit_metadata(std::mem::size_of::<usize>())?;
            }
            self.quote_starts.push(start);
            return Ok(std::mem::size_of::<usize>());
        }
        if !ticket.include_header {
            return Ok(0);
        }
        let anchor = if start < header.header_byte_start {
            if self.before_header[index] {
                return Ok(0);
            } // 同kind旧祖先在H行都从同一物理起点识别marker。
            self.before_header[index] = true;
            header.header_byte_start
        } else {
            start
        };
        if self.header_markers.len() + self.quote_starts.len() == MAX_RUNS {
            // metadata先准入再push；深spine不在失败前生成无界header marker目录。
            return Err(Error::SemanticLimit);
        }
        if let Some(quota) = &self.quota {
            quota.admit_metadata(std::mem::size_of::<(TableScopeKind, usize)>())?;
        }
        self.header_markers.push((kind, anchor));
        Ok(std::mem::size_of::<(TableScopeKind, usize)>())
    }
    pub fn into_builder(self) -> PrefixRuns {
        PrefixRuns {
            scope: self,
            runs: Vec::new(),
            first_index: 0,
            row_index: 0,
            marker_index: 0,
            quote_index: 0,
            quote_cursor: 0,
            stage: Stage::Containers,
        }
    }
}

#[derive(Clone, Copy)]
enum Stage {
    Containers,
    TablePrefix,
    Quotes,
    Markers,
    Complete,
}

pub(super) struct PrefixRuns {
    scope: ScopeProjection,
    runs: Vec<MarkdownPresentationRun>,
    first_index: usize,
    row_index: usize,
    marker_index: usize,
    quote_index: usize,
    quote_cursor: usize,
    stage: Stage,
}
impl PrefixRuns {
    /// 每次只做一个有限style/marker映射；marker识别仍交既有helper，输入量单独报告。
    pub fn advance(
        &mut self,
        lines: &[OwnedLine],
        ticket: &MarkdownTableStructuralWindow,
        selection: &Range<usize>,
        document_utf16_len: usize,
    ) -> Result<(SemanticWork, bool), Error> {
        let mut work = SemanticWork {
            structural_transitions: 64,
            ..SemanticWork::default()
        };
        match self.stage {
            Stage::Containers => {
                if self.first_index == 3 {
                    self.stage = Stage::TablePrefix;
                    return Ok((work, false));
                }
                let index = self.first_index;
                self.first_index += 1;
                if let Some(start) = self.scope.first[index] {
                    let h = &lines[0];
                    let clipped = start.max(h.spec.bytes.start);
                    let local = clipped - h.spec.bytes.start;
                    let prefix = h.text.get(..local).ok_or(Error::InvalidSource)?;
                    work.normalizer_input_bytes = prefix.len();
                    let utf16_start = h.spec.utf16.start + prefix.encode_utf16().count();
                    let end = lines.last().ok_or(Error::InvalidSource)?.spec.utf16.end;
                    self.push(utf16_start..end, style(index))?;
                }
            }
            Stage::TablePrefix => {
                if self.row_index == lines.len() {
                    self.row_index = 0;
                    self.stage = Stage::Quotes;
                    return Ok((work, false));
                }
                let line = &lines[self.row_index];
                let is_header = self.row_index == 0;
                self.row_index += 1;
                if is_header
                    || line.spec.prefix == 0
                    || line.spec.delimiter && !ticket.include_header
                {
                    return Ok((work, false)); // H prefix在原TagTable.start之前，不能误补TABLE。
                }
                let prefix = line
                    .text
                    .get(..line.spec.prefix)
                    .ok_or(Error::InvalidSource)?;
                work.normalizer_input_bytes = prefix.len();
                let mut styles = Styles::TABLE;
                if line.spec.delimiter {
                    // 原delimiter字节范围含真实container prefix且不含EOL，压缩投影必须补回同样隐藏属性。
                    styles = styles
                        .union(Styles::TABLE_DELIMITER)
                        .union(Styles::HIDDEN_MARKER);
                }
                self.push(
                    line.spec.utf16.start..line.spec.utf16.start + prefix.encode_utf16().count(),
                    styles,
                )?;
            }
            Stage::Quotes => {
                if self.row_index == lines.len() {
                    // quote阶段结束后复用原列表marker阶段；每次advance仍最多输出一个marker。
                    self.row_index = 0;
                    self.stage = Stage::Markers;
                    return Ok((work, false));
                }
                let line = &lines[self.row_index];
                if self.quote_index == self.scope.quote_count
                    || self.row_index <= 1 && !ticket.include_header
                {
                    self.row_index += 1;
                    self.quote_index = 0;
                    self.quote_cursor = 0;
                    return Ok((work, false));
                }
                let source_start = self.scope.quote_starts.get(self.quote_index).copied();
                self.quote_index += 1;
                let prefix = line
                    .text
                    .get(..line.spec.prefix)
                    .ok_or(Error::InvalidSource)?;
                let start = if self.row_index == 0
                    && source_start.is_some_and(|start| start >= line.spec.bytes.start)
                {
                    // H上新quote的真实anchor跳过已验证Item/Footnote opening，不能搜label里的字面`>`。
                    source_start.ok_or(Error::InvalidSource)? - line.spec.bytes.start
                } else {
                    // ActiveTable续行只有原container continuation；按scope次序从上个quote末端前进。
                    self.quote_cursor
                };
                if start < self.quote_cursor || start > prefix.len() {
                    return Err(Error::InvalidSource);
                }
                work.normalizer_input_bytes = prefix.len() - start;
                let range =
                    crate::markdown_viewport_presentation::parser::table_line_prefix_marker(
                        prefix,
                        start,
                        TableScopeKind::BlockQuote,
                    )
                    .map_err(|_| Error::ParserMismatch)?
                    .ok_or(Error::ParserMismatch)?;
                self.quote_cursor = range.end;
                let marker_start =
                    line.spec.utf16.start + prefix[..range.start].encode_utf16().count();
                let marker_end = marker_start + prefix[range].encode_utf16().count();
                let includes_end =
                    line.spec.utf16.end == document_utf16_len && !line.text.ends_with(['\r', '\n']);
                let active =
                    crate::markdown_viewport_presentation::markdown_physical_line_is_active(
                        selection,
                        &line.spec.utf16,
                        includes_end,
                    );
                let styles = if active {
                    Styles::MARKER
                } else {
                    // 同一真实物理行端点规则；压缩projection/gap不能给旧quote重新授予可见权限。
                    Styles::MARKER.union(Styles::HIDDEN_MARKER)
                };
                self.push(marker_start..marker_end, styles)?;
            }
            Stage::Markers => {
                if self.row_index == lines.len() {
                    self.stage = Stage::Complete;
                    return Ok((work, true));
                }
                let line = &lines[self.row_index];
                if self.row_index <= 1 && !ticket.include_header {
                    self.row_index += 1;
                    self.marker_index = 0;
                    return Ok((work, false)); // 离屏H/D仅语法context，不发布marker。
                }
                let marker = if self.row_index == 0 {
                    self.scope.header_markers.get(self.marker_index).copied()
                } else if self.marker_index < 3 {
                    self.scope.first[self.marker_index]
                        .map(|_| (kind(self.marker_index), line.spec.bytes.start))
                } else {
                    None
                };
                let limit = if self.row_index == 0 {
                    self.scope.header_markers.len()
                } else {
                    3
                };
                if self.marker_index == limit {
                    self.row_index += 1;
                    self.marker_index = 0;
                    return Ok((work, false));
                }
                self.marker_index += 1;
                if let Some((role, start)) = marker {
                    if role == TableScopeKind::BlockQuote {
                        return Ok((work, false)); // quote各层已经单独签发，不再按kind首祖先重复输出。
                    }
                    let local_start = start.max(line.spec.bytes.start) - line.spec.bytes.start;
                    let prefix = line
                        .text
                        .get(..line.spec.prefix)
                        .ok_or(Error::InvalidSource)?;
                    if local_start > prefix.len() {
                        return Err(Error::InvalidSource);
                    }
                    // 此helper只检查已知container prefix；不会为了marker读取/解析整个巨cell正文。
                    work.normalizer_input_bytes = prefix.len();
                    if let Some(range) =
                        crate::markdown_viewport_presentation::parser::table_line_prefix_marker(
                            prefix,
                            local_start,
                            role,
                        )
                        .map_err(|_| Error::ParserMismatch)?
                    {
                        let start =
                            line.spec.utf16.start + prefix[..range.start].encode_utf16().count();
                        let end = start + prefix[range].encode_utf16().count();
                        self.push(start..end, Styles::MARKER)?;
                    }
                }
            }
            Stage::Complete => return Ok((work, true)),
        }
        Ok((work, false))
    }
    fn push(&mut self, range: Range<usize>, styles: Styles) -> Result<(), Error> {
        if range.start >= range.end {
            return Ok(());
        }
        if self.runs.len() == MAX_RUNS {
            return Err(Error::SemanticLimit);
        }
        if let Some(quota) = &self.scope.quota {
            quota.admit_prefix()?;
        }
        self.runs.push(MarkdownPresentationRun {
            utf16_range: range,
            styles,
        });
        Ok(())
    }
    pub fn into_runs(self) -> Vec<MarkdownPresentationRun> {
        self.runs
    }
}
fn kind_index(kind: TableScopeKind) -> Option<usize> {
    match kind {
        TableScopeKind::BlockQuote => Some(0),
        TableScopeKind::OrderedList => Some(1),
        TableScopeKind::UnorderedList => Some(2),
        TableScopeKind::Other => None,
    }
}
fn kind(index: usize) -> TableScopeKind {
    [
        TableScopeKind::BlockQuote,
        TableScopeKind::OrderedList,
        TableScopeKind::UnorderedList,
    ][index]
}
fn style(index: usize) -> Styles {
    [
        Styles::BLOCK_QUOTE,
        Styles::ORDERED_LIST,
        Styles::UNORDERED_LIST,
    ][index]
}
