//! 当前标题的有限脚注查询；每个真实key独立重扫原Global，不保存全源定义表或定义正文。
//! 已证明的定义key直接流式casefold比较，极长无关key也不会被物化成String。

use super::*;
use pulldown_cmark::global_frontier::{InlineReferenceDecoder, VerifiedGlobalFootnoteDefinition};
use unicase::UniCase;

// label只属于当前标题的原规范化identifier；found=None是Unknown，不能解释为Missing。
// 原文定义不进入该库存，重复key沿原UniCase匹配后复用同一个真实答案。
struct Query {
    label: String,
    found: Option<bool>,
}

// 当前定义只保共享解码续体及比较水位；源语法已经由真实Global Fact闭合。
// 失配前缀不可能被后文修正，因此可立即放下这个decoder并继续原Global，无须读取剩余key。
struct Definition {
    decoder: InlineReferenceDecoder,
    end: usize,
    compared: usize,
}

pub(super) struct HeadingFootnotes {
    queries: Vec<Query>,
    query_bytes: usize,
    current: usize,
    folded: String,
    global: Option<GlobalFrontier>,
    definition: Option<Definition>,
    quota: MarkdownTableWindowQuota,
    failure: Option<Error>,
}

impl HeadingFootnotes {
    /// 此库存只收当前标题经原Parser/cursor认证的key；没有Doc、正文或历史强引用。
    pub(super) fn new(quota: MarkdownTableWindowQuota) -> Self {
        Self {
            queries: Vec::new(),
            query_bytes: 0,
            current: 0,
            folded: String::new(),
            global: None,
            definition: None,
            quota,
            failure: None,
        }
    }

    /// Fn回调只能表达Unknown；真实准入失败仍留原类型，Parser迭代后由拥有者立即返回。
    pub(super) fn lookup(&mut self, label: &str) -> Option<bool> {
        if let Some(query) = self
            .queries
            .iter()
            .find(|query| UniCase::new(query.label.as_str()) == UniCase::new(label))
        {
            return query.found;
        }
        if self.failure.is_some() {
            return None;
        }
        let bytes = self.query_bytes.checked_add(label.len());
        if bytes.is_none_or(|bytes| bytes > FRAGMENT_LIMIT) {
            self.failure = Some(Error::Semantic(
                super::super::table_window_request::MarkdownTableRequestError::Window(
                    crate::markdown_viewport_presentation::table_window::MarkdownTableWindowError::ReferenceLimit,
                ),
            ));
            return None;
        }
        // 只在目录满时按倍增分配新句柄页；旧字符串只移动所有权，不重复复制key。
        let capacity = if self.queries.len() == self.queries.capacity() {
            self.queries.capacity().saturating_mul(2).max(4)
        } else {
            0
        };
        let additional = label.len() + std::mem::size_of::<Query>() * capacity;
        if let Err(error) = self.quota.admit_metadata(additional) {
            self.failure = Some(Error::Semantic(
                super::super::table_window_request::MarkdownTableRequestError::Window(error),
            ));
            return None;
        }
        if capacity != 0 {
            let mut next = Vec::with_capacity(capacity);
            if let Err(error) = self
                .quota
                .admit_metadata((next.capacity() - capacity) * std::mem::size_of::<Query>())
            {
                self.failure = Some(Error::Semantic(
                    super::super::table_window_request::MarkdownTableRequestError::Window(error),
                ));
                return None;
            }
            next.append(&mut self.queries);
            self.queries = next;
        }
        self.query_bytes = bytes.unwrap();
        self.queries.push(Query {
            label: label.to_owned(),
            found: None,
        });
        None
    }

    /// 所有当前查询均已有Found或真实EOF的Missing；没有查询时不创建全局扫描或匿名资源。
    pub(super) fn is_ready(&self) -> bool {
        self.current == self.queries.len()
    }
    pub(super) fn take_failure(&mut self) -> Option<Error> {
        self.failure.take()
    }

    /// 一个key只有Found或真实Global EOF才能结束；前瞻Fact不抬高其它源的可解释水位。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        if self.is_ready() {
            return Ok((MarkdownHeadingAnchorWork::default(), true));
        }
        if self.definition.is_some() {
            return self.compare_definition(document, budget);
        }
        // 一个当前key独占一次原Global；串行重扫是明确可计费的CPU/I/O，换取有界内存和真实EOF。
        // 成功或失败都随原请求释放，不留跨文档缓存、后台任务或一份全源脚注目录。
        if self.global.is_none() {
            // 原UniCase的full casefold无上下文；只物化有限当前查询，不物化任何全源定义。
            self.quota
                .admit_metadata(self.queries[self.current].label.len().saturating_mul(4))
                .map_err(|error| {
                    Error::Semantic(
                        super::super::table_window_request::MarkdownTableRequestError::Window(
                            error,
                        ),
                    )
                })?;
            self.folded = UniCase::new(self.queries[self.current].label.as_str()).to_folded_case();
            let fuel = document
                .byte_len()
                .checked_mul(4096)
                .and_then(|n| n.checked_add(1024 * 1024))
                .ok_or(Error::ArithmeticOverflow)?;
            self.global = Some(
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
            );
            return Ok((
                MarkdownHeadingAnchorWork {
                    metadata_bytes: self.folded.capacity(),
                    structural_transitions: 128,
                    ..Default::default()
                },
                false,
            ));
        }
        let global = self.global.as_mut().unwrap();
        let frame = document.table_source_frame(global.next_byte(), PAGE)?;
        let source_bytes = frame.bytes.len();
        let step = global.advance(
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
        );
        frame.finish()?;
        // 这里只接真实Footnote定义存在性事实；正文、代码、HTML及相似裸字符都不能生成Found。
        // SourceFrame已finish，若此前读取/回收失败不会进入下面的成功路径。
        let (measured, fact) = match step.map_err(Error::Global)? {
            GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => (work, None),
            GlobalStep::Fact { fact, work } => (work, Some(fact)),
            GlobalStep::Complete { work } => {
                self.complete_query(false); // 唯一全源EOF授权Missing；当前屏幕没有定义不算Missing。
                return Ok((
                    MarkdownHeadingAnchorWork {
                        source_bytes,
                        structural_transitions: work.transitions + 128,
                        ..Default::default()
                    },
                    false,
                ));
            }
        };
        if let Some(GlobalFact::Footnote(fact)) = fact {
            self.definition = Some(Self::definition(fact));
        }
        Ok((
            MarkdownHeadingAnchorWork {
                source_bytes,
                structural_transitions: measured.transitions + 128,
                ..Default::default()
            },
            false,
        ))
    }

    fn definition(fact: VerifiedGlobalFootnoteDefinition) -> Definition {
        Definition {
            decoder: fact.reference_decoder(),
            end: fact.reference_range().end,
            compared: 0,
        }
    }

    /// 共享LabelScanner先产完整UTF8小片，再按原UniCase规则比较；一旦失配即可丢弃已认证完整定义。
    /// 后继字符不能修正已经不同的casefold前缀，因此不需要读取或保存定义剩余key。
    fn compare_definition(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let definition = self.definition.as_mut().unwrap();
        let next = definition.decoder.next_byte();
        let frame = document.table_source_frame(next, (definition.end - next).min(256))?;
        let source_bytes = frame.bytes.len();
        let mut output = [0; 1024];
        let step = definition.decoder.advance(
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
            &mut output,
            budget - 128,
        );
        frame.finish()?; // 当前借页归还后才展开decoder错误或进行后继文件/查询动作。
        let step = step.map_err(Error::Global)?;
        let text =
            std::str::from_utf8(&output[..step.written]).map_err(|_| Error::UnsupportedInline)?;
        self.quota
            .admit_metadata(text.len().saturating_mul(4))
            .map_err(|error| {
                Error::Semantic(
                    super::super::table_window_request::MarkdownTableRequestError::Window(error),
                )
            })?;
        let folded = UniCase::new(text).to_folded_case();
        let end = definition
            .compared
            .checked_add(folded.len())
            .ok_or(Error::ArithmeticOverflow)?;
        // casefold不会跨scalar重排字符；当前固定输出片与有限查询相同区间比较即可。
        // 只有decoder真正消费closing bracket、长度也相等时，才能结束为Found。
        let same = self.folded.as_bytes().get(definition.compared..end) == Some(folded.as_bytes());
        definition.compared = end;
        let exact = same && step.complete && end == self.folded.len();
        let work = MarkdownHeadingAnchorWork {
            source_bytes,
            copied_bytes: step.written + folded.len(),
            metadata_bytes: folded.capacity(),
            structural_transitions: step.work.transitions + folded.len() + 128,
            ..Default::default()
        };
        if exact {
            self.complete_query(true);
        } else if !same || step.complete {
            self.definition = None;
        }
        Ok((work, false))
    }

    /// 查询与全局源都只有一个当前拥有者；结论交付即归还当前定义decoder和结构页。
    fn complete_query(&mut self, found: bool) {
        self.queries[self.current].found = Some(found);
        self.current += 1;
        self.definition = None;
        self.global = None;
        self.folded = String::new();
    }
}
