//! Move one real GCB inside the original visual part; a part edge grants no line or document edge authority.

use super::*;

mod line;
pub use line::{
    MarkdownComposedVisualAnswer, MarkdownComposedVisualQuery, MarkdownComposedVisualStep,
};
#[cfg(feature = "paged-shaping-frontier")]
mod word;
#[cfg(feature = "paged-shaping-frontier")]
pub use word::{
    MarkdownComposedVisualWordAnswer, MarkdownComposedVisualWordQuery,
    MarkdownComposedVisualWordStep,
};

/// The logical grapheme before/after this position, independent of upstream/downstream soft-wrap affinity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedCaretAssociation {
    Before,
    After,
}

/// A side from actual L1/L2 parts; callers must still authenticate Source, input serial and layout geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownComposedCaretSide {
    pub level: u8,
    pub association: MarkdownComposedCaretAssociation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedVisualGraphemeAnswer {
    Pending,
    /// Only this part has ended; an adjacent part or line must come from the same real output.
    Edge,
    Found {
        utf16: usize,
        side: MarkdownComposedCaretSide,
    },
}

/// 外部 caret 必须是真 GCB；整行内部进入真实视觉 part 时，还可跳过其非 GCB 整形端点。
#[derive(Clone, Copy)]
enum GraphemeQueryStart {
    Caret,
    PartEntry { inclusive: bool },
}

/// A weak part/directory query retaining one GCB record across suspension, without a reader or text.
pub struct MarkdownComposedVisualGraphemeQuery {
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    registration: MarkdownLineFontRegistration,
    range: Range<usize>,
    level: u8,
    utf16: usize,
    logical_forward: bool,
    folded_section: bool,
    bytes: usize,
    units: usize,
    search: graphemes::BoundarySearch,
    locating: bool,
    start: GraphemeQueryStart,
    #[cfg(feature = "paged-shaping-frontier")]
    origin: Option<(usize, crate::MarkdownGraphemeBoundary)>,
    #[cfg(feature = "paged-shaping-frontier")]
    neighbor: Option<(usize, crate::MarkdownGraphemeBoundary)>,
    answer: Option<MarkdownComposedVisualGraphemeAnswer>,
    failed: bool,
}

pub struct MarkdownComposedVisualGraphemeStep {
    pub records: usize,
    pub answer: MarkdownComposedVisualGraphemeAnswer,
}

impl MarkdownComposedLines {
    /// Follow this authenticated part direction for one GCB; do not infer adjacency across font or UBA boundaries.
    ///
    /// # Errors
    /// Foreign parts, Source/font errors, cancellation and directory IO retain the original failure and revoke output;
    /// a non-GCB or out-of-part position rejects this input without poisoning a healthy output.
    pub fn visual_grapheme_query(
        &mut self,
        part: &MarkdownComposedLinePart,
        utf16: usize,
        right: bool,
    ) -> Result<MarkdownComposedVisualGraphemeQuery> {
        let result = (|| {
            self.require(&part.source)?;
            if !Weak::ptr_eq(&part.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&part.registration)?;
            let range = &part.facts.source_utf16;
            if utf16 < range.start || utf16 > range.end {
                return Err(Error::InvalidRequest);
            }
            let folded_section =
                part.facts.kind == crate::MarkdownComposedLinePartKind::FoldedSection;
            if folded_section && utf16 != range.start && utf16 != range.end {
                return Err(Error::InvalidRequest);
            } // 折叠控制只有两个可见端点；原 Source 的内部 GCB 不授予隐藏 caret。
            let logical_forward = match part.facts.direction {
                MarkdownShapingDirection::LeftToRight => right,
                MarkdownShapingDirection::RightToLeft => !right,
                _ => return Err(Error::InconsistentCoverage),
            };
            let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
            Ok(MarkdownComposedVisualGraphemeQuery {
                output: part.output.clone(),
                source: part.source.clone(),
                registration: part.registration.clone(),
                range: range.clone(),
                level: part.facts.level,
                utf16,
                logical_forward,
                folded_section,
                bytes: source.byte_len(),
                units: source.utf16_len(),
                search: graphemes::BoundarySearch::offset(utf16),
                locating: true,
                start: GraphemeQueryStart::Caret,
                #[cfg(feature = "paged-shaping-frontier")]
                origin: None,
                #[cfg(feature = "paged-shaping-frontier")]
                neighbor: None,
                answer: None,
                failed: false,
            })
        })();
        if matches!(result, Err(Error::InvalidRequest)) {
            self.require(&part.source)?;
            return result; // Invalid external positions reject this action without poisoning the healthy Source/reader.
        }
        self.finish(result)
    }

    /// Advance the same part under one record budget; quota one may suspend at any original link.
    pub fn step_visual_grapheme(
        &mut self,
        query: &mut MarkdownComposedVisualGraphemeQuery,
        budget: usize,
    ) -> Result<MarkdownComposedVisualGraphemeStep> {
        let result = (|| {
            self.require(&query.source)?;
            if query.failed {
                return Err(Error::Poisoned);
            }
            if !Weak::ptr_eq(&query.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&query.registration)?;
            if budget == 0 || budget > STEP {
                return Err(Error::InvalidRequest);
            }
            let mut records = 0;
            while query.answer.is_none() && records < budget {
                if query.utf16 > self.graphemes.covered.utf16 {
                    break; // This snapshot lacks the GCB; only the original Producer can publish a later prefix.
                }
                let step = query.search.step(
                    self.graphemes,
                    query.bytes,
                    query.units,
                    budget - records,
                    |record| self.row(record),
                )?;
                records += step.records;
                let Some((ordinal, value)) = step.answer else {
                    break;
                };
                if query.locating {
                    let exact = value.filter(|value| value.utf16 == query.utf16);
                    if let GraphemeQueryStart::PartEntry { inclusive } = query.start
                        && (inclusive || exact.is_none())
                    {
                        // lower_bound 给出真实后继 GCB；向左进入 RTL/LTR part 时按其逻辑方向取前驱。
                        // 不把内部标量 seam 冒充起点，也不把后继边界搬进当前 part。
                        let ordinal = if query.logical_forward || exact.is_some() {
                            ordinal
                        } else {
                            ordinal.checked_sub(1).ok_or(Error::InconsistentCoverage)?
                        };
                        query.search = graphemes::BoundarySearch::ordinal(ordinal);
                        query.locating = false;
                        continue;
                    }
                    let boundary = exact.ok_or(Error::InvalidRequest)?;
                    #[cfg(feature = "paged-shaping-frontier")]
                    {
                        query.origin = Some((ordinal, boundary));
                    } // 只记录原目录实际交付的双坐标；最终 caret 在 Bidi 边界可能改到另一等价侧。
                    let edge = if query.logical_forward {
                        query.range.end
                    } else {
                        query.range.start
                    };
                    if boundary.utf16 == edge {
                        query.answer = Some(MarkdownComposedVisualGraphemeAnswer::Edge);
                        break;
                    }
                    if query.folded_section {
                        // 仍查询同一输出的真实端点 GCB；不能把整个隐区当一个普通字符后取内部 ordinal。
                        query.search = graphemes::BoundarySearch::offset(edge);
                    } else {
                        let ordinal = if query.logical_forward {
                            ordinal.checked_add(1).ok_or(Error::Overflow)?
                        } else {
                            ordinal.checked_sub(1).ok_or(Error::InconsistentCoverage)?
                        };
                        query.search = graphemes::BoundarySearch::ordinal(ordinal);
                    }
                    query.locating = false;
                    continue; // The next ordinal still consumes the remaining budget of this same turn.
                }
                let boundary = value.ok_or(Error::InconsistentCoverage)?;
                let next = boundary.utf16;
                #[cfg(feature = "paged-shaping-frontier")]
                {
                    query.neighbor = Some((ordinal, boundary));
                }
                if next < query.range.start || next > query.range.end {
                    // 真实 GCB 可以跨两个 UBA/字体 part；本 part 无权签跨域 caret，
                    // 由原整行查询继续寻找邻接 part。内部 seam 绝不变成可编辑的位置。
                    query.answer = Some(MarkdownComposedVisualGraphemeAnswer::Edge);
                    break;
                }
                if (query.folded_section
                    && next
                        != if query.logical_forward {
                            query.range.end
                        } else {
                            query.range.start
                        })
                    || (matches!(query.start, GraphemeQueryStart::Caret)
                        && ((query.logical_forward && next <= query.utf16)
                            || (!query.logical_forward && next >= query.utf16)))
                {
                    return Err(Error::InconsistentCoverage);
                }
                query.answer = Some(MarkdownComposedVisualGraphemeAnswer::Found {
                    utf16: next,
                    side: MarkdownComposedCaretSide {
                        level: query.level,
                        association: if matches!(query.start, GraphemeQueryStart::PartEntry { .. })
                            && next == query.range.start
                        {
                            MarkdownComposedCaretAssociation::After
                        } else if matches!(query.start, GraphemeQueryStart::PartEntry { .. })
                            && next == query.range.end
                            || query.logical_forward
                        {
                            MarkdownComposedCaretAssociation::Before
                        } else {
                            MarkdownComposedCaretAssociation::After
                        },
                    },
                });
            }
            self.require(&query.source)?;
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&query.registration)?;
            Ok(MarkdownComposedVisualGraphemeStep {
                records,
                answer: query
                    .answer
                    .unwrap_or(MarkdownComposedVisualGraphemeAnswer::Pending),
            })
        })();
        if result.is_err() {
            query.failed = true;
        }
        if matches!(result, Err(Error::InvalidRequest)) {
            self.require(&query.source)?;
            return result; // Reject this query only; do not poison healthy output or permit changing the rejected query position.
        }
        self.finish(result)
    }
}
