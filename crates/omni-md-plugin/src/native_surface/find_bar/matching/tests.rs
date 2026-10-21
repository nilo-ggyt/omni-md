//! 输入和模板纯策略回归；不启动 NSApplication 或原生窗口。
use super::*;

#[test]
fn actual_codemirror_regex_reference_cases() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("regex_reference.json")).unwrap();
    let deviations: serde_json::Value =
        serde_json::from_str(include_str!("regex_contract_deviations.json")).unwrap();
    let rows = reference["rows"].as_array().unwrap();
    let reviewed = deviations["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 1_800);
    assert_eq!(reviewed.len(), 51);
    let mut seen = std::collections::HashSet::new();
    for (index, row) in rows.iter().enumerate() {
        // 每一 oracle row 都实际运行，具名差异使用逐例输入/结果断言，绝不按 query 类别跳过。
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Regex.md");
        let source = row["source"].as_str().unwrap();
        std::fs::write(&path, source).unwrap();
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(&path, 65_536).unwrap(),
        ));
        let query = Query {
            search: row["query"].as_str().unwrap().into(),
            replacement: String::new(),
            case_sensitive: row["case_sensitive"].as_bool().unwrap(),
            regex: true,
            word: row["whole_word"].as_bool().unwrap(),
            compiled: std::cell::OnceCell::new(),
        };
        let mut actual = Vec::new();
        let result = query.visit(
            &document,
            &NSString::from_str(source),
            false,
            CaptureRanges::Omit,
            |found| {
                actual.push(serde_json::json!({"start":found.range.start,"end":found.range.end}));
                Ok(true)
            },
        );
        let actual = serde_json::Value::Array(actual);
        let expected = reviewed.iter().find(|entry| entry["row_index"] == index);
        if let Some(expected) = expected {
            // 输入、flags、baseline 坐标也被锁定；任何 oracle 更新都要求重新解释此差异。
            assert_eq!(
                &expected["reference"], row,
                "reviewed input drift at row {index}"
            );
            assert!(seen.insert(index), "duplicate reviewed row {index}");
            assert_eq!(
                actual, expected["native_matches"],
                "native result at row {index}"
            );
            assert_eq!(
                serde_json::to_value(result.err()).unwrap(),
                expected["native_error"],
                "native failure at row {index}"
            );
        } else {
            // 未列出的 1,749 rows 必须与真实保留编辑器完全一致，错误同样判失败。
            assert!(
                result.is_ok(),
                "unexpected native error at row {index}: {result:?}"
            );
            assert_eq!(
                actual, row["matches"],
                "unreviewed regex difference at row {index}"
            );
        }
        assert_eq!(
            document.borrow().generation(),
            0,
            "search mutated row {index}"
        );
    }
    assert_eq!(seen.len(), reviewed.len(), "unexecuted deviation entry");
}

fn synthetic_query(source: &str, pattern: &str, budget: Duration) -> Result<Vec<Found>, String> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Regex.md");
    std::fs::write(&path, source).unwrap();
    let document = Rc::new(RefCell::new(
        MarkdownDocumentBuffer::open(&path, 4 * 1_024 * 1_024).unwrap(),
    ));
    // 仅测试合成输入允许建立独立 NSString；产品仍传递 piece-backed 虚拟 NSString。
    let string = NSString::from_str(source);
    let query = Query {
        search: pattern.into(),
        replacement: String::new(),
        case_sensitive: false,
        regex: true,
        word: false,
        compiled: std::cell::OnceCell::new(),
    };
    let mut matches = Vec::new();
    query.visit_with_budget(
        &document,
        &string,
        false,
        CaptureRanges::Include,
        budget,
        |found| {
            matches.push(found);
            Ok(true)
        },
    )?;
    assert_eq!(document.borrow().generation(), 0);
    Ok(matches)
}

#[test]
fn public_regex_progress_interrupts_a_no_match_backtracking_search() {
    // 必须依靠 progress nil callback 终止无匹配的回溯；不能只在有 result 时检查期限。
    let source = format!("{}!", "a".repeat(32_768));
    let started = Instant::now();
    let result = synthetic_query(&source, "(a+)+$", Duration::from_millis(20));
    assert!(result.is_err_and(|error| error.contains("执行时限")));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "Foundation progress did not provide a bounded interruption"
    );
}

#[test]
fn regex_batches_and_scalar_empty_matches_make_forward_progress() {
    // 超过单批 1,024 结果不会截断，最终零长匹配也不会在同一位置无限重开。
    let source = "a ".repeat(1_100);
    assert_eq!(
        synthetic_query(&source, "a", Duration::from_secs(2))
            .unwrap()
            .len(),
        1_100
    );
    let matches = synthetic_query("😀x", "(?=.)", Duration::from_secs(2)).unwrap();
    assert_eq!(
        matches
            .iter()
            .map(|found| found.range.clone())
            .collect::<Vec<_>>(),
        [0..0, 2..2]
    );
}

#[test]
fn regex_character_classes_and_capture_coordinates_keep_js_basics() {
    let matches = synthetic_query(
        "中文 ABC_12 １２\u{85}\u{feff}",
        r"\w+",
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(
        matches
            .iter()
            .map(|found| found.range.clone())
            .collect::<Vec<_>>(),
        std::iter::once(3..9).collect::<Vec<_>>()
    );
    let digits = synthetic_query("12 １２", r"\d+", Duration::from_secs(2)).unwrap();
    assert_eq!(
        digits
            .iter()
            .map(|found| found.range.clone())
            .collect::<Vec<_>>(),
        std::iter::once(0..2).collect::<Vec<_>>()
    );
    let captures = synthetic_query("😀42", r"(\u{1f600})(\d+)", Duration::from_secs(2)).unwrap();
    assert_eq!(captures[0].captures, [Some(0..4), Some(0..2), Some(2..4)]);
}

#[test]
fn escapes_and_capture_templates_keep_original_rules() {
    assert_eq!(unquote(r"a\nb\rc\td\\e\q"), "a\nb\rc\td\\e\\q");
    let found = Found {
        range: 2..6,
        precise: true,
        captures: vec![Some(2..6), Some(2..3), None],
    };
    let template = ReplacementTemplate::parse("$$:$&:$12:$2:$0", true, found.captures.len());
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool.push(&found).unwrap();
    let mut records = spool.into_replacements().unwrap();
    let parts = records
        .next()
        .unwrap()
        .unwrap()
        .parts
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(matches!(&parts[0], MarkdownReplacementPart::Literal(text) if text == "$:"));
    assert!(matches!(&parts[1], MarkdownReplacementPart::SourceRange(range) if *range == (2..6)));
    assert!(parts.iter().any(
        |part| matches!(part, MarkdownReplacementPart::Literal(text) if text.contains("undefined"))
    ));
}

#[test]
fn sole_nonempty_match_wraps_for_both_navigation_directions() {
    // 真实 CodeMirror 6 oracle：唯一英文、中文/emoji 与 regex 非空匹配都允许再次选中自己。
    // ReplaceNext 仍直接消费独立候选字段，不调用此普通导航策略。
    for range in [4..11, 1..5, 29..36] {
        // 分别重建方向状态，确保同一轮五个候选足以完成回绕，不重扫正文。
        for backwards in [false, true] {
            let mut matches = NavigationMatches::default();
            matches
                .observe(
                    Found {
                        range: range.clone(),
                        precise: true,
                        captures: Vec::new(),
                    },
                    &range,
                )
                .unwrap();
            assert_eq!(
                matches.navigation_target(backwards).unwrap().found.range,
                range
            );
        }
    }
}

#[test]
fn navigation_excludes_current_and_wraps_with_one_scan() {
    // 包含前/当前/后候选时，双向回绕只需要有界标量；当前空范围也不能成为自身下一处。
    let found = |range| Found {
        range,
        precise: true,
        captures: Vec::new(),
    };
    let mut matches = NavigationMatches::default();
    for range in [0..2, 3..5, 6..8] {
        matches.observe(found(range), &(3..5)).unwrap();
    }
    assert_eq!(matches.first.unwrap().found.range, 0..2);
    assert_eq!(matches.last.unwrap().found.range, 6..8);
    assert_eq!(matches.previous.unwrap().found.range, 0..2);
    assert_eq!(matches.next.unwrap().found.range, 6..8);
    assert_eq!(matches.current.unwrap().found.range, 3..5);
    let mut only = NavigationMatches::default();
    only.observe(found(2..2), &(2..2)).unwrap();
    assert!(
        only.first.is_none()
            && only.last.is_none()
            && only.next.is_none()
            && only.previous.is_none()
    );
    assert_eq!(only.current.unwrap().found.range, 2..2);
}
