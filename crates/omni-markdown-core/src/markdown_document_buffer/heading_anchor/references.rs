//! 一个当前标题的真实引用查询；有限 Queries 只收当前标签，不创建全篇定义 Map。

use super::*;
use crate::markdown_document_buffer::table_reference_request;
use crate::markdown_document_buffer::table_window_request::MarkdownTableRequestError;
use pulldown_cmark::CowStr;

pub(super) struct HeadingReferences {
    queries: MarkdownTableReferenceQueries,
    global: Option<GlobalFrontier>,
    pending: Option<table_reference_request::Request>,
}

impl HeadingReferences {
    /// 先收有限标题实际引用，再按同源 Global 的 first-wins 定义顺序补齐；后置定义同样可见。
    pub(super) fn new(
        document: &MarkdownDocumentBuffer,
        queries: MarkdownTableReferenceQueries,
    ) -> Result<Self, Error> {
        let fuel = document
            .byte_len()
            .checked_mul(4096)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(Error::ArithmeticOverflow)?;
        let global = if queries.is_ready() {
            None
        } else {
            Some(
                GlobalFrontier::new(
                    document.generation(),
                    document.byte_len(),
                    crate::markdown_viewport_presentation::parser::markdown_presentation_options(
                        true,
                    ),
                    GlobalFrontierLimits {
                        spine: StructuralSpineLimits {
                            max_depth: 16_384,
                            max_live_page_bytes: 4 * 1024 * 1024,
                            max_page_allocations: 1_000_000,
                            max_role_work: fuel,
                        },
                        max_byte_work: fuel,
                        max_transitions: fuel,
                    },
                )
                .map_err(Error::Global)?,
            )
        };
        Ok(Self {
            queries,
            global,
            pending: None,
        })
    }

    /// Parser 的 callback 生命周期只借本次有限结果；返回借用值，不复制另一个 URL/title 缓冲。
    pub(super) fn resolve<'a>(&'a self, label: &str) -> Option<(CowStr<'a>, CowStr<'a>)> {
        self.queries
            .resolve(label)
            .map(|(destination, title)| (destination.into(), title.into()))
    }

    /// 一个真实定义仍通过现有有限规范化/原 Parser；源页、复制与语义工作都交回原 heading 请求。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        if let Some(pending) = self.pending.as_mut() {
            let step = pending
                .advance(document, &mut self.queries, budget)
                .map_err(|error| Error::Semantic(MarkdownTableRequestError::Window(error)))?;
            work.source_bytes = step.work.source_bytes;
            work.copied_bytes = step.work.copied_bytes;
            work.parser_input_bytes = step.work.parser_input_bytes;
            work.structural_transitions += step.work.structural_transitions;
            if step.complete {
                self.pending = None;
            }
            return Ok((work, false));
        }
        if self.queries.is_ready() {
            self.global = None; // 真实最早定义已回答所有当前标签，无需扫描无关文尾。
            return Ok((work, true));
        }
        let global = self.global.as_mut().ok_or(Error::ArithmeticOverflow)?;
        let frame = document.table_source_frame(global.next_byte(), PAGE)?;
        let borrowed = frame.bytes.len();
        let step = global
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                GlobalSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
                budget - 128,
            )
            .map_err(Error::Global);
        frame.finish()?;
        let (measured, fact) = match step? {
            GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => (work, None),
            GlobalStep::Fact { fact, work } => (work, Some(fact)),
            GlobalStep::Complete { work } => {
                self.queries.finish_verified_eof(); // 只有这个原全局 EOF 才授权 Missing。
                self.global = None;
                return Ok((
                    MarkdownHeadingAnchorWork {
                        source_bytes: borrowed,
                        structural_transitions: work.transitions + 1,
                        ..Default::default()
                    },
                    true,
                ));
            }
        };
        work.source_bytes = borrowed;
        work.structural_transitions += measured.transitions;
        if let Some(GlobalFact::Reference(fact)) = fact {
            let source = document
                .table_source_identity()
                .ok_or(Error::ArithmeticOverflow)?;
            self.pending = table_reference_request::Request::new(source, fact, &self.queries)
                .map_err(|error| Error::Semantic(MarkdownTableRequestError::Window(error)))?;
        }
        Ok((work, false))
    }
}
