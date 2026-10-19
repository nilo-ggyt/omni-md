//! 恒等scalar快速路径对原算法全Unicode差分；实际跨窗与晚Source故障仍走完整查找入口。

use super::*;
use std::{cell::Cell, io::Write};

fn original_units(scalar: char, case_sensitive: bool) -> Vec<u16> {
    let normalized = scalar.to_string().nfkd().collect::<String>();
    let normalized = if case_sensitive {
        normalized
    } else {
        normalized.to_lowercase()
    };
    normalized.encode_utf16().collect() // 独立保留改前算法，不能调用新identity判定生成期望。
}

#[test]
fn every_unicode_scalar_matches_the_original_nfkd_and_string_lowercase_algorithm() {
    let mut checked = 0usize;
    for value in 0..=u32::from(char::MAX) {
        let Some(scalar) = char::from_u32(value) else {
            continue; // surrogate不是Rust scalar，不将非法编码混进算法差分。
        };
        let mut original = [0; 2];
        let original = scalar.encode_utf16(&mut original);
        for case_sensitive in [false, true] {
            assert_eq!(
                scalar_units(scalar, case_sensitive, original).as_ref(),
                original_units(scalar, case_sensitive),
                "U+{value:04X}, case_sensitive={case_sensitive}"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 0x11_0000 - 0x800); // 实际遍历全部合法scalar，不用有限常见字符代替全域对照。
}

#[test]
fn unchanged_cjk_and_non_bmp_scalars_borrow_the_original_stack_units() {
    for scalar in ['中', '目', '标', '😀', '\u{20000}'] {
        let mut buffer = [0; 2];
        let original = scalar.encode_utf16(&mut buffer);
        for case_sensitive in [false, true] {
            let actual = scalar_units(scalar, case_sensitive, original);
            assert!(matches!(actual, Cow::Borrowed(_)));
            assert_eq!(actual.len(), scalar.len_utf16());
            assert!(std::ptr::eq(actual.as_ptr(), original.as_ptr()));
        }
    }
    for scalar in ['é', 'ﬀ', 'İ', 'Σ', '\u{10400}'] {
        let mut buffer = [0; 2];
        let original = scalar.encode_utf16(&mut buffer);
        assert!(matches!(
            scalar_units(scalar, false, original),
            Cow::Owned(_)
        ));
    } // 兼容展开、组合音标、多字符小写及非BMP变大小写都不能误走恒等借用。
}

fn fixture() -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("FindScalar.md");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all("目标".as_bytes()).unwrap();
    let mut block = [0; 4095];
    for scalar in block.as_chunks_mut::<3>().0 {
        scalar.copy_from_slice("中".as_bytes());
    }
    for _ in 0..128 {
        file.write_all(&block).unwrap();
    }
    drop(file); // 固定块生成超过原64KiB驻留窗的CJK源，不构造测试全文String。
    let document = MarkdownDocumentBuffer::open(&path, 1_024 * 1_024).unwrap();
    (directory, document)
}

#[test]
fn a_first_match_still_scans_to_eof_and_explicit_cancellation_keeps_the_original_checkpoints() {
    let (_directory, document) = fixture();
    let checks = Cell::new(0);
    let mut matches = 0;
    visit_markdown_literal_matches_cancellable(
        &document,
        "目标",
        0..document.utf16_len(),
        MarkdownLiteralFindOptions::default(),
        || {
            checks.set(checks.get() + 1);
            true
        },
        |found| {
            assert_eq!(found.range_utf16, 0..2);
            assert!(found.precise);
            matches += 1;
            Ok(true)
        },
    )
    .unwrap();
    assert_eq!(matches, 1);
    assert_eq!(checks.get(), document.utf16_len().div_ceil(4096));

    let checks = Cell::new(0);
    let result = visit_markdown_literal_matches_cancellable(
        &document,
        "不存在",
        0..document.utf16_len(),
        MarkdownLiteralFindOptions::default(),
        || {
            checks.set(checks.get() + 1);
            checks.get() < 3
        },
        |_| panic!("固定源不含该查询"),
    );
    assert_eq!(result, Err(MarkdownDocumentBufferError::FindInterrupted));
    assert_eq!(checks.get(), 3);
}

#[test]
fn source_reclaim_failure_after_the_first_match_still_rejects_the_whole_scan_and_future_cancel() {
    let (_directory, document) = fixture();
    let observer = document.original_backing_observer().unwrap();
    let mut matches = 0;
    let result = visit_markdown_literal_matches(
        &document,
        "目标",
        0..document.utf16_len(),
        MarkdownLiteralFindOptions::default(),
        |found| {
            assert_eq!(found.range_utf16, 0..2);
            matches += 1;
            document.fail_next_original_reclaim_for_test();
            Ok(true)
        },
    );
    assert_eq!(matches, 1);
    assert_eq!(
        result,
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(observer.first_errno(), Some(libc::EIO));
    assert!(observer.reclaim_attempts() > 0 && observer.reclaim_successes() > 0);
    let result = visit_markdown_literal_matches_cancellable(
        &document,
        "目标",
        0..document.utf16_len(),
        MarkdownLiteralFindOptions::default(),
        || false,
        |_| panic!("原Source已撤权，不得发布旧匹配"),
    );
    assert_eq!(
        result,
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    drop(document);
    assert!(observer.unmap_succeeded() && observer.mapped_bytes() == 0);
} // EIO来自原真实msync成功后的既有注入点，不把它称为内核自身返回的EIO。

#[test]
fn mixed_identity_and_expansion_matches_keep_exact_ranges_across_the_surrogate_window_edge() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("FindWindow.md");
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..4095 {
        file.write_all("中".as_bytes()).unwrap();
    }
    file.write_all("😀目é标ﬀ𐐀".as_bytes()).unwrap();
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 64 * 1_024).unwrap();
    for (query, expected, precise) in [
        ("😀目e\u{301}标ff", 4095..4101, true),
        ("e", 4098..4099, false),
        ("ff", 4100..4101, true),
        ("𐐨", 4101..4103, true),
    ] {
        let mut found = Vec::new();
        visit_markdown_literal_matches(
            &document,
            query,
            0..document.utf16_len(),
            MarkdownLiteralFindOptions::default(),
            |value| {
                found.push(value);
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(
            found,
            [MarkdownFindMatch {
                range_utf16: expected,
                precise
            }]
        );
    }
    assert_eq!(
        visit_markdown_literal_matches(
            &document,
            "目",
            4096..document.utf16_len(),
            MarkdownLiteralFindOptions::default(),
            |_| panic!("低surrogate范围不得进入匹配回调"),
        ),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
}
