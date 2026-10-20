//! 真实Core接口的边界、分页、失败与资源合同；官方原始case只从显式ROOT路径读取。
use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownTextSelection};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    rc::Rc,
};
use tempfile::TempDir;

const TINY: MarkdownLineBreakBudget = MarkdownLineBreakBudget {
    bytes: 3,
    scalars: 1,
    records: 1,
};

fn fixture(bytes: &[u8]) -> (TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("line-breaks.md");
    std::fs::write(&path, bytes).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}
fn source(bytes: &[u8]) -> MarkdownLayoutTextSource {
    let (_directory, document) = fixture(bytes);
    MarkdownLayoutTextSource::capture(&document)
}
fn finish(job: &mut MarkdownLineBreakJob, authority: &MarkdownLayoutTextAuthority) {
    let length = job.storage_profile().source_bytes;
    for _ in 0..length.checked_mul(4).unwrap().checked_add(16).unwrap() {
        let step = job.step(authority, TINY).unwrap();
        assert!(
            step.bytes <= TINY.bytes
                && step.scalars <= TINY.scalars
                && step.records <= TINY.records
        );
        if step.status == MarkdownLineBreakStatus::Ready {
            assert!(job.is_ready());
            assert_eq!(job.storage_profile().source_bytes_read, length);
            return;
        }
        assert!(
            step.bytes > 0 || step.scalars > 0 || step.records > 0,
            "Pending必须实际推进"
        );
    }
    panic!("有限step没有达到真实EOF");
}
fn released(observer: &MarkdownLineBreakResourceObserver, source_done: bool) {
    let facts = observer.snapshot();
    assert!(!facts.metadata_file_open);
    assert_eq!(facts.metadata_map_bytes, 0);
    assert_eq!(facts.metadata_peak_map_bytes, 0);
    assert_eq!(facts.metadata_buffer_bytes, 0);
    assert!(facts.metadata_peak_buffer_bytes <= MARKDOWN_LINE_BREAK_INDEX_BYTES);
    assert!(!facts.cleanup_failed);
    assert_eq!(facts.source_payload_released, source_done);
}
fn position(value: MarkdownLineBreakBoundary) -> (usize, usize) {
    (value.byte, value.utf16)
}
fn compare(
    job: &mut MarkdownLineBreakJob,
    authority: &MarkdownLayoutTextAuthority,
    expected: &[(usize, usize)],
) -> usize {
    assert_eq!(job.storage_profile().boundary_records, expected.len());
    let profile = job.storage_profile();
    let mut count = 0;
    for unit in [MarkdownLineBreakUnit::Byte, MarkdownLineBreakUnit::Utf16] {
        let coordinate = |value: (usize, usize)| match unit {
            MarkdownLineBreakUnit::Byte => value.0,
            MarkdownLineBreakUnit::Utf16 => value.1,
        };
        let maximum = match unit {
            MarkdownLineBreakUnit::Byte => profile.source_bytes,
            MarkdownLineBreakUnit::Utf16 => profile.source_utf16,
        };
        for offset in 0..=maximum {
            for query in [
                MarkdownLineBreakQuery::IsBoundary,
                MarkdownLineBreakQuery::Previous,
                MarkdownLineBreakQuery::Next,
            ] {
                let wanted = match query {
                    MarkdownLineBreakQuery::IsBoundary => expected
                        .iter()
                        .copied()
                        .find(|value| coordinate(*value) == offset),
                    MarkdownLineBreakQuery::Previous => expected
                        .iter()
                        .copied()
                        .rev()
                        .find(|value| coordinate(*value) < offset),
                    MarkdownLineBreakQuery::Next => expected
                        .iter()
                        .copied()
                        .find(|value| coordinate(*value) > offset),
                };
                match job.query(authority, unit, offset, query).unwrap() {
                    MarkdownLineBreakAnswer::Known(actual) => assert_eq!(
                        actual.map(position),
                        wanted,
                        "unit={unit:?} offset={offset} query={query:?}"
                    ),
                    MarkdownLineBreakAnswer::Pending => panic!("完整EOF后不能仍Pending"),
                }
                count += 1;
            }
        }
    }
    count
}

/// 每个量子只收费实际字节、标量和机会；折叠跳转不能以零工作 Pending 忙转。
#[cfg(feature = "paged-line-compose")]
fn finish_visible(job: &mut MarkdownLineBreakJob, authority: &MarkdownLayoutTextAuthority) {
    for _ in 0..16_384 {
        let step = job.step(authority, TINY).unwrap();
        assert!(
            step.bytes <= TINY.bytes
                && step.scalars <= TINY.scalars
                && step.records <= TINY.records
        );
        if step.status == MarkdownLineBreakStatus::Ready {
            assert!(job.is_ready());
            return;
        }
        assert!(step.bytes != 0 || step.scalars != 0 || step.records != 0);
    }
    panic!("可见断行量子没有收敛到真实 EOF");
}

#[cfg(feature = "paged-line-compose")]
#[test]
/// 同源折叠只撤内部查询；可见首尾机会仍逐条等同原完整 UAX14 结果。
fn folded_hard_breaks_keep_original_rules_and_deny_only_hidden_queries() {
    // CR、LF、CRLF、真实无终止符 EOF 都由同一原 job 处理，含引号、数字和组合标记上下文。
    for (text, hidden) in [
        ("a (\r\n隐😀\r\n后 1.2\n", 3..10),
        ("a\n\n后\n", 1..3),
        ("a\r隐\r后", 1..4),
        ("a\n隐😀", 1..5),
        ("a\r\n", 1..3),
        ("x \"\r\n\u{301}z\n后", 3..8),
    ] {
        let original = source(text.as_bytes());
        let authority = original.authority();
        let plan = FoldPlan::new(&original, std::slice::from_ref(&hidden)).unwrap();
        let span = plan.spans()[0];
        let mut complete = MarkdownLineBreakJob::new(original.clone()).unwrap();
        finish(&mut complete, &authority);
        let mut expected = Vec::new();
        for index in 0..complete.index.len() {
            let value = complete.index.get(index).unwrap();
            if !plan.hides(value.utf16, true) {
                expected.push(value);
            }
        }
        let mut folded = MarkdownLineBreakJob::new(original.clone()).unwrap();
        folded.install_fold_plan(plan.clone()).unwrap();
        let observer = folded.resource_observer();
        finish_visible(&mut folded, &authority);
        assert_eq!(folded.index.len(), expected.len(), "{text:?}");
        for (index, wanted) in expected.into_iter().enumerate() {
            assert_eq!(folded.index.get(index).unwrap(), wanted, "{text:?}");
        }
        for unit in [MarkdownLineBreakUnit::Byte, MarkdownLineBreakUnit::Utf16] {
            let (start, end) = match unit {
                MarkdownLineBreakUnit::Byte => (span.start.byte, span.end.byte),
                MarkdownLineBreakUnit::Utf16 => (span.start.utf16, span.end.utf16),
            };
            for query in [
                MarkdownLineBreakQuery::IsBoundary,
                MarkdownLineBreakQuery::Previous,
                MarkdownLineBreakQuery::Next,
            ] {
                assert!(matches!(
                    folded.query(&authority, unit, start + 1, query),
                    Err(MarkdownLineBreakError::FoldedRange)
                ));
            }
            // 有效终点保留真实 mandatory；拒绝内部查询不会撤销它或原健康资源。
            assert!(matches!(
                folded
                    .query(&authority, unit, end, MarkdownLineBreakQuery::IsBoundary)
                    .unwrap(),
                MarkdownLineBreakAnswer::Known(Some(MarkdownLineBreakBoundary {
                    kind: MarkdownLineBreakKind::Mandatory,
                    ..
                }))
            ));
        }
        folded.close().unwrap();
        released(&observer, false);
        complete.close().unwrap();
        drop(original);
        released(&observer, true);
    }
}

#[cfg(feature = "paged-line-compose")]
#[test]
/// 超长隐区不经过解码器，取消后仍须真实交还匿名索引与原 Source 资源。
fn folded_giant_body_is_not_read_and_original_eof_and_cancel_owners_still_apply() {
    let hidden_body = "隐😀ภาษาไทย".repeat(32_768);
    let text = format!("可见标题\r\n{hidden_body}\r\n后续正文\n");
    let hidden_start = "可见标题".encode_utf16().count();
    let hidden_end = hidden_start + 2 + hidden_body.encode_utf16().count() + 2;
    let original = source(text.as_bytes());
    let authority = original.authority();
    let plan = FoldPlan::new(&original, std::slice::from_ref(&(hidden_start..hidden_end))).unwrap();
    let mut folded = MarkdownLineBreakJob::new(original.clone()).unwrap();
    folded.install_fold_plan(plan.clone()).unwrap();
    let observer = folded.resource_observer();
    finish_visible(&mut folded, &authority);
    let profile = folded.storage_profile();
    assert_eq!(profile.source_bytes_read, text.len() - hidden_body.len());
    assert!(profile.source_bytes_read < 64);
    assert!(profile.source_borrow_max_bytes <= TINY.bytes);
    assert!(!profile.needs_dictionary, "未读隐区不能声称实际扫描过 SA");
    assert!(folded.is_ready());
    folded.cancel();
    assert!(matches!(
        folded.query(
            &authority,
            MarkdownLineBreakUnit::Utf16,
            hidden_start + 1,
            MarkdownLineBreakQuery::Next
        ),
        Err(MarkdownLineBreakError::Poisoned)
    ));
    drop(folded);
    released(&observer, false);
    drop(original);
    // proof 只保原 Weak authority；保留显示段表不应保活正文或匿名断行文件。
    released(&observer, true);
    drop(plan);
}

#[test]
fn empty_crlf_and_surrogates_use_real_eof_and_exact_global_coordinates() {
    for (bytes, expected) in [
        (b"".as_slice(), vec![(0, 0)]),
        ("A 中\r\n😀".as_bytes(), vec![(2, 2), (7, 5), (11, 7)]),
    ] {
        let original = source(bytes);
        let authority = original.authority();
        for split in 0..=bytes.len() {
            let mut job = MarkdownLineBreakJob::new(original.clone()).unwrap();
            job.split = Some(split);
            let observer = job.resource_observer();
            finish(&mut job, &authority);
            compare(&mut job, &authority, &expected);
            let eof = job
                .query(
                    &authority,
                    MarkdownLineBreakUnit::Byte,
                    bytes.len(),
                    MarkdownLineBreakQuery::IsBoundary,
                )
                .unwrap();
            assert!(matches!(
                eof,
                MarkdownLineBreakAnswer::Known(Some(MarkdownLineBreakBoundary {
                    kind: MarkdownLineBreakKind::Mandatory,
                    ..
                }))
            ));
            job.close().unwrap();
            released(&observer, false);
        }
    }
}

#[test]
fn partial_context_does_not_publish_eot_and_sa_is_not_dictionary_ready() {
    let original = source("$ (.5 ภาษาไทย".as_bytes());
    let authority = original.authority();
    let mut job = MarkdownLineBreakJob::new(original).unwrap();
    job.step(
        &authority,
        MarkdownLineBreakBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
        },
    )
    .unwrap();
    assert_eq!(
        job.query(
            &authority,
            MarkdownLineBreakUnit::Byte,
            0,
            MarkdownLineBreakQuery::Next
        )
        .unwrap(),
        MarkdownLineBreakAnswer::Pending
    );
    assert_eq!(
        job.query(
            &authority,
            MarkdownLineBreakUnit::Byte,
            1,
            MarkdownLineBreakQuery::IsBoundary
        )
        .unwrap(),
        MarkdownLineBreakAnswer::Pending
    );
    finish(&mut job, &authority);
    assert!(job.storage_profile().needs_dictionary);
    let last = job
        .query(
            &authority,
            MarkdownLineBreakUnit::Byte,
            job.storage_profile().source_bytes,
            MarkdownLineBreakQuery::IsBoundary,
        )
        .unwrap();
    assert!(matches!(
        last,
        MarkdownLineBreakAnswer::Known(Some(MarkdownLineBreakBoundary {
            needs_dictionary: true,
            ..
        }))
    ));
}

#[test]
fn same_payload_survives_edit_undo_and_document_drop_but_new_capture_is_foreign() {
    let (_directory, document) = fixture("A 中\r\n😀".as_bytes());
    let document = Rc::new(std::cell::RefCell::new(document));
    let canonical = Rc::downgrade(&document);
    let original = MarkdownLayoutTextSource::capture(&document.borrow());
    let authority = original.authority();
    let observer_source = original.resource_observer();
    let mut job = MarkdownLineBreakJob::new(original.clone()).unwrap();
    document
        .borrow_mut()
        .insert_utf16(
            0,
            "x",
            MarkdownTextSelection {
                anchor_utf16: 1,
                head_utf16: 1,
            },
        )
        .unwrap();
    assert!(document.borrow_mut().undo().is_some());
    let foreign = MarkdownLayoutTextSource::capture(&document.borrow()).authority();
    drop(document);
    assert!(canonical.upgrade().is_none()); // 索引只保不可变source，不保活动Doc或Undo对象。
    finish(&mut job, &authority);
    compare(&mut job, &authority, &[(2, 2), (7, 5), (11, 7)]);
    assert!(matches!(
        job.query(
            &foreign,
            MarkdownLineBreakUnit::Byte,
            0,
            MarkdownLineBreakQuery::Next
        ),
        Err(MarkdownLineBreakError::SourceMismatch)
    ));
    assert!(matches!(
        job.query(
            &authority,
            MarkdownLineBreakUnit::Byte,
            0,
            MarkdownLineBreakQuery::Next
        ),
        Err(MarkdownLineBreakError::Poisoned)
    ));
    drop(job);
    assert!(!observer_source.payload_released());
    drop(original);
    assert!(observer_source.payload_released());
}

#[test]
fn actual_readonly_fd_failure_revokes_prefix_and_closes_its_original_resources() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata-control");
    std::fs::write(&path, b"unchanged").unwrap();
    let readonly = File::open(&path).unwrap();
    let original = source("a ".repeat(600).as_bytes());
    let authority = original.authority();
    let mut job = MarkdownLineBreakJob::new(original.clone()).unwrap();
    job.index = store::LineBreakIndex::test_with_file(readonly).unwrap();
    let observer = job.resource_observer();
    // 600条真实机会超过一页512条；不是让尚未交付File的私有seal冒充写失败。
    let mut failure = None;
    for _ in 0..32 {
        match job.step(&authority, MarkdownLineBreakBudget::default()) {
            Ok(_) => {}
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    assert!(
        matches!(failure, Some(MarkdownLineBreakError::Storage(error)) if error.raw_os_error() == Some(libc::EBADF))
    );
    assert!(!job.is_ready());
    assert!(matches!(
        job.query(
            &authority,
            MarkdownLineBreakUnit::Byte,
            2,
            MarkdownLineBreakQuery::IsBoundary
        ),
        Err(MarkdownLineBreakError::Poisoned)
    ));
    assert!(matches!(
        job.step(&authority, TINY),
        Err(MarkdownLineBreakError::Poisoned)
    ));
    assert!(matches!(job.close(), Err(MarkdownLineBreakError::Poisoned)));
    let closed = observer.snapshot();
    assert!(
        !closed.metadata_file_open
            && closed.metadata_buffer_bytes == 0
            && closed.metadata_map_bytes == 0
    );
    assert!(closed.cleanup_failed && !closed.source_payload_released);
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
    drop(original);
    assert!(observer.snapshot().source_payload_released);
}

#[test]
fn cross_page_queries_and_many_no_breaks_keep_one_buffer_and_no_scalar_inventory() {
    let original = source("a ".repeat(10_000).as_bytes());
    let authority = original.authority();
    let mut job = MarkdownLineBreakJob::new(original).unwrap();
    let observer = job.resource_observer();
    while job
        .step(&authority, MarkdownLineBreakBudget::default())
        .unwrap()
        .status
        == MarkdownLineBreakStatus::Pending
    {}
    assert_eq!(job.storage_profile().boundary_records, 10_000);
    for value in (1..=10_000).rev().map(|index| index * 2) {
        let result = job
            .query(
                &authority,
                MarkdownLineBreakUnit::Byte,
                value,
                MarkdownLineBreakQuery::IsBoundary,
            )
            .unwrap();
        assert!(
            matches!(result, MarkdownLineBreakAnswer::Known(Some(boundary)) if boundary.byte == value)
        );
    }
    assert!(job.storage_profile().index_page_loads > 1);
    assert_eq!(job.storage_profile().index_peak_mapped_bytes, 0);
    assert_eq!(
        observer.snapshot().metadata_peak_buffer_bytes,
        MARKDOWN_LINE_BREAK_INDEX_BYTES
    );
    assert!(job.storage_profile().source_borrow_max_bytes <= MARKDOWN_LINE_BREAK_SOURCE_BYTES);
    assert_eq!(job.storage_profile().machine_bytes, 176);
    job.close().unwrap();
    released(&observer, true);
}

#[test]
fn unicode_cross_page_strict_queries_keep_pending_prefix_and_sealed_eof() {
    // 固定字面每组只有一个emoji和一个空格，真实断行机会在空格之后（LB18）；
    // 原文每组恰为5个UTF8字节、3个UTF16单元，不能由待测索引反推期望坐标。
    // 1027组跨过两个512-record页，最后三条仍处于未满的真实追加页。
    let text = "😀 ".repeat(1027);
    let original = source(text.as_bytes());
    let authority = original.authority();
    let mut job = MarkdownLineBreakJob::new(original).unwrap();
    let observer = job.resource_observer();

    // 在旧页恰满、下一页首条和第三页首条分别暂停。这里只通过公开查询判断前缀
    // 已交付，不读取私有页号、记录槽或machine位置来制造准备完成的事实。
    for ordinal in [512, 513, 1025] {
        for _ in 0..text.len() * 4 {
            if matches!(
                job.query(
                    &authority,
                    MarkdownLineBreakUnit::Utf16,
                    ordinal * 3,
                    MarkdownLineBreakQuery::IsBoundary,
                )
                .unwrap(),
                MarkdownLineBreakAnswer::Known(Some(_))
            ) {
                break;
            }
            let step = job.step(&authority, TINY).unwrap();
            assert!(
                step.bytes <= TINY.bytes
                    && step.scalars <= TINY.scalars
                    && step.records <= TINY.records
            );
        }
        for (unit, stride) in [
            (MarkdownLineBreakUnit::Byte, 5),
            (MarkdownLineBreakUnit::Utf16, 3),
        ] {
            let at = ordinal * stride;
            // 页首相等时Previous必须交付旧页的真实前一条；Next不能以当前机会自身
            // 或尚未发布的未来记录作答。字节/代理项坐标不相同，但两边属于同一机会。
            for (offset, query, expected) in [
                (
                    at,
                    MarkdownLineBreakQuery::IsBoundary,
                    Some((ordinal * 5, ordinal * 3)),
                ),
                (
                    at,
                    MarkdownLineBreakQuery::Previous,
                    Some(((ordinal - 1) * 5, (ordinal - 1) * 3)),
                ),
                (
                    at - 1,
                    MarkdownLineBreakQuery::Next,
                    Some((ordinal * 5, ordinal * 3)),
                ),
                (at - 1, MarkdownLineBreakQuery::IsBoundary, None),
            ] {
                match job.query(&authority, unit, offset, query).unwrap() {
                    MarkdownLineBreakAnswer::Known(actual) => assert_eq!(
                        actual.map(position),
                        expected,
                        "ordinal={ordinal} unit={unit:?} offset={offset} query={query:?}"
                    ),
                    MarkdownLineBreakAnswer::Pending => panic!("已知前缀中的机会不能退回Pending"),
                }
            }
            assert_eq!(
                job.query(&authority, unit, at, MarkdownLineBreakQuery::Next)
                    .unwrap(),
                MarkdownLineBreakAnswer::Pending
            );
            // 全文EOF仍在未读尾部，不能因追加页已含机会便签出其Previous或非边界。
            for query in [
                MarkdownLineBreakQuery::IsBoundary,
                MarkdownLineBreakQuery::Previous,
            ] {
                assert_eq!(
                    job.query(&authority, unit, 1027 * stride, query).unwrap(),
                    MarkdownLineBreakAnswer::Pending
                );
            }
        }
    }

    finish(&mut job, &authority);
    // 封口后再从新旧页来回查询；零位置、真实EOF与页首两侧都保严格小于/大于，
    // 并确认半页保留不会改变同一源的完整查询结果。
    for ordinal in [0, 1, 511, 512, 513, 1023, 1024, 1025, 1026, 1027] {
        for (unit, stride) in [
            (MarkdownLineBreakUnit::Byte, 5),
            (MarkdownLineBreakUnit::Utf16, 3),
        ] {
            for query in [
                MarkdownLineBreakQuery::IsBoundary,
                MarkdownLineBreakQuery::Previous,
                MarkdownLineBreakQuery::Next,
            ] {
                let expected: Option<usize> = match query {
                    MarkdownLineBreakQuery::IsBoundary => (ordinal != 0).then_some(ordinal),
                    MarkdownLineBreakQuery::Previous => {
                        ordinal.checked_sub(1).filter(|value| *value != 0)
                    }
                    MarkdownLineBreakQuery::Next => (ordinal < 1027).then_some(ordinal + 1),
                };
                match job
                    .query(&authority, unit, ordinal * stride, query)
                    .unwrap()
                {
                    MarkdownLineBreakAnswer::Known(actual) => assert_eq!(
                        actual.map(position),
                        expected.map(|value| (value * 5, value * 3)),
                        "ordinal={ordinal} unit={unit:?} query={query:?}"
                    ),
                    MarkdownLineBreakAnswer::Pending => panic!("真实EOF封口后不能仍Pending"),
                }
            }
        }
    }
    // 原资源观察随同一生产任务close核归还，不额外保存Source、页或FD的强owner。
    job.close().unwrap();
    released(&observer, true);
}

#[test]
#[ignore = "需要ROOT官方Unicode18 LineBreakTest及显式OMNI_LB18_TEST_FILE"]
fn official_19346_every_byte_split_and_real_core_global_queries() {
    let path = std::env::var_os("OMNI_LB18_TEST_FILE").expect("需要官方ROOT fixture路径");
    let mut file = File::open(&path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    assert_eq!(
        format!("{:x}", hash.finalize()),
        "fd9ff9eca411080085b41471bba79ae19d9db2e16924f9770b8509f1affd3f95"
    );
    let mut cases: usize = 0;
    let mut jobs: usize = 0;
    let mut queries: usize = 0;
    for (line_number, line) in BufReader::new(File::open(path).unwrap())
        .lines()
        .enumerate()
    {
        let line = line.unwrap();
        let body = line.split('#').next().unwrap().trim();
        if body.is_empty() {
            continue;
        }
        let mut bytes = Vec::new();
        let mut expected = Vec::new();
        let mut utf16 = 0;
        for token in body.split_whitespace() {
            match token {
                "÷" => expected.push((bytes.len(), utf16)),
                "×" => {}
                value => {
                    let value = char::from_u32(u32::from_str_radix(value, 16).unwrap()).unwrap();
                    let mut encoded = [0; 4];
                    bytes.extend_from_slice(value.encode_utf8(&mut encoded).as_bytes());
                    utf16 += value.len_utf16();
                }
            }
        }
        assert!(bytes.len() <= 4096, "完整oracle仅限官方有限case");
        let original = source(&bytes);
        let authority = original.authority();
        cases += 1;
        for split in 0..=bytes.len() {
            let mut job = MarkdownLineBreakJob::new(original.clone()).unwrap();
            job.split = Some(split);
            let observer = job.resource_observer();
            finish(&mut job, &authority);
            queries += compare(&mut job, &authority, &expected);
            jobs += 1;
            job.close().unwrap();
            released(&observer, false);
        }
        if cases.is_multiple_of(2000) {
            eprintln!(
                "core_line_break_progress cases={cases} jobs={jobs} line={}",
                line_number + 1
            );
        }
    }
    assert_eq!(cases, 19346);
    eprintln!(
        "core_line_break_official unicode=18.0.0 revision=57 cases={cases} jobs={jobs} queries={queries} mismatches=0 product_connected=false formal=false"
    );
}
