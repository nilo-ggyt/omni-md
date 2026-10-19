//! 导航保留真实绝对row ordinal；窗口缺少下一行只请求该行，不误判为整表结束。

use super::{MarkdownProjectedTableWindow, MarkdownTableWindowError, TABLE_WINDOW_MAX_COLUMNS};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownTableWindowNavigation {
    Next,
    Previous,
    NextRow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownTableWindowCoordinate {
    pub row_ordinal: usize,
    pub column_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownTableWindowNavigationTarget {
    Available(MarkdownTableWindowCoordinate),
    /// 目标只是一条待真实producer验证的行请求，不提供source range/几何或激活权限。
    RequiredRow(MarkdownTableWindowCoordinate),
    /// header的第一格向前没有行；表尾必须由真实TableEnd另行确认。
    TableStart,
}

/// 只计算语义坐标，不解析源、不触发layout，也不保存当前window的借用。
///
/// # Errors
/// 当前row/column不属于本批或metadata不符合有限表格约束时拒绝，不夹取相邻cell。
pub fn table_window_navigation_target(
    window: &MarkdownProjectedTableWindow,
    current: MarkdownTableWindowCoordinate,
    navigation: MarkdownTableWindowNavigation,
) -> Result<MarkdownTableWindowNavigationTarget, MarkdownTableWindowError> {
    if window.column_count == 0
        || window.column_count > TABLE_WINDOW_MAX_COLUMNS
        || current.column_index >= window.column_count
    {
        // 先验证真实header列数，后面的列加减不能跨越flat Vec或触发无界请求。
        return Err(MarkdownTableWindowError::ColumnLimit);
    }
    if window.row(current.row_ordinal).is_none() {
        // 当前格也须来自本批真实行；旧激活坐标不能直接为另一个窗口推算范围。
        return Err(MarkdownTableWindowError::ContextPending);
    }
    if window
        .cell(current.row_ordinal, current.column_index)
        .is_none()
    {
        // row已在批次里却没有其真实列是metadata失败，不能伪装成尚未加载再请求同一行。
        return Err(MarkdownTableWindowError::ParserMismatch);
    }
    let target = match navigation {
        MarkdownTableWindowNavigation::Next => {
            if current.column_index + 1 < window.column_count {
                // 同一行向右只换列，implicit缺格仍有真实row身份和独立补格计划。
                MarkdownTableWindowCoordinate {
                    column_index: current.column_index + 1,
                    ..current
                }
            } else {
                // 行尾换行使用绝对ordinal，不假设window Vec的下一项就是全表下一行。
                MarkdownTableWindowCoordinate {
                    row_ordinal: current
                        .row_ordinal
                        .checked_add(1)
                        .ok_or(MarkdownTableWindowError::InvalidSource)?,
                    column_index: 0,
                }
            }
        }
        MarkdownTableWindowNavigation::Previous => {
            if current.column_index > 0 {
                // 同行向左保留row ordinal，绝不以flat cell index跨入context-only header。
                MarkdownTableWindowCoordinate {
                    column_index: current.column_index - 1,
                    ..current
                }
            } else if let Some(previous) = current.row_ordinal.checked_sub(1) {
                // 第一列向前需要上一真实row的最后列；header离屏时同样返回明确RequiredRow。
                MarkdownTableWindowCoordinate {
                    row_ordinal: previous,
                    column_index: window.column_count - 1,
                }
            } else {
                // ordinal0就是唯一header；其第一格向前的终止不需要猜测源尾。
                return Ok(MarkdownTableWindowNavigationTarget::TableStart);
            }
        }
        MarkdownTableWindowNavigation::NextRow => {
            // Return保留列并移动真实ordinal；当前window末边不能当作整表末边。
            MarkdownTableWindowCoordinate {
                row_ordinal: current
                    .row_ordinal
                    .checked_add(1)
                    .ok_or(MarkdownTableWindowError::InvalidSource)?,
                column_index: current.column_index,
            }
        }
    };
    if window.row(target.row_ordinal).is_none() {
        // 不缓存虚拟source范围，也不自动全表重试；真实producer决定此ordinal存在或TableEnd。
        Ok(MarkdownTableWindowNavigationTarget::RequiredRow(target))
    } else if window
        .cell(target.row_ordinal, target.column_index)
        .is_some()
    {
        // 只向调用方返回本批实际存在的格，原生层仍要用同source/generation/key重新验证几何。
        Ok(MarkdownTableWindowNavigationTarget::Available(target))
    } else {
        // 当前已发布row缺列不是窗口边界；拒绝本批，不能让调度器无意义重复请求同一row。
        Err(MarkdownTableWindowError::ParserMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_viewport_presentation::table_window::{
        MarkdownTableAlignment, MarkdownTableWindowCell, MarkdownTableWindowCellSource,
        MarkdownTableWindowRow,
    };

    fn window(ordinals: &[usize]) -> MarkdownProjectedTableWindow {
        let rows = ordinals
            .iter()
            .enumerate()
            .map(|(local, ordinal)| MarkdownTableWindowRow {
                row_ordinal: *ordinal,
                source_line: ordinal + 1,
                byte_range: ordinal * 10..ordinal * 10 + 5,
                utf16_range: ordinal * 10..ordinal * 10 + 5,
                cell_index_range: local * 2..local * 2 + 2,
                is_header: *ordinal == 0,
            })
            .collect();
        let cells = ordinals
            .iter()
            .flat_map(|ordinal| {
                (0..2).map(move |column_index| MarkdownTableWindowCell {
                    source: MarkdownTableWindowCellSource::ImplicitEmpty {
                        insertion_anchor_utf16: ordinal * 10 + 5,
                    },
                    column_index,
                    alignment: MarkdownTableAlignment::Left,
                })
            })
            .collect();
        MarkdownProjectedTableWindow {
            generation: 1,
            context_revision: 2,
            header_byte_range: 0..5,
            delimiter_byte_range: 6..9,
            header_utf16_range: 0..5,
            delimiter_utf16_range: 6..9,
            column_count: 2,
            header_context: super::super::MarkdownTableWindowHeaderContext {
                row: MarkdownTableWindowRow {
                    row_ordinal: 0,
                    source_line: 0,
                    byte_range: 0..5,
                    utf16_range: 0..5,
                    cell_index_range: 0..2,
                    is_header: true,
                },
                cells: Vec::new(),
                runs: Vec::new(),
            },
            rows,
            cells,
            runs: Vec::new(),
            projection_bytes: 32,
            projection_utf16: 32,
            origins: Vec::new(),
        }
    }

    #[test]
    fn middle_window_uses_absolute_ordinal_and_requests_only_the_required_neighbor() {
        let window = window(&[4_500, 4_501]);
        let current = MarkdownTableWindowCoordinate {
            row_ordinal: 4_500,
            column_index: 1,
        };
        assert_eq!(
            table_window_navigation_target(&window, current, MarkdownTableWindowNavigation::Next)
                .unwrap(),
            MarkdownTableWindowNavigationTarget::Available(MarkdownTableWindowCoordinate {
                row_ordinal: 4_501,
                column_index: 0
            })
        );
        let edge = MarkdownTableWindowCoordinate {
            row_ordinal: 4_501,
            column_index: 1,
        };
        assert_eq!(
            table_window_navigation_target(&window, edge, MarkdownTableWindowNavigation::NextRow)
                .unwrap(),
            MarkdownTableWindowNavigationTarget::RequiredRow(MarkdownTableWindowCoordinate {
                row_ordinal: 4_502,
                column_index: 1
            })
        );
        assert!(
            table_window_navigation_target(
                &window,
                MarkdownTableWindowCoordinate {
                    row_ordinal: 0,
                    column_index: 0
                },
                MarkdownTableWindowNavigation::Next
            )
            .is_err()
        );
    }

    #[test]
    fn context_header_gap_and_true_table_start_have_distinct_outcomes() {
        let window = window(&[0, 500]);
        let body = MarkdownTableWindowCoordinate {
            row_ordinal: 500,
            column_index: 0,
        };
        assert_eq!(
            table_window_navigation_target(&window, body, MarkdownTableWindowNavigation::Previous)
                .unwrap(),
            MarkdownTableWindowNavigationTarget::RequiredRow(MarkdownTableWindowCoordinate {
                row_ordinal: 499,
                column_index: 1
            })
        );
        let header = MarkdownTableWindowCoordinate {
            row_ordinal: 0,
            column_index: 1,
        };
        assert_eq!(
            table_window_navigation_target(&window, header, MarkdownTableWindowNavigation::Next)
                .unwrap(),
            MarkdownTableWindowNavigationTarget::RequiredRow(MarkdownTableWindowCoordinate {
                row_ordinal: 1,
                column_index: 0
            })
        );
        assert_eq!(
            table_window_navigation_target(
                &window,
                MarkdownTableWindowCoordinate {
                    column_index: 0,
                    ..header
                },
                MarkdownTableWindowNavigation::Previous
            )
            .unwrap(),
            MarkdownTableWindowNavigationTarget::TableStart
        );
    }

    #[test]
    fn admitted_row_with_missing_metadata_is_not_requeued_as_an_absent_neighbor() {
        let mut window = window(&[500]);
        window.cells.pop();
        let current = MarkdownTableWindowCoordinate {
            row_ordinal: 500,
            column_index: 0,
        };
        assert_eq!(
            table_window_navigation_target(&window, current, MarkdownTableWindowNavigation::Next),
            Err(MarkdownTableWindowError::ParserMismatch)
        );
    }
}
