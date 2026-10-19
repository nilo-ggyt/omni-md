//! 超预算唯一物理行的原扫描 oracle、预算边界及语义回归。

use super::*;
use crate::markdown_document_buffer::{
    MAX_MATERIALIZED_UTF8_BYTES, MAX_VIEWPORT_OVERSCAN_UTF16_UNITS, MarkdownBlockScannerState,
    scan_markdown_block_slice,
};
use std::ops::Range;

/// 使用真实旧扫描原语证明：未到 EOF 的唯一物理行不能消费字符或追加 checkpoint。
fn assert_truncated_scan_has_no_publication(buffer: &MarkdownDocumentBuffer) {
    assert_eq!(buffer.line_count(), 1);
    let byte_end = buffer
        .scalar_aligned_byte_offset_down(MAX_MATERIALIZED_UTF8_BYTES.min(buffer.byte_len()))
        .expect("旧字节上限按真实 scalar 边界回退");
    let scan_end = MAX_MATERIALIZED_UTF16_UNITS
        .min(
            buffer
                .utf16_offset_for_byte(byte_end)
                .expect("真实 UTF-16 边界"),
        )
        .min(buffer.utf16_len());
    assert!(scan_end < buffer.utf16_len());
    let slice = buffer
        .materialize_viewport(0..scan_end, 0)
        .expect("旧扫描前缀保持合法 scalar 边界");
    let initial = MarkdownBlockScannerState::document_start();
    let scan = scan_markdown_block_slice(&slice.text, 0, 0, initial, false, true);
    assert_eq!(scan.scanned_utf16_end, 0);
    assert_eq!(scan.final_state, initial);
    assert!(scan.completed_blocks.is_empty());
    assert!(scan.checkpoints.is_empty());
}

/// 以同一有限 source API 比较字节、坐标和状态，同时检查原稀疏索引没有隐藏变化。
fn assert_same_source_fallback(
    buffer: &MarkdownDocumentBuffer,
    range: Range<usize>,
    overscan: usize,
) {
    let checkpoints = buffer.block_checkpoints.borrow().checkpoints.clone();
    let expected = buffer
        .materialize_viewport(range.clone(), overscan)
        .expect("原有限 source 请求");
    let actual = buffer
        .materialize_markdown_viewport(range, overscan)
        .expect("同一 source fallback");
    assert_eq!(
        actual.semantic_status,
        MarkdownViewportSemanticStatus::SourceFallback
    );
    assert_eq!(actual.utf16_range, expected.utf16_range);
    assert_eq!(actual.text, expected.text);
    assert_eq!(buffer.block_checkpoints.borrow().checkpoints, checkpoints);
}

#[test]
fn oversized_single_line_preserves_source_windows_and_checkpoint_state() {
    // 即使开头含 Markdown 样式，截断物理行也不能提前把局部文本当成完整 block。
    let content = format!(
        "**开头** {} [结尾](https://example.test)",
        "x".repeat(MAX_MATERIALIZED_UTF16_UNITS + 512)
    );
    let fixture = MarkdownBufferFixture::new(&content);
    assert_truncated_scan_has_no_publication(&fixture.buffer);
    assert_eq!(
        fixture.buffer.block_checkpoints.borrow().checkpoints.len(),
        1
    );
    for range in [
        0..6,
        8_000..8_010,
        fixture.buffer.utf16_len() - 6..fixture.buffer.utf16_len(),
    ] {
        // 首屏、中部和末尾都消费原 source 路径；overscan 不扩大为整行。
        assert_same_source_fallback(&fixture.buffer, range, 8);
    }
}

#[test]
fn exact_utf16_and_utf8_limits_keep_complete_semantics() {
    let byte_limit_text =
        "中".repeat(MAX_MATERIALIZED_UTF8_BYTES / 3) + &"a".repeat(MAX_MATERIALIZED_UTF8_BYTES % 3);
    for content in ["a".repeat(MAX_MATERIALIZED_UTF16_UNITS), byte_limit_text] {
        // 等于上限仍能抵达真实 EOF；不能把 >= 误写成超限并丢失完整语义。
        let fixture = MarkdownBufferFixture::new(&content);
        let viewport = fixture
            .buffer
            .materialize_markdown_viewport(0..1, 0)
            .expect("上限等值仍可完成扫描");
        assert_eq!(
            viewport.semantic_status,
            MarkdownViewportSemanticStatus::Complete
        );
        assert_eq!(viewport.utf16_range, 0..fixture.buffer.utf16_len());
        assert_eq!(viewport.text, content);
    }
}

#[test]
fn utf8_only_overflow_preserves_unicode_source_fallback() {
    // CJK 的字节预算先耗尽，UTF-16 长度仍低于另一个上限；该分支必须同样省掉旧扫描副本。
    let content = "中".repeat(MAX_MATERIALIZED_UTF8_BYTES / 3)
        + &"a".repeat(MAX_MATERIALIZED_UTF8_BYTES % 3 + 1);
    let fixture = MarkdownBufferFixture::new(&content);
    assert!(fixture.buffer.byte_len() > MAX_MATERIALIZED_UTF8_BYTES);
    assert!(fixture.buffer.utf16_len() < MAX_MATERIALIZED_UTF16_UNITS);
    assert_truncated_scan_has_no_publication(&fixture.buffer);
    assert_same_source_fallback(&fixture.buffer, 7..9, 1);
}

#[test]
fn unicode_separators_and_emoji_preserve_the_crlf_scanner_contract() {
    // U+2028/U+2029 不是现有 block scanner 的 CR/LF 边界；不能误用 AppKit 段落规则。
    let content = "甲\u{2028}乙\u{2029}😀".repeat(25_000);
    let fixture = MarkdownBufferFixture::new(&content);
    assert_truncated_scan_has_no_publication(&fixture.buffer);
    assert_same_source_fallback(&fixture.buffer, 4..6, 1);
    assert_same_source_fallback(&fixture.buffer, 6..7, MAX_VIEWPORT_OVERSCAN_UTF16_UNITS);
}

#[test]
fn truncated_surrogate_limit_preserves_the_original_error_order() {
    // 固定 UTF-16 上限落在完整 emoji 的两个 unit 之间；旧扫描切片会先拒绝这个边界。
    let content = "a".repeat(MAX_MATERIALIZED_UTF16_UNITS - 1) + "😀tail";
    let fixture = MarkdownBufferFixture::new(&content);
    assert_eq!(
        fixture
            .buffer
            .materialize_viewport(0..MAX_MATERIALIZED_UTF16_UNITS, 0),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    assert_eq!(
        fixture.buffer.materialize_markdown_viewport(0..1, 0),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    // 当请求先超过 scan_end 时，原来的更早 fallback 分支仍返回同一完整 emoji，不能交换错误顺序。
    assert_same_source_fallback(
        &fixture.buffer,
        MAX_MATERIALIZED_UTF16_UNITS - 1..MAX_MATERIALIZED_UTF16_UNITS + 1,
        0,
    );
}

#[test]
fn empty_origin_and_invalid_requests_keep_existing_results() {
    let fixture = MarkdownBufferFixture::new(&"a".repeat(MAX_MATERIALIZED_UTF16_UNITS + 1));
    let empty = fixture
        .buffer
        .materialize_markdown_viewport(0..0, 0)
        .expect("原起点空查询");
    assert_eq!(
        empty.semantic_status,
        MarkdownViewportSemanticStatus::Complete
    );
    assert_eq!(empty.utf16_range, 0..0);
    assert!(empty.text.is_empty());
    assert_same_source_fallback(&fixture.buffer, 0..0, 1);
    let end = fixture.buffer.utf16_len();
    assert_same_source_fallback(&fixture.buffer, end..end, 0);
    // 参数错误仍在所有扫描和新早退之前返回；不能用 source 路径替换原错误类型。
    assert_eq!(
        fixture
            .buffer
            .materialize_markdown_viewport(end..end + 1, 0),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    assert_eq!(
        fixture
            .buffer
            .materialize_markdown_viewport(0..1, MAX_VIEWPORT_OVERSCAN_UTF16_UNITS + 1),
        Err(MarkdownDocumentBufferError::MaterializationLimitExceeded)
    );
}

#[test]
fn short_single_line_markdown_keeps_original_styles() {
    for (content, target, style) in [
        ("# 标题", "标题", MarkdownPresentationStyles::HEADING_1),
        ("**粗体**", "粗体", MarkdownPresentationStyles::STRONG),
        (
            "[链接](https://example.test)",
            "链接",
            MarkdownPresentationStyles::LINK,
        ),
        ("`代码`", "代码", MarkdownPresentationStyles::INLINE_CODE),
        ("> 引用", "引用", MarkdownPresentationStyles::BLOCK_QUOTE),
        ("- 条目", "条目", MarkdownPresentationStyles::UNORDERED_LIST),
    ] {
        // 唯一行本身不是 fallback 条件；保留完整 envelope 及后续原解析器的实际样式。
        let fixture = MarkdownBufferFixture::new(content);
        let index = utf16_offset_of(content, target);
        let viewport = fixture
            .buffer
            .materialize_markdown_viewport(index..index + 1, 0)
            .expect("短 Markdown 的完整语义");
        assert_eq!(
            viewport.semantic_status,
            MarkdownViewportSemanticStatus::Complete
        );
        assert_eq!(viewport.text, content);
        let mut presentation = MarkdownViewportPresentation::default();
        presentation
            .rebuild(
                fixture.buffer.generation(),
                viewport.utf16_range,
                &viewport.text,
            )
            .expect("原样式解析器");
        assert!(
            presentation
                .query(index)
                .expect("真实源样式")
                .styles
                .contains(style)
        );
    }
}

#[test]
fn adding_and_undoing_a_line_break_rechecks_the_live_piece_tree() {
    let content = "## 标题 ".to_owned() + &"x".repeat(MAX_MATERIALIZED_UTF16_UNITS + 512);
    let mut fixture = MarkdownBufferFixture::new(&content);
    assert_same_source_fallback(&fixture.buffer, 3..4, 0);
    fixture
        .buffer
        .insert_utf16(5, "\n\n", MarkdownTextSelection::caret(7))
        .expect("在标题后插入真实分隔行");
    for after_undo in [false, true] {
        // 多行文档可在巨行之前完成短标题；不能因为总长度超限就全局禁用 Markdown 语义。
        assert_eq!(fixture.buffer.line_count(), 3);
        let viewport = fixture
            .buffer
            .materialize_markdown_viewport(3..4, 0)
            .expect("恢复短标题 envelope");
        assert_eq!(
            viewport.semantic_status,
            MarkdownViewportSemanticStatus::Complete
        );
        assert_eq!(viewport.text, "## 标题\n");
        if after_undo {
            break; // Redo 的同一真实结果已核完，不再发起额外编辑。
        }
        fixture.buffer.undo().expect("撤销分隔行");
        assert_eq!(fixture.buffer.line_count(), 1);
        assert_same_source_fallback(&fixture.buffer, 3..4, 0);
        fixture.buffer.redo().expect("重做分隔行");
    }
}
