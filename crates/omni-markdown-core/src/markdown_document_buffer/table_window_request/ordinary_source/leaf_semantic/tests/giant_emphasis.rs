//! Giant emphasis through real Source requests: the full parser supplies a test oracle while the
//! product consumes small mmap frames and its anonymous matching stack.

use super::inline::finish_inline;
use super::*;

#[test]
/// Resolve distant closers/nested emphasis/code on the original request and Global; editing/undo
/// must not revive proofs retained from earlier forward windows.
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
/// Compare same-source Unicode flanking, mod3, failed openers, and HTML/code priority position by position.
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
/// Exercise real multipage previous-record history; offscreen nesting must not become a RAM
/// directory of ranges/openers.
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
/// Real UTF-16 selection endpoints outside the viewport still activate markers owned by the
/// complete byte-range construct.
fn giant_emphasis_owner_selection_is_not_clipped_to_visible_coordinates() {
    // A nonwhitespace tail makes the right delimiter real; a parser-literal fixture would not
    // establish marker activity.
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
