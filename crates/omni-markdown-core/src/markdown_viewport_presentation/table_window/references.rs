//! 当前有限投影实际需要的引用查询和值；不创建全文definition目录。

use crate::markdown_viewport_presentation::parser::MarkdownParserSeed;
use std::{cell::Cell, collections::HashMap};

use pulldown_cmark::{BrokenLink, Parser};
use unicase::UniCase;

use super::{
    MarkdownTableWindowError, TABLE_WINDOW_MAX_BYTES, TABLE_WINDOW_MAX_REFERENCE_BYTES,
    TABLE_WINDOW_MAX_UTF16,
};
use crate::markdown_viewport_presentation::parser::markdown_presentation_options;

const MAX_REFERENCE_QUERIES: usize = 1_024;
const MAX_REFERENCE_NORMALIZED_LABEL_BYTES: usize = TABLE_WINDOW_MAX_REFERENCE_BYTES + 1_000;

#[derive(Debug)]
enum ReferenceValue {
    Pending,
    Missing,
    Resolved { destination: String, title: String },
}

#[derive(Debug)]
/// key是固定UniCase实现产生的Unicode folded值，正文、源片段和definition文本均不保留。
pub(crate) struct MarkdownTableReferenceQueries {
    entries: HashMap<String, ReferenceValue>,
    key_bytes: usize,
    value_bytes: usize,
    maximum_folded_label_bytes: usize,
    pending: usize,
    finished: bool,
    failed: bool,
    collecting: bool,
    quota: Option<super::MarkdownTableWindowQuota>,
}

impl MarkdownTableReferenceQueries {
    /// 只解析有限投影；本层不把“投影里没有定义”误判为真实全文里不存在定义。
    #[cfg(test)]
    pub(crate) fn from_projection(
        projection: &str,
        actual_document_start: bool,
    ) -> Result<Self, MarkdownTableWindowError> {
        Self::from_projection_with_math(projection, actual_document_start, None)
    }

    /// 查询库存与最终渲染必须使用同一数学seed，否则省略row可能令一个链接被错误math吞掉。
    pub(crate) fn from_projection_with_math(
        projection: &str,
        actual_document_start: bool,
        math_seeds: Option<&[pulldown_cmark::math_context::TableMathSeed]>,
    ) -> Result<Self, MarkdownTableWindowError> {
        let mut result = Self::for_batch(None);
        result.collect_projection_with_math(projection, actual_document_start, math_seeds)?;
        result.seal_batch();
        Ok(result)
    }

    /// 同批直接向唯一库存收集；不先建第二窗Map再检测总key预算。
    pub(crate) fn collect_projection_with_math(
        &mut self,
        projection: &str,
        actual_document_start: bool,
        math_seeds: Option<&[pulldown_cmark::math_context::TableMathSeed]>,
    ) -> Result<(), MarkdownTableWindowError> {
        self.collect_with_options(
            projection,
            markdown_presentation_options(actual_document_start),
            math_seeds.map(MarkdownParserSeed::TableRows),
        )
    }

    /// 真实 Root Fact 保留自己的 options；查询与最终 Parser 必须共用它，不能重新启用扩展语法。
    pub(crate) fn from_bounded_inline(
        projection: &str,
        options: pulldown_cmark::Options,
        quota: super::MarkdownTableWindowQuota,
        seed: Option<MarkdownParserSeed<'_>>,
    ) -> Result<Self, MarkdownTableWindowError> {
        let mut result = Self::for_batch(Some(quota));
        result.collect_with_options(projection, options, seed)?;
        result.seal_batch();
        Ok(result)
    }

    /// 全源配对真正遇到含tick的小块时，在原库存补收该块的实际label；已证值和Missing不重置。
    pub(crate) fn collect_for_classification(
        &mut self,
        source: &str,
        options: pulldown_cmark::Options,
    ) -> Result<(), MarkdownTableWindowError> {
        self.collecting = true;
        let result = self.collect_with_options(source, options, None);
        self.seal_batch();
        if result.is_err() {
            self.failed = true; // 有限收集失败撤销整个原库存，不能发布只收了一部分label的配对。
        }
        result
    }

    /// 有限整块的原 Parser 只收集实际触发的引用；完整定义仍须原 Global 顺序确认。
    fn collect_with_options(
        &mut self,
        projection: &str,
        options: pulldown_cmark::Options,
        seed: Option<MarkdownParserSeed<'_>>,
    ) -> Result<(), MarkdownTableWindowError> {
        if !self.collecting || self.failed {
            return Err(MarkdownTableWindowError::ReferenceLimit);
        }
        if projection.len() > TABLE_WINDOW_MAX_BYTES
            || projection.encode_utf16().count() > TABLE_WINDOW_MAX_UTF16
        {
            // 所有Parser节点和callback都在源预算之后；巨行不能先解析再截取结果。
            return Err(MarkdownTableWindowError::SourceLimit);
        }
        let failed = Cell::new(false);
        let mut observe = |link: BrokenLink<'_>| {
            if failed.get() {
                // 首次超限后不再分配查询；外层在当前Parser事件返回后结束有限解析。
                return None;
            }
            if self.admit_query(&link.reference).is_err() {
                failed.set(true);
            }
            None
        };
        let mut parser = match seed {
            Some(MarkdownParserSeed::Cell(seed)) => {
                Parser::new_with_table_cell_seed(projection, seed, Some(&mut observe))
                    .map_err(|_| MarkdownTableWindowError::ParserMismatch)?
            }
            Some(MarkdownParserSeed::Atx(leaf)) => {
                Parser::new_with_atx_leaf_seed(projection, leaf, Some(&mut observe))
                    .map_err(|_| MarkdownTableWindowError::ParserMismatch)?
            }
            Some(MarkdownParserSeed::Scoped(context)) => {
                Parser::new_with_inline_block_seed(projection, context, Some(&mut observe))
                    .map_err(|_| MarkdownTableWindowError::ParserMismatch)?
            }
            Some(MarkdownParserSeed::TableRows(seeds)) => {
                Parser::new_with_table_math_seeds(projection, options, Some(&mut observe), seeds)
                    .map_err(|_| MarkdownTableWindowError::ParserMismatch)?
            }
            None => Parser::new_with_broken_link_callback(projection, options, Some(&mut observe)),
        };
        while parser.next().is_some() {
            if failed.get() {
                // 不继续收集部分查询；调用者必须丢弃整批，不能发布缺少链接的属性。
                return Err(MarkdownTableWindowError::ReferenceLimit);
            }
        }
        drop(parser);
        Ok(())
    }

    /// batch只收有限窗口实际需要的label，不建立全文definition目录。
    pub(crate) fn for_batch(quota: Option<super::MarkdownTableWindowQuota>) -> Self {
        Self {
            entries: HashMap::new(),
            key_bytes: 0,
            value_bytes: 0,
            maximum_folded_label_bytes: 0,
            pending: 0,
            finished: false,
            failed: false,
            collecting: true,
            quota,
        }
    }

    pub(crate) fn seal_batch(&mut self) {
        // 此后严禁新query，缺失结论只由一次完整源序遍历的真实EOF签发。
        self.collecting = false;
        self.finished = self.pending == 0;
    }

    fn admit_query(&mut self, normalized: &str) -> Result<(), MarkdownTableWindowError> {
        let folded = folded_label(normalized)?;
        if self.entries.contains_key(&folded) {
            // 同一窗口的重复链接和Unicode等价拼写共享一个查询，不重复收费或持有值。
            return Ok(());
        }
        if self.entries.len() == MAX_REFERENCE_QUERIES
            || self.key_bytes.saturating_add(folded.len()) > TABLE_WINDOW_MAX_REFERENCE_BYTES
        {
            // 固定entry数量和总key正文预算均在HashMap插入前检查。
            return Err(MarkdownTableWindowError::ReferenceLimit);
        }
        if let Some(quota) = &self.quota {
            quota.admit_metadata(folded.len() + std::mem::size_of::<(String, ReferenceValue)>())?;
        }
        self.maximum_folded_label_bytes = self.maximum_folded_label_bytes.max(folded.len());
        self.key_bytes += folded.len();
        self.pending += 1;
        self.entries.insert(folded, ReferenceValue::Pending);
        Ok(())
    }

    /// 已验证首个候选解析失败后撤销整个库存，不能忽略子请求错误再采用后继定义。
    pub(crate) fn fail_verified_resolution(&mut self) {
        self.failed = true;
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.finished && !self.failed && !self.collecting
    }

    /// 原定义解析器的真实目录和续行记录共用当前批次额度，不借另一份库存绕过上限。
    pub(crate) fn admit_resolution_metadata(
        &self,
        bytes: usize,
    ) -> Result<(), MarkdownTableWindowError> {
        if let Some(quota) = &self.quota {
            quota.admit_metadata(bytes)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 普通refdef的ASCII普通字节在规范化和folding后逐字节保留；超过任何查询不可能命中。
    /// 此方法只排除不可能的候选，不能证明global source中的block/member身份。
    pub(crate) fn may_match_ascii_count(&self, ordinary_ascii: usize) -> bool {
        !self.failed && !self.finished && ordinary_ascii <= self.maximum_folded_label_bytes
    }

    /// raw label计数由共享scanner给出：未保留的非普通ASCII字节总数严格小于1000。
    pub(crate) fn maximum_candidate_label_bytes(&self) -> usize {
        self.maximum_folded_label_bytes + 1_000
    }

    /// 匹配前仅处理有限label，不为无关/重复定义复制或解码destination、title。
    pub(crate) fn needs_definition(
        &self,
        normalized_label: &str,
    ) -> Result<bool, MarkdownTableWindowError> {
        if self.failed {
            return Err(MarkdownTableWindowError::ReferenceLimit);
        }
        let folded = folded_label(normalized_label)?;
        Ok(matches!(
            self.entries.get(&folded),
            Some(ReferenceValue::Pending)
        ))
    }

    /// 只接真实candidate完整span且已按真实scope去掉容器prefix的有限定义。
    /// 原Parser负责URL/title解码；不手写entity、反斜线或多行title的第二套规则。
    ///
    /// # Errors
    /// 最早匹配定义超预算或与原Parser不一致时撤销整请求，不能改采用后续重复定义。
    pub(crate) fn admit_verified_definition(
        &mut self,
        normalized_label: &str,
        definition: &str,
    ) -> Result<bool, MarkdownTableWindowError> {
        if !self.needs_definition(normalized_label)? {
            // caller应在读取完整span前走同一查询；这里再次核对以拒绝迟到的重复payload。
            return Ok(false);
        }
        if definition.len() > TABLE_WINDOW_MAX_REFERENCE_BYTES {
            self.failed = true;
            return Err(MarkdownTableWindowError::ReferenceLimit);
        }
        let parser = Parser::new_ext(definition, markdown_presentation_options(false));
        let Some(value) = parser.reference_definitions().get(normalized_label) else {
            // authority来自真实producer，有限值解析仍不一致时不能降级成“全源没有定义”。
            self.failed = true;
            return Err(MarkdownTableWindowError::ParserMismatch);
        };
        if value.span.start != 0 || value.span.end != definition.len() {
            // 一个candidate只允许自己的起点定义，不能把有限payload中的后继definition当替代。
            self.failed = true;
            return Err(MarkdownTableWindowError::ParserMismatch);
        }
        self.admit_verified_value(
            normalized_label,
            &value.dest,
            value.title.as_deref().unwrap_or(""),
        )
    }

    /// 实际global producer必须按源顺序提供已验证、已规范化的首个definition。
    /// 本内部入口只接有限解析后的值，不自行把截断片段当作合法definition。
    pub(crate) fn admit_verified_value(
        &mut self,
        normalized_label: &str,
        destination: &str,
        title: &str,
    ) -> Result<bool, MarkdownTableWindowError> {
        if self.failed {
            // 首个匹配定义一旦超限，后续定义不能修补或覆盖本批首因。
            return Err(MarkdownTableWindowError::ReferenceLimit);
        }
        let folded = folded_label(normalized_label)?;
        let Some(value) = self.entries.get_mut(&folded) else {
            // 无关定义不复制URL或title；它不消耗当前窗口有限值预算。
            return Ok(false);
        };
        if !matches!(value, ReferenceValue::Pending) {
            // first-wins在查询槽上执行，后续重复定义绝不覆盖已有值或重新分配。
            return Ok(false);
        }
        let bytes = destination.len().saturating_add(title.len());
        if self.value_bytes.saturating_add(bytes) > TABLE_WINDOW_MAX_REFERENCE_BYTES {
            // 最早匹配定义过大时整批失败，不能跳过它去采用更短的后续同名定义。
            self.failed = true;
            return Err(MarkdownTableWindowError::ReferenceLimit);
        }
        if let Some(quota) = &self.quota {
            quota.admit_metadata(bytes)?;
        }
        *value = ReferenceValue::Resolved {
            destination: destination.to_owned(),
            title: title.to_owned(),
        };
        self.value_bytes += bytes;
        self.pending -= 1;
        self.finished = self.pending == 0;
        Ok(true)
    }

    /// 只有真正global EOF才能把剩余Pending转为Missing；短frame或单个候选拒绝均不能调用。
    pub(crate) fn finish_verified_eof(&mut self) {
        if self.failed {
            // EOF只决定未知定义是否缺失，不撤销已经发生的预算失败。
            return;
        }
        for value in self.entries.values_mut() {
            if matches!(value, ReferenceValue::Pending) {
                // 已解析值保持不变，未知槽在真实EOF才获得“无定义”的确定语义。
                *value = ReferenceValue::Missing;
            }
        }
        self.pending = 0;
        self.finished = true;
    }

    /// Ready之前不暴露部分结果；最终callback借用有限值，不为每个链接复制URL/title。
    pub(crate) fn resolve(&self, normalized_label: &str) -> Option<(&str, &str)> {
        if self.failed || !self.finished || self.collecting {
            return None;
        }
        let folded = folded_label(normalized_label).ok()?;
        match self.entries.get(&folded)? {
            ReferenceValue::Resolved { destination, title } => Some((destination, title)),
            ReferenceValue::Pending | ReferenceValue::Missing => None,
        }
    }

    /// code/HTML优先级需要区分已证缺失和未收集的label；后者不能借普通callback的None伪作Missing。
    pub(crate) fn resolve_for_classification(
        &self,
        normalized_label: &str,
    ) -> Result<Option<(&str, &str)>, MarkdownTableWindowError> {
        if !self.is_ready() {
            return Err(MarkdownTableWindowError::ContextPending);
        }
        let folded = folded_label(normalized_label)?;
        match self.entries.get(&folded) {
            Some(ReferenceValue::Resolved { destination, title }) => Ok(Some((destination, title))),
            Some(ReferenceValue::Missing) => Ok(None),
            Some(ReferenceValue::Pending) | None => Err(MarkdownTableWindowError::ContextPending),
        }
    }
}

fn folded_label(normalized: &str) -> Result<String, MarkdownTableWindowError> {
    if normalized.len() > MAX_REFERENCE_NORMALIZED_LABEL_BYTES {
        // 原label允许<1000个被计数的非普通ASCII字节；Unicode拼写可比folded key长。
        // 多保1000字节有限余量，不能因K→k等折叠把合法同名标签误判成不相等。
        return Err(MarkdownTableWindowError::ReferenceLimit);
    }
    // 不以to_lowercase代替上游case folding。固定2.10.0每scalar最多3个folded scalar，
    // 因而scratch payload上界是12*(16KiB+1000)，不是“只分配16KiB”的声明。
    let folded = UniCase::new(normalized).to_folded_case();
    // key插入由admit_query的总量门禁负责。查询一个不可能存在的较长folded值仍返回“不匹配”，
    // 不能因为无关global definition的Unicode expansion而毒化当前窗口。
    Ok(folded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 同库存补收只增加新未知；旧真实Missing、已解析目标和新增定义的first-wins都保持有效。
    fn code_classification_extension_preserves_verified_queries_and_first_wins() {
        let mut queries =
            MarkdownTableReferenceQueries::from_projection("[当前][T] [原缺失][M]", true).unwrap();
        queries
            .admit_verified_value("T", "/current", "当前标题")
            .unwrap();
        queries.finish_verified_eof();
        assert_eq!(queries.resolve_for_classification("M"), Ok(None));
        queries
            .collect_for_classification("[前置][U] `</u>`", pulldown_cmark::Options::empty())
            .unwrap();
        assert!(!queries.is_ready());
        assert!(
            queries
                .admit_verified_value("U", "/first", "首定义")
                .unwrap()
        );
        assert!(
            !queries
                .admit_verified_value("U", "/later", "后定义")
                .unwrap()
        );
        queries.finish_verified_eof();
        assert_eq!(queries.resolve("T"), Some(("/current", "当前标题")));
        assert_eq!(queries.resolve("U"), Some(("/first", "首定义")));
        assert_eq!(queries.resolve_for_classification("M"), Ok(None));
        assert_eq!(
            queries.resolve_for_classification("未查询"),
            Err(MarkdownTableWindowError::ContextPending)
        );
    }

    #[test]
    /// 同一个空结果可能是已证缺失或根本没有查询权限；code分类只接受前者。
    fn code_classification_preserves_unknown_and_verified_missing() {
        let mut queries =
            MarkdownTableReferenceQueries::from_projection("[x][wanted]", true).unwrap();
        assert_eq!(
            queries.resolve_for_classification("wanted"),
            Err(MarkdownTableWindowError::ContextPending)
        );
        queries.finish_verified_eof();
        assert_eq!(queries.resolve_for_classification("WANTED"), Ok(None));
        assert_eq!(
            queries.resolve_for_classification("uncollected"),
            Err(MarkdownTableWindowError::ContextPending)
        );
    }

    #[test]
    fn actual_parser_labels_share_unicode_and_table_escape_queries() {
        // Parser自身决定shortcut/escaped-pipe规范化；单纯lowercase不能把Maße和MASSE配对。
        let source = "| [one][Maße] | [two][MASSE] | [pipe][a\\|b] |\n|---|---|---|\n";
        let mut queries = MarkdownTableReferenceQueries::from_projection(source, true).unwrap();
        assert_eq!(queries.len(), 2);
        assert!(!queries.is_ready());
        assert!(
            queries
                .admit_verified_value("masse", "/first", "标题")
                .unwrap()
        );
        assert!(
            !queries
                .admit_verified_value("Maße", "/later", "ignored")
                .unwrap()
        );
        assert!(queries.resolve("MASSE").is_none());
        assert!(queries.admit_verified_value("a|b", "/pipe", "").unwrap());
        assert!(queries.is_ready());
        assert_eq!(queries.resolve("Maße"), Some(("/first", "标题")));
        assert_eq!(queries.resolve("a|b"), Some(("/pipe", "")));
    }

    #[test]
    fn real_eof_is_required_for_missing_and_footnotes_remain_product_literal() {
        // GFM footnote不会走普通broken-link callback；既有产品将FootnoteReference显示为literal。
        let mut queries =
            MarkdownTableReferenceQueries::from_projection("[x][missing] [^foot]", true).unwrap();
        assert_eq!(queries.len(), 1);
        assert!(queries.may_match_ascii_count(7));
        assert!(!queries.may_match_ascii_count(8));
        assert!(!queries.is_ready());
        queries.finish_verified_eof();
        assert!(queries.is_ready());
        assert!(queries.resolve("missing").is_none());
        assert!(!queries.may_match_ascii_count(0));
    }

    #[test]
    fn query_and_value_budgets_reject_before_map_or_value_publication() {
        // aggregate key超限与首个definition过大都是整批失败，不得接受部分属性或后续短定义。
        let mut queries =
            MarkdownTableReferenceQueries::from_projection("[x][a] [y][b]", true).unwrap();
        let huge = "x".repeat(TABLE_WINDOW_MAX_REFERENCE_BYTES + 1);
        assert_eq!(
            queries.admit_query(&huge),
            Err(MarkdownTableWindowError::ReferenceLimit)
        );
        assert_eq!(queries.len(), 2);
        assert_eq!(
            queries.admit_verified_value("a", &huge, ""),
            Err(MarkdownTableWindowError::ReferenceLimit)
        );
        assert_eq!(queries.value_bytes, 0);
        assert!(!queries.is_ready());
        assert_eq!(
            queries.admit_verified_value("a", "/later", ""),
            Err(MarkdownTableWindowError::ReferenceLimit)
        );
        queries.finish_verified_eof();
        assert!(!queries.is_ready());
        let mut queries =
            MarkdownTableReferenceQueries::from_projection("[x][a] [y][b]", true).unwrap();
        let mut bytes = 2;
        let mut n = 0;
        while bytes + 1_024 <= TABLE_WINDOW_MAX_REFERENCE_BYTES {
            // 以有限长且互异的ASCII标签填满总额度；每次检查都发生在新map entry之前。
            let key = format!("{n:04}{}", "x".repeat(1_020));
            queries.admit_query(&key).unwrap();
            bytes += key.len();
            n += 1;
        }
        let count = queries.len();
        assert_eq!(
            queries.admit_query(&"q".repeat(1_024)),
            Err(MarkdownTableWindowError::ReferenceLimit)
        );
        assert_eq!(queries.len(), count);
    }

    #[test]
    fn finite_projection_inventory_matches_public_parser_reference_resolution() {
        // 完整Parser只在纯测试充当oracle；产品只保本投影查询且按真实源次序接收有限值。
        let projection =
            "| [中文][Straße] | [missing] |\n|---|---|\n| [x][STRASSE] | ![image][图] |\n";
        let definitions = "[Straße]: /first \"标题\"\n[STRASSE]: /second\n[图]: /image\n";
        let mut queries = MarkdownTableReferenceQueries::from_projection(projection, true).unwrap();
        let parser = Parser::new_ext(definitions, markdown_presentation_options(false));
        for label in ["Straße", "STRASSE", "图"] {
            // 上游RefDefs自身first-wins和Unicode折叠提供有限候选值的独立预期。
            let definition = parser.reference_definitions().get(label).unwrap();
            queries
                .admit_verified_value(
                    label,
                    &definition.dest,
                    definition.title.as_deref().unwrap_or(""),
                )
                .unwrap();
        }
        queries.finish_verified_eof();
        for label in ["Straße", "STRASSE", "图", "missing"] {
            let expected = parser
                .reference_definitions()
                .get(label)
                .map(|value| (value.dest.as_ref(), value.title.as_deref().unwrap_or("")));
            assert_eq!(queries.resolve(label), expected);
        }
    }

    #[test]
    fn exact_definition_span_uses_original_url_title_decoding_and_cannot_skip_a_bad_first_value() {
        let definition = "[r]: <https://example.test/a\\>b?x=&amp;> '标题 &amp; \\'quote\\''";
        let oracle = Parser::new_ext(definition, markdown_presentation_options(false));
        let expected = oracle.reference_definitions().get("r").unwrap();
        let mut queries = MarkdownTableReferenceQueries::from_projection("[r]", true).unwrap();
        assert!(queries.admit_verified_definition("r", definition).unwrap());
        assert_eq!(
            queries.resolve("r"),
            Some((
                expected.dest.as_ref(),
                expected.title.as_deref().unwrap_or("")
            ))
        );
        // 即使原Parser能从prefix找到r，携带额外正文的payload也不是candidate的完整精确span。
        let mut bad = MarkdownTableReferenceQueries::from_projection("[r]", true).unwrap();
        assert_eq!(
            bad.admit_verified_definition("r", "[r]: /first\nnot-this-definition"),
            Err(MarkdownTableWindowError::ParserMismatch)
        );
        assert_eq!(
            bad.admit_verified_definition("r", "[r]: /later"),
            Err(MarkdownTableWindowError::ReferenceLimit)
        );
        bad.finish_verified_eof();
        assert!(!bad.is_ready());
    }

    #[test]
    fn unresolved_inventory_covers_labels_used_after_real_reference_resolution() {
        use crate::markdown_viewport_presentation::parser::{
            MarkdownPresentationReferenceContext,
            build_markdown_presentation_with_reference_context,
        };

        let definitions = "[a]: /a\n[b]: /b\n[c]: /c\n[Maße]: /first\n[MASSE]: /later\n[图]: /image\n[a b]: /space\n";
        let references = Parser::new_ext(definitions, markdown_presentation_options(false));
        let alphabet = [
            'a', 'b', 'c', ' ', '[', ']', '(', ')', '!', '\\', '*', '_', '^', '中', '图',
        ];
        let mut random = 0x6ea41d9cb325807f_u64;
        for case in 0..4_096 {
            // 同一个有限paragraph含嵌套/失败link形状；所有全源定义只在纯oracle文本中出现。
            let mut projection = String::new();
            for _ in 0..case % 113 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                projection.push(alphabet[random as usize % alphabet.len()]);
            }
            projection.push_str(" [x][Maße] ![图] [a b]");
            let mut queries =
                MarkdownTableReferenceQueries::from_projection(&projection, false).unwrap();
            for label in ["a", "b", "c", "Maße", "MASSE", "图", "a b"] {
                // 仅测试以原RefDefs提供值；产品由有界global事实按源次序提供相同值。
                let value = references.reference_definitions().get(label).unwrap();
                queries
                    .admit_verified_value(label, &value.dest, value.title.as_deref().unwrap_or(""))
                    .unwrap();
            }
            queries.finish_verified_eof();
            let selection = usize::MAX..usize::MAX;
            let original = build_markdown_presentation_with_reference_context(
                &projection,
                0,
                &selection,
                false,
                Some(MarkdownPresentationReferenceContext::Definitions(
                    definitions,
                )),
                false,
            )
            .unwrap();
            let actual = build_markdown_presentation_with_reference_context(
                &projection,
                0,
                &selection,
                false,
                Some(MarkdownPresentationReferenceContext::Resolved(&queries)),
                false,
            )
            .unwrap();
            assert_eq!(actual.runs, original.runs, "case={case}");
        }
    }

    #[test]
    fn unicode_label_spelling_can_exceed_key_budget_while_its_real_folded_key_fits() {
        // 300个Kelvin符号消耗900个原parser计数字节，fold后仅300个k，仍低于1000门禁。
        // 此等价拼写比16KiB key长，但真实ASCII定义和唯一folded key均在既定预算内。
        let ascii = "k".repeat(16_300);
        let spelling = format!("{}{}", "K".repeat(300), "k".repeat(16_000));
        assert!(spelling.len() > TABLE_WINDOW_MAX_REFERENCE_BYTES);
        let projection = format!("[x][{spelling}]");
        let definition = format!("[{ascii}]: /x");
        assert!(definition.len() <= TABLE_WINDOW_MAX_REFERENCE_BYTES);
        let mut queries =
            MarkdownTableReferenceQueries::from_projection(&projection, false).unwrap();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries.key_bytes, ascii.len());
        assert!(
            queries
                .admit_verified_definition(&ascii, &definition)
                .unwrap()
        );
        assert_eq!(queries.resolve(&spelling), Some(("/x", "")));
    }

    #[test]
    fn unrelated_unicode_expansion_does_not_spend_value_budget_or_poison_queries() {
        let wanted = "k".repeat(16_000);
        let unrelated = format!("{wanted}{}", "ΐ".repeat(300));
        let mut queries =
            MarkdownTableReferenceQueries::from_projection(&format!("[x][{wanted}]"), false)
                .unwrap();
        assert!(queries.may_match_ascii_count(wanted.len()));
        assert!(UniCase::new(&unrelated).to_folded_case().len() > TABLE_WINDOW_MAX_REFERENCE_BYTES);
        // 粗筛并不证明命中；原标签600个非ASCII计数字节合法，但folded key与wanted不同。
        assert!(!queries.needs_definition(&unrelated).unwrap());
        assert!(
            !queries
                .admit_verified_value(
                    &unrelated,
                    &"x".repeat(TABLE_WINDOW_MAX_REFERENCE_BYTES + 1),
                    ""
                )
                .unwrap()
        );
        assert_eq!(queries.value_bytes, 0);
        assert!(
            queries
                .admit_verified_definition(&wanted, &format!("[{wanted}]: /x"))
                .unwrap()
        );
        assert_eq!(queries.resolve(&wanted), Some(("/x", "")));
    }
}
