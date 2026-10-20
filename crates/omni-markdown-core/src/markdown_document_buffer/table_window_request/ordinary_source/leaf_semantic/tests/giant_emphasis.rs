//! 真 Source 请求验证巨大强调：原完整 Parser 只作测试 oracle，产品沿 mmap 小帧与匿名匹配栈。

use super::inline::finish_inline;
use super::*;

#[test]
/// 远处closer、嵌套强调和code真正进入原请求；前移同Global且编辑/Undo不能复活旧证明。
fn giant_root_emphasis_code_and_forward_publication_match_original_parser_without_copy() {
    let source = format!(
        "prefix ***{} `code *literal* 😀` ~~删除~~ ~下标~ ^上标^ _斜体_*** tail\n",
        "中😀 ".repeat(3000)
    );
    let (_directory, mut doc) = document(&source);
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let first_range = 5000..5041;
    let mut request = doc
        .viewport_leaf_source_request(first_range.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let pointer = request.batch.source_probe_state().0;
    let first = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &first.semantic else {
        panic!("真正giant强调不得仍然Pending")
    };
    assert_oracle(ready, &doc, &source, &first_range);
    let styles = ready
        .query_for(&doc, authority(&doc), first_range.start)
        .unwrap()
        .unwrap()
        .styles;
    assert!(styles.contains(MarkdownPresentationStyles::STRONG));
    assert!(styles.contains(MarkdownPresentationStyles::EMPHASIS));
    let at = source.find("`code").unwrap();
    let next = units(&source, at..source.len());
    request
        .continue_forward(&doc, &first, next.clone(), authority(&doc))
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let second = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(current) = &second.semantic else {
        panic!("同块前移须实际保留嵌套/code/扩展强调")
    };
    assert_oracle(current, &doc, &source, &next);
    assert_oracle(ready, &doc, &source, &first_range);
    assert_eq!(request.batch.source_probe_state().0, pointer);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    doc.insert_utf16(0, "改", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert_eq!(
        ready
            .query_for(&doc, authority(&doc), first_range.start)
            .err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    assert_eq!(
        current.query_for(&doc, authority(&doc), next.start).err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
}

#[test]
/// 同源Unicode邻接、mod3、失败opener与HTML/code优先序均与原Parser逐位置一致。
fn giant_root_emphasis_flanking_and_priority_match_complete_parser() {
    for (before, after) in [
        ("a_", "_b"),
        ("a*", "*b"),
        ("***", "***"),
        ("**a _", "_ z**"),
        ("*a _", "* z_"),
        ("中文：_", "_。"),
        (r"\*escaped* **", "**"),
        (r"\\*escaped* **", "**"),
        ("**<span title=\"*fake*\">", "</span> `*literal*`**"),
        ("~~", "~~"),
        ("H~", "~O"),
        ("^", "^"),
    ] {
        let source = format!("prefix {before}{}{after} tail\r\n", "中😀 ".repeat(500));
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
            .unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
            .unwrap();
        finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
            panic!("已接giant强调切片必须有真实Ready: {before:?} {after:?}")
        };
        assert_oracle(ready, &doc, &source, &requested);
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(request.total_work().allocated_source_capacity, 0);
    }
}

#[test]
/// 多页prev链有真正磁盘历史，巨量离屏嵌套不能变成内存中的range或opener目录。
fn giant_root_emphasis_uses_paged_history_and_finite_visible_output() {
    let source = format!(
        "{}{}{}\n",
        "*a ".repeat(80),
        "中".repeat(4000),
        " a*".repeat(80)
    );
    let (_directory, doc) = document(&source);
    let requested = 1000..1040;
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("实际匿名prev链必须参与完整生产匹配")
    };
    assert_oracle(ready, &doc, &source, &requested);
    assert!(ready.run_count() <= requested.len());
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
/// 选区是真UTF16输入，owner是真byte范围；端点在可见区外仍能激活整条巨大结构的marker。
fn giant_emphasis_owner_selection_is_not_clipped_to_visible_coordinates() {
    // 非空白尾字符让真正右delimiter成立，不能以原Parser也视作字面的样本伪证marker活动。
    let source = format!("prefix **{}尾** tail\n", "中😀 ".repeat(1500));
    let (_directory, mut doc) = document(&source);
    doc.set_selection(MarkdownTextSelection::caret(3000))
        .unwrap();
    let requested = 7..13;
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("真实owner选择不得被投影视口截断")
    };
    assert_oracle(ready, &doc, &source, &requested);
    let opening = ready
        .query_for(&doc, authority(&doc), 7)
        .unwrap()
        .unwrap()
        .styles;
    assert!(opening.contains(MarkdownPresentationStyles::STRONG));
    assert!(opening.contains(MarkdownPresentationStyles::MARKER));
    assert!(!opening.contains(MarkdownPresentationStyles::HIDDEN_MARKER));
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
}
