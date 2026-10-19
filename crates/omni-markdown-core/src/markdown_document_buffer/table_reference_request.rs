//! 一个真实全局定义的有限解析事务；不持文档/root强引用，不创建全文引用目录。
//!
//! 调用者只可将同一source identity驱动的global producer事实交到这里，并按源顺序处理。
//! 本模块再次核弱root/代次，但opaque事实没有跨文档root品牌，不能自行证明错误配对的事实。
//! 任何Err都须令外层全局请求失败；不得跳过它去采用后继同名定义或把未知声明为Missing。

use super::{MarkdownDocumentBuffer, table_source::MarkdownTableSourceIdentity};
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowError as Error, TABLE_WINDOW_MAX_REFERENCE_BYTES,
    references::MarkdownTableReferenceQueries as Queries,
};
use pulldown_cmark::{
    block_frontier::SourceFrame,
    global_frontier::{
        GlobalSourceAuthority, ReferencePrefixPurpose, ReferencePrefixScanner, ReferencePrefixStep,
        VerifiedGlobalReference,
    },
};
use std::ops::Range;

const MIN_STEP: usize = 4 * 1_024;
const MAX_STEP: usize = 64 * 1_024;
const STATE_WORK: usize = 1_024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// source/copy/结构工作分别报告。normalizer与Parser是有限whole-slice oracle，输入单独计量；
/// 不把它们内部的AST/Unicode转换伪称为这里已经逐token计数的转换fuel。
pub(super) struct ReferenceWork {
    pub source_bytes: usize,
    pub copied_bytes: usize,
    pub metadata_bytes: usize,
    pub source_tree_steps: usize,
    pub structural_transitions: usize,
    pub normalizer_input_bytes: usize,
    pub parser_input_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReferenceStep {
    pub work: ReferenceWork,
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    CopyLabel,
    ScanLabel,
    ProbeLabel,
    NormalizeLabel,
    CopyDefinition,
    ScanDefinition,
    ProbeDefinition,
    StripDefinition,
    DecodeDefinition,
    Complete,
}

#[derive(Clone, Copy, Debug)]
/// 只保一个真实续行位置及已验证skip；不保行正文或flatten的深容器角色。
struct PrefixRecord {
    at: usize,
    bytes: usize,
}

struct Buffer {
    range: Range<usize>,
    text: String,
    scan_at: usize,
    prefixes: Vec<PrefixRecord>,
}

impl Buffer {
    fn new(range: Range<usize>, maximum: usize) -> Result<Self, Error> {
        let len = range
            .end
            .checked_sub(range.start)
            .ok_or(Error::InvalidSource)?;
        if len == 0 || len > maximum {
            // 先核完整范围再预留；截断标签/URL不能获得原规范化器的虚拟EOF权限。
            return Err(Error::ReferenceLimit);
        }
        Ok(Self {
            range,
            text: String::with_capacity(len),
            scan_at: 0,
            prefixes: Vec::new(),
        })
    }
}

pub(super) struct Request {
    source: MarkdownTableSourceIdentity,
    fact: Option<VerifiedGlobalReference>,
    buffer: Option<Buffer>,
    probe: Option<ReferencePrefixScanner>,
    pending_line: Option<usize>,
    normalized: Option<String>,
    stripped: Option<String>,
    strip_at: usize,
    strip_record: usize,
    phase: Phase,
    failure: Option<Error>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 默认诊断只有状态/有限容量，不打印标签、URL、title或个人源字节。
        formatter
            .debug_struct("ReferenceRequest")
            .field("source", &self.source)
            .field("phase", &self.phase)
            .field("failure", &self.failure)
            .field(
                "source_buffer_bytes",
                &self.buffer.as_ref().map_or(0, |buffer| buffer.text.len()),
            )
            .field(
                "prefix_records",
                &self
                    .buffer
                    .as_ref()
                    .map_or(0, |buffer| buffer.prefixes.len()),
            )
            .finish()
    }
}

impl Request {
    /// ASCII计数不可能命中时零正文复制。possible-match范围超限是未完成错误，不能当作miss。
    pub(super) fn new(
        source: MarkdownTableSourceIdentity,
        fact: VerifiedGlobalReference,
        queries: &Queries,
    ) -> Result<Option<Self>, Error> {
        let candidate = fact.candidate();
        let label_end = candidate
            .label_end
            .checked_add(1)
            .ok_or(Error::InvalidSource)?;
        if candidate.source_revision != source.generation()
            || candidate.span_start > candidate.label_start
            || candidate.label_start >= label_end
            || label_end > candidate.span_end
            || candidate.span_end > source.byte_len()
        {
            // 事实必须来自当前source驱动；即使无关标签，也不接受倒序或另一代次的范围。
            return Err(Error::InvalidSource);
        }
        if !queries.may_match_ascii_count(candidate.label_ordinary_ascii) {
            return Ok(None); // 无关巨URL/title不读取、不分配，也不消耗有限值库存。
        }
        let buffer = Buffer::new(
            candidate.label_start..label_end,
            queries.maximum_candidate_label_bytes(),
        )?;
        Ok(Some(Self {
            source,
            fact: Some(fact),
            buffer: Some(buffer),
            probe: None,
            pending_line: None,
            normalized: None,
            stripped: None,
            strip_at: 0,
            strip_record: 0,
            phase: Phase::CopyLabel,
            failure: None,
        }))
    }

    /// 每step只做一个阶段；预算预留源树真实缓存height和固定状态/共享scope收尾成本。
    /// 取消由外层请求在调用前检查；这里在每次入口（包括Complete）继续核实际root/代次。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        queries: &mut Queries,
        budget: usize,
    ) -> Result<ReferenceStep, Error> {
        if let Some(error) = self.failure {
            return Err(error); // 首错不能被后继短定义或重新交付frame修复。
        }
        let result = self.advance_inner(document, queries, budget);
        if let Err(error) = result {
            // 原库存也撤权，防外层误忽略Err后调用EOF或另一个Request发布部分结果。
            queries.fail_verified_resolution();
            self.failure = Some(error);
            self.release_payload();
        }
        result
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        queries: &mut Queries,
        budget: usize,
    ) -> Result<ReferenceStep, Error> {
        if !self.source.is_current(document) {
            return Err(Error::InvalidSource);
        }
        if !(MIN_STEP..=MAX_STEP).contains(&budget) {
            return Err(Error::SourceLimit);
        }
        let tree_work = document
            .root
            .as_ref()
            .map_or(0, |root| usize::from(root.height()))
            + 1;
        let available = budget
            .checked_sub(STATE_WORK + tree_work)
            .ok_or(Error::SourceLimit)?;
        if available < 2_048 {
            return Err(Error::SourceLimit); // 深源树不能挤占prefix scanner的真实最小fuel。
        }
        let mut work = ReferenceWork {
            structural_transitions: STATE_WORK,
            ..ReferenceWork::default()
        };
        match self.phase {
            Phase::CopyLabel | Phase::CopyDefinition => {
                self.copy_source(document, available, tree_work, &mut work)?;
            }
            Phase::ScanLabel | Phase::ScanDefinition => self.scan_lines(available, &mut work)?,
            Phase::ProbeLabel | Phase::ProbeDefinition => {
                self.advance_prefix(document, available, tree_work, &mut work)?;
            }
            Phase::NormalizeLabel => self.normalize(queries, &mut work)?,
            Phase::StripDefinition => self.strip(available, &mut work)?,
            Phase::DecodeDefinition => {
                let label = self.normalized.as_deref().ok_or(Error::InvalidSource)?;
                let value = self.stripped.as_deref().ok_or(Error::InvalidSource)?;
                // 原Parser只接已匹配的≤16KiB完整定义，继续负责entity/escape/title/首定义解码。
                // parser_input_bytes是独立oracle输入上限，不声称其内部工作逐token服从available。
                work.parser_input_bytes = value.len();
                queries.admit_verified_definition(label, value)?;
                self.release_payload();
                self.phase = Phase::Complete;
            }
            Phase::Complete => (), // Ready不恢复旧source或分配已释放载荷。
        }
        debug_assert!(work.source_bytes <= budget);
        debug_assert!(work.structural_transitions + work.source_tree_steps <= budget);
        Ok(ReferenceStep {
            work,
            complete: self.phase == Phase::Complete,
        })
    }

    fn copy_source(
        &mut self,
        document: &MarkdownDocumentBuffer,
        available: usize,
        tree_work: usize,
        work: &mut ReferenceWork,
    ) -> Result<(), Error> {
        let buffer = self.buffer.as_mut().ok_or(Error::InvalidSource)?;
        let next = buffer.range.start + buffer.text.len();
        if next == buffer.range.end {
            self.phase = if self.phase == Phase::CopyLabel {
                Phase::ScanLabel
            } else {
                Phase::ScanDefinition
            };
            return Ok(());
        }
        // 一次树下降加最多两次UTF8验证/一次复制；frame可切scalar但本buffer始终在scalar边界结束。
        let limit = (available / 3).min(buffer.range.end - next);
        let frame = document
            .table_source_frame(next, limit)
            .map_err(|_| Error::InvalidSource)?;
        work.source_tree_steps = tree_work;
        work.source_bytes = frame.bytes.len();
        let text = match std::str::from_utf8(&frame.bytes) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => {
                // 仅丢回frame尾≤3byte未完成scalar，下step从同源精确位置重借；不替换或丢源字节。
                std::str::from_utf8(&frame.bytes[..error.valid_up_to()])
                    .map_err(|_| Error::InvalidSource)?
            }
            Err(_) => return Err(Error::InvalidSource),
        };
        if text.is_empty() {
            return Err(Error::InvalidSource); // 非空范围没有进展不能忙循环。
        }
        work.structural_transitions += frame.bytes.len() * 3;
        work.copied_bytes = text.len();
        buffer.text.push_str(text);
        frame.finish().map_err(|_| Error::InvalidSource)?;
        Ok(())
    }

    fn scan_lines(&mut self, available: usize, work: &mut ReferenceWork) -> Result<(), Error> {
        let buffer = self.buffer.as_mut().ok_or(Error::InvalidSource)?;
        let bytes = buffer.text.as_bytes();
        let mut used = 0;
        while buffer.scan_at < bytes.len() && used + 2 <= available {
            let at = buffer.scan_at;
            let byte = bytes[at];
            let width = if byte == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            buffer.scan_at += width;
            used += 2;
            work.metadata_bytes += width;
            if matches!(byte, b'\r' | b'\n') && buffer.scan_at < bytes.len() {
                // 这里只识别真实行锚点；容器/空白规则仍交给opaque事实携带的原prefix scanner。
                self.pending_line = Some(buffer.range.start + buffer.scan_at);
                self.phase = if self.phase == Phase::ScanLabel {
                    Phase::ProbeLabel
                } else {
                    Phase::ProbeDefinition
                };
                break;
            }
        }
        work.structural_transitions += used;
        if buffer.scan_at == bytes.len() && self.pending_line.is_none() {
            self.phase = if self.phase == Phase::ScanLabel {
                Phase::NormalizeLabel
            } else {
                Phase::StripDefinition
            };
        }
        Ok(())
    }

    fn advance_prefix(
        &mut self,
        document: &MarkdownDocumentBuffer,
        available: usize,
        tree_work: usize,
        work: &mut ReferenceWork,
    ) -> Result<(), Error> {
        let line = self.pending_line.ok_or(Error::InvalidSource)?;
        if self.probe.is_none() {
            let purpose = if self.phase == Phase::ProbeLabel {
                ReferencePrefixPurpose::Label
            } else {
                ReferencePrefixPurpose::ContainerOnly
            };
            self.probe = Some(
                self.fact
                    .as_ref()
                    .ok_or(Error::InvalidSource)?
                    .prefix_scanner(line, purpose)
                    .map_err(|_| Error::ParserMismatch)?,
            );
            return Ok(()); // 固定游标建立与实际源读取分step，不能隐藏deep scope展开。
        }
        let probe = self.probe.as_mut().ok_or(Error::InvalidSource)?;
        let frame = document
            .table_source_frame(probe.next_byte(), available)
            .map_err(|_| Error::InvalidSource)?;
        work.source_tree_steps = tree_work;
        let step = probe
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
                available,
            )
            .map_err(|_| Error::ParserMismatch);
        // 回收成功后才消费step，不能让原parser提前终止遮住真实映射错误。
        frame.finish().map_err(|_| Error::InvalidSource)?;
        let step = step?;
        let (child_work, completed) = match step {
            ReferencePrefixStep::Progress { work, .. }
            | ReferencePrefixStep::NeedInput { work, .. } => (work, None),
            ReferencePrefixStep::Complete { prefix_bytes, work } => (work, Some(prefix_bytes)),
        };
        work.source_bytes = child_work.byte_reads;
        work.structural_transitions += child_work.transitions;
        if let Some(prefix) = completed {
            let buffer = self.buffer.as_mut().ok_or(Error::InvalidSource)?;
            let relative = line
                .checked_sub(buffer.range.start)
                .ok_or(Error::InvalidSource)?;
            let end = relative.checked_add(prefix).ok_or(Error::InvalidSource)?;
            if end > buffer.text.len()
                || !buffer.text.is_char_boundary(end)
                || buffer.prefixes.len() >= buffer.text.len()
            {
                return Err(Error::ParserMismatch);
            }
            if buffer.prefixes.is_empty() {
                // 每条记录至少对应一个实际EOL；总payload≤有限raw字节数×size_of(PrefixRecord)。
                // 常见单行定义完全不分配此Vec，不能把16KiB正文上限误称全部metadata内存。
                buffer.prefixes = Vec::with_capacity(buffer.text.len());
            }
            buffer.prefixes.push(PrefixRecord {
                at: line,
                bytes: prefix,
            });
            self.probe = None;
            self.pending_line = None;
            self.phase = if self.phase == Phase::ProbeLabel {
                Phase::ScanLabel
            } else {
                Phase::ScanDefinition
            };
        }
        Ok(())
    }

    fn normalize(&mut self, queries: &Queries, work: &mut ReferenceWork) -> Result<(), Error> {
        let buffer = self.buffer.as_ref().ok_or(Error::InvalidSource)?;
        work.normalizer_input_bytes = buffer.text.len();
        let normalized = self
            .fact
            .as_ref()
            .ok_or(Error::InvalidSource)?
            .normalized_label(&buffer.text, |after_eol| {
                // 原normalizer传入同一raw的suffix，只查已完成map；绝不在这里同步遍历深scope×全部续行。
                let relative = buffer.text.len().checked_sub(after_eol.len())?;
                let at = buffer.range.start.checked_add(relative)?;
                let index = buffer
                    .prefixes
                    .binary_search_by_key(&at, |record| record.at)
                    .ok()?;
                Some(buffer.prefixes[index].bytes)
            })
            .map_err(|_| Error::ParserMismatch)?;
        if !queries.needs_definition(&normalized)? {
            self.release_payload();
            self.phase = Phase::Complete;
            return Ok(()); // 无关/已解析定义到此停止，不读URL或title。
        }
        let normalized = normalized.into_string();
        let candidate = *self.fact.as_ref().ok_or(Error::InvalidSource)?.candidate();
        // 命中后才核并分配完整候选；过大的第一定义不可跳过后改采用短定义。
        let value_len = candidate
            .span_end
            .checked_sub(candidate.span_start)
            .ok_or(Error::InvalidSource)?;
        if value_len > TABLE_WINDOW_MAX_REFERENCE_BYTES {
            return Err(Error::ReferenceLimit);
        }
        self.buffer = None; // 先drop旧label/raw/prefix容量，阶段切换不叠两个完整source buffer。
        self.buffer = Some(Buffer::new(
            candidate.span_start..candidate.span_end,
            TABLE_WINDOW_MAX_REFERENCE_BYTES,
        )?);
        self.normalized = Some(normalized);
        self.phase = Phase::CopyDefinition;
        Ok(())
    }

    fn strip(&mut self, available: usize, work: &mut ReferenceWork) -> Result<(), Error> {
        let buffer = self.buffer.as_ref().ok_or(Error::InvalidSource)?;
        let value = self
            .stripped
            .get_or_insert_with(|| String::with_capacity(buffer.text.len()));
        let mut used = 0;
        while self.strip_at < buffer.text.len() && used + 8 <= available {
            if let Some(record) = buffer.prefixes.get(self.strip_record)
                && record.at == buffer.range.start + self.strip_at
            {
                // 只删除实际ContainerOnly证明的前缀，额外indent留给原Parser处理label/title语义。
                self.strip_at += record.bytes;
                self.strip_record += 1;
                used += 1;
                continue;
            }
            let next_prefix = buffer
                .prefixes
                .get(self.strip_record)
                .map_or(buffer.text.len(), |record| record.at - buffer.range.start);
            let mut end = (self.strip_at + (available - used) / 2).min(next_prefix);
            while !buffer.text.is_char_boundary(end) {
                end -= 1; // 有限UTF8尾最多退3byte；完整源字符保留到后继step，不截断String。
                used += 1;
            }
            if end <= self.strip_at {
                return Err(Error::InvalidSource); // 记录顺序/范围错误不能造成零进展复制循环。
            }
            value.push_str(&buffer.text[self.strip_at..end]);
            work.copied_bytes += end - self.strip_at;
            used += (end - self.strip_at) * 2;
            self.strip_at = end;
        }
        work.structural_transitions += used;
        if self.strip_at == buffer.text.len() {
            self.buffer = None; // Parser解码前释放原始candidate及prefix表，只保已去前缀的有限值。
            self.phase = Phase::DecodeDefinition;
        }
        Ok(())
    }

    fn release_payload(&mut self) {
        // 不持任何RefCell/源借用；关闭/错误/完成均同步释放所有正文与scope中间值。
        self.probe = None;
        self.buffer = None;
        self.normalized = None;
        self.stripped = None;
        self.fact = None;
        self.pending_line = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MarkdownTextSelection,
        markdown_viewport_presentation::parser::markdown_presentation_options,
    };
    use pulldown_cmark::{
        Parser,
        global_frontier::{
            GlobalFact, GlobalFrontier, GlobalFrontierLimits, GlobalStep, StructuralSpineLimits,
        },
    };

    fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("有限引用.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    fn facts(document: &MarkdownDocumentBuffer) -> Vec<VerifiedGlobalReference> {
        let mut global = GlobalFrontier::new(
            document.generation(),
            document.byte_len(),
            markdown_presentation_options(true),
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: 200_000_000,
                },
                max_byte_work: document.byte_len().saturating_mul(1000) + 1_000_000,
                max_transitions: document.byte_len().saturating_mul(10000) + 1_000_000,
            },
        )
        .unwrap();
        let mut result = Vec::new();
        for _ in 0..document.byte_len().saturating_mul(100) + 10000 {
            let frame = document
                .table_source_frame(global.next_byte(), 113)
                .unwrap();
            match global
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
                    4096,
                )
                .unwrap()
            {
                GlobalStep::Fact {
                    fact: GlobalFact::Reference(fact),
                    ..
                } => result.push(fact),
                GlobalStep::Complete { .. } => return result,
                _ => (),
            }
        }
        panic!("真实global producer未完成");
    }

    fn run(
        request: &mut Request,
        document: &MarkdownDocumentBuffer,
        queries: &mut Queries,
    ) -> Result<ReferenceWork, Error> {
        let mut total = ReferenceWork::default();
        for _ in 0..10000 {
            let step = request.advance(document, queries, 4096)?;
            assert!(step.work.source_bytes <= 4096);
            assert!(step.work.structural_transitions + step.work.source_tree_steps <= 4096);
            assert!(step.work.parser_input_bytes <= TABLE_WINDOW_MAX_REFERENCE_BYTES);
            total.source_bytes += step.work.source_bytes;
            total.copied_bytes += step.work.copied_bytes;
            total.metadata_bytes += step.work.metadata_bytes;
            total.source_tree_steps += step.work.source_tree_steps;
            total.structural_transitions += step.work.structural_transitions;
            total.normalizer_input_bytes += step.work.normalizer_input_bytes;
            total.parser_input_bytes += step.work.parser_input_bytes;
            if step.complete {
                return Ok(total);
            }
        }
        panic!("有限引用未完成");
    }

    #[test]
    fn actual_global_scopes_and_multiline_unicode_labels_match_original_value_parser() {
        for (source, label) in [
            (
                "[Straße]: <https://example.test/a\\>b?x=&amp;> '标题 &amp; \\'q\\''\n",
                "STRASSE",
            ),
            (
                "> [Maße\n>  Küche]: /first \"Title\n>  Next &amp;\"\n",
                "MASSE KÜCHE",
            ),
            ("- [r]:\n  /target\n  \"标题\n  第二行\"\n", "r"),
            ("[中]: <>\r\n", "中"),
        ] {
            let (_directory, document) = fixture(source);
            let mut queries = Queries::from_projection(&format!("[x][{label}]"), true).unwrap();
            let mut found = facts(&document);
            assert_eq!(found.len(), 1, "{source:?}");
            let mut request = Request::new(
                document.table_source_identity().unwrap(),
                found.remove(0),
                &queries,
            )
            .unwrap()
            .unwrap();
            let work = run(&mut request, &document, &mut queries).unwrap();
            assert!(queries.is_ready());
            let oracle = Parser::new_ext(source, markdown_presentation_options(true));
            let expected = oracle.reference_definitions().get(label).unwrap();
            assert_eq!(
                queries.resolve(label),
                Some((
                    expected.dest.as_ref(),
                    expected.title.as_deref().unwrap_or("")
                )),
                "{source:?}"
            );
            assert!(work.parser_input_bytes > 0);
            assert!(request.buffer.is_none() && request.probe.is_none() && request.fact.is_none());
        }
    }

    #[test]
    fn giant_unrelated_payload_is_never_copied_and_normalized_miss_does_not_read_value() {
        let source = format!(
            "[unrelated-label]: /{}\n[r]: /{}\n",
            "x".repeat(1024 * 1024),
            "y".repeat(1024 * 1024)
        );
        let (_directory, document) = fixture(&source);
        let mut queries = Queries::from_projection("[z]", true).unwrap();
        let mut found = facts(&document);
        assert_eq!(found.len(), 2);
        assert!(
            Request::new(
                document.table_source_identity().unwrap(),
                found.remove(0),
                &queries
            )
            .unwrap()
            .is_none()
        );
        let mut request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        let work = run(&mut request, &document, &mut queries).unwrap();
        assert_eq!(work.parser_input_bytes, 0);
        assert!(work.copied_bytes <= 2, "只复制r和closer，不读巨大的无关URL");
        assert!(!queries.is_ready());
    }

    #[test]
    fn earliest_matching_oversize_definition_is_sticky_even_with_later_short_definition() {
        let source = format!(
            "[r]: /{}\n[r]: /later\n",
            "x".repeat(TABLE_WINDOW_MAX_REFERENCE_BYTES + 1)
        );
        let (_directory, document) = fixture(&source);
        let mut found = facts(&document);
        let mut queries = Queries::from_projection("[r]", true).unwrap();
        let mut request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            run(&mut request, &document, &mut queries),
            Err(Error::ReferenceLimit)
        );
        assert_eq!(
            request.advance(&document, &mut queries, 4096),
            Err(Error::ReferenceLimit)
        );
        assert!(request.buffer.is_none() && request.fact.is_none());
        assert!(
            Request::new(
                document.table_source_identity().unwrap(),
                found.remove(0),
                &queries
            )
            .unwrap()
            .is_none()
        );
        queries.finish_verified_eof();
        assert!(!queries.is_ready());
        assert!(queries.resolve("r").is_none());
    }

    #[test]
    fn source_identity_rejects_other_document_edit_and_undo_without_keeping_root_alive() {
        let (_directory, mut document) = fixture("[r]: /target\n");
        let mut found = facts(&document);
        let mut queries = Queries::from_projection("[r]", true).unwrap();
        let mut request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        let (_other_directory, other) = fixture("[r]: /target\n");
        assert_eq!(document.generation(), other.generation());
        assert_eq!(
            request.advance(&other, &mut queries, 4096),
            Err(Error::InvalidSource)
        );
        assert!(request.buffer.is_none());
        let mut queries = Queries::from_projection("[r]", true).unwrap();
        let mut found = facts(&document);
        let mut request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        document
            .insert_utf16(0, "x", MarkdownTextSelection::caret(1))
            .unwrap();
        assert!(document.undo_text_edit().is_some());
        assert_eq!(
            request.advance(&document, &mut queries, 4096),
            Err(Error::InvalidSource)
        );
        let mut found = facts(&document);
        let queries = Queries::from_projection("[r]", true).unwrap();
        let request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        let weak = std::sync::Arc::downgrade(document.root.as_ref().unwrap());
        drop(document);
        assert!(
            weak.upgrade().is_none(),
            "Request只保Weak身份和结构scope，不延长正文/mmap生命周期"
        );
        drop(request);
    }
}
