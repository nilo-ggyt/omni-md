//! 字面搜索的兼容位置、Unicode 精确度与固定源窗口回归。

use super::*;

#[test]
fn actual_codemirror_literal_reference_cases() {
    // 独立 oracle 来自保存的真实 SearchQuery/getCursor，不调用本实现生成期望。
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("reference.json")).unwrap();
    let mut failures = Vec::new();
    for (index, row) in reference["rows"].as_array().unwrap().iter().enumerate() {
        let actual = collect(
            row["source"].as_str().unwrap(),
            row["query"].as_str().unwrap(),
            MarkdownLiteralFindOptions {
                case_sensitive: row["case_sensitive"].as_bool().unwrap(),
                whole_word: row["whole_word"].as_bool().unwrap(),
                overlapping: row["overlapping"].as_bool().unwrap(),
            },
        );
        let actual = actual.iter().map(|found| serde_json::json!({"range_utf16":{"start":found.range_utf16.start,"end":found.range_utf16.end},"precise":found.precise})).collect::<Vec<_>>();
        if serde_json::Value::Array(actual) != row["matches"] {
            // 失败有界报告合成 fixture 编号和查询；不涉及真实 vault 数据。
            failures.push(index);
        }
    }
    assert!(
        failures.is_empty(),
        "{} literal reference differences; first {:?}",
        failures.len(),
        &failures[..failures.len().min(20)]
    );
}

fn collect(text: &str, query: &str, options: MarkdownLiteralFindOptions) -> Vec<MarkdownFindMatch> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Find.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 4 * 1_024 * 1_024).unwrap();
    let mut matches = Vec::new();
    visit_markdown_literal_matches(
        &document,
        query,
        0..document.utf16_len(),
        options,
        |found| {
            matches.push(found);
            Ok(true)
        },
    )
    .unwrap();
    matches
}

#[test]
fn unicode_normalization_preserves_source_ranges_and_precision() {
    // expansion 内部命中可以显示原 scalar，但不能把未匹配的字符尾部当成已匹配内容替换。
    let found = collect(
        "é e\u{301} ﬀ 😀 中文",
        "e",
        MarkdownLiteralFindOptions::default(),
    );
    assert_eq!(
        found,
        vec![
            MarkdownFindMatch {
                range_utf16: 0..1,
                precise: false
            },
            MarkdownFindMatch {
                range_utf16: 2..3,
                precise: true
            },
        ]
    );
    assert_eq!(
        collect("ﬀ", "ff", MarkdownLiteralFindOptions::default()),
        vec![MarkdownFindMatch {
            range_utf16: 0..1,
            precise: true
        }]
    );
    assert_eq!(
        collect("😀 😀", "😀", MarkdownLiteralFindOptions::default())
            .iter()
            .map(|value| value.range_utf16.clone())
            .collect::<Vec<_>>(),
        [0..2, 3..5]
    );
}

#[test]
fn dense_overlapping_and_word_queries_have_distinct_admission() {
    // 导航允许重叠，全部替换使用非重叠结果；二者不靠事后删除匹配来改变范围。
    let options = MarkdownLiteralFindOptions {
        overlapping: true,
        ..Default::default()
    };
    assert_eq!(collect("aaaa", "aa", options).len(), 3);
    assert_eq!(
        collect("aaaa", "aa", MarkdownLiteralFindOptions::default()).len(),
        2
    );
    let options = MarkdownLiteralFindOptions {
        whole_word: true,
        ..Default::default()
    };
    assert_eq!(
        collect("cat scatter cat_ cat 中文", "cat", options)
            .iter()
            .map(|value| value.range_utf16.clone())
            .collect::<Vec<_>>(),
        [0..3, 17..20]
    );
}

#[test]
fn giant_source_crosses_windows_without_flattening_or_losing_match() {
    // emoji 横跨读取窗口后仍能继续搜索；稠密 ASCII 窗口不逐 scalar 分配缓存。
    let text = format!("{}😀Needle{}", "a".repeat(4_095), "b".repeat(1_024 * 1_024));
    let found = collect(&text, "😀needle", MarkdownLiteralFindOptions::default());
    assert_eq!(
        found,
        vec![MarkdownFindMatch {
            range_utf16: 4_095..4_103,
            precise: true
        }]
    );
}
