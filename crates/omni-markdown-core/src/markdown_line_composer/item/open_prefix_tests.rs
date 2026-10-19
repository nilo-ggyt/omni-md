//! 原Source/SCX/GCB真实请求的前缀准入；不借Font构造器掩盖元数据预扫描。
use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    document: crate::MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("开放脚本请求.md");
        std::fs::write(&path, text).unwrap();
        let document =
            crate::MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        Self {
            _directory: directory,
            document,
            source,
        }
    }
    fn coordinator(&self, cancel: MarkdownLineComposeCancellation) -> MarkdownLineItemCoordinator {
        MarkdownLineItemCoordinator::new_with_frontier(
            self.source.clone(),
            &self.source.authority(),
            cancel,
        )
        .unwrap()
    }
}
fn first_style(
    worker: &mut MarkdownLineItemCoordinator,
    budget: MarkdownLineComposeBudget,
) -> (MarkdownLineStyleRequest, usize) {
    let mut bytes = 0;
    for _ in 0..1_000_000 {
        let value = worker.step(budget).unwrap();
        assert!(
            value.bytes <= budget.bytes
                && value.scalars <= budget.scalars
                && value.records <= budget.records
                && value.metadata_units <= budget.metadata_units
        );
        bytes += value.bytes;
        match value.answer {
            MarkdownLineItemAnswer::NeedStyle(request) => return (request, bytes),
            MarkdownLineItemAnswer::Yielded => {}
            _ => panic!("未选Font之前不能交付字形或伪EOF"),
        }
    }
    panic!("有限请求未完成")
}
fn small() -> MarkdownLineComposeBudget {
    MarkdownLineComposeBudget {
        bytes: 96,
        scalars: 32,
        records: 32,
        metadata_units: 32,
    }
}

#[test]
fn ordinary_han_first_style_uses_original_stable_script_and_gcb_without_complete_source() {
    let mut observed = Vec::new(); // 三个固定测试结果；不是产品的run/正文缓存。
    for length in [4096usize, 8192, 32768] {
        let fixture = Fixture::new(&"中文".repeat(length / 2));
        let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
        let (request, bytes) = first_style(&mut worker, small());
        let payload = worker.payload.as_mut().unwrap();
        assert!(payload.open_script && !payload.scripts_ready && !payload.breaks_ready);
        assert_eq!(request.script(), *b"Hani");
        assert_eq!(request.paragraph_range(), 0..length);
        assert!(request.source_range().end >= 2 && request.source_range().end < length);
        assert!(bytes < 1024);
        let prefix = payload
            .breaks
            .grapheme_prefix_facts(&fixture.source.authority())
            .unwrap();
        assert!(!prefix.complete && prefix.total_count.is_none());
        assert_eq!(
            prefix.last_committed_boundary.unwrap().utf16,
            request.source_range().end
        );
        observed.push((request.source_range(), bytes));
        let probe = worker.font_probe(&request).unwrap();
        assert_eq!(probe.source_range(), 0..1);
        assert!(probe.accepts_request(&request).unwrap());
    }
    assert!(observed.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn ordinary_ltr_greek_keeps_original_script_intersection_without_forcing_line_eof() {
    let fixture = Fixture::new("hello αλφα world");
    let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let (request, _) = first_style(&mut worker, small());
    // 期望取自原SCX策略：Greek之前的首个Latin run包含其后的space，范围0..6；
    // 原paragraph仍是完整16个UTF16单位，不能把样式范围当成新段落。
    assert_eq!(request.script(), *b"Latn");
    assert_eq!(request.source_range(), 0..6);
    assert_eq!(request.paragraph_range(), 0..16);
    let payload = worker.payload.as_mut().unwrap();
    assert!(!payload.breaks_ready);
    assert_eq!(payload.breaks.line_source_bytes_read_for_test(), 0);
    let probe = worker.font_probe(&request).unwrap();
    assert_eq!(probe.source_range(), 0..1);
}

#[test]
fn distant_latin_does_not_close_or_change_a_proven_hani_prefix() {
    let fixture = Fixture::new(&format!("{}tail", "中文".repeat(4096)));
    let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let (request, bytes) = first_style(&mut worker, small());
    assert_eq!(request.script(), *b"Hani");
    assert!(request.source_range().end < 128 && bytes < 1024);
    assert!(worker.payload.as_ref().unwrap().open_script);
    assert_eq!(request.paragraph_range(), 0..fixture.source.utf16_len());
}

#[test]
fn non_bmp_first_probe_uses_real_utf16_cluster_and_quota_one_makes_progress() {
    let fixture = Fixture::new(&"𠮷中".repeat(128));
    let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let one = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    let (request, bytes) = first_style(&mut worker, one);
    assert!(worker.payload.as_ref().unwrap().open_script);
    assert!(bytes < fixture.source.byte_len());
    let probe = worker.font_probe(&request).unwrap();
    assert_eq!(probe.source_range(), 0..2); // 原非BMP标量是一个GCB、两个UTF16单位。
    assert!(request.source_range().end >= 3);
    assert_eq!(request.paragraph_range(), 0..384);
}

#[test]
fn ambiguous_scx_and_giant_gcb_do_not_publish_guessed_partial_script_or_cluster() {
    for text in [
        format!("{}中", "ー".repeat(128)),
        format!("A{}Z", "\u{0301}".repeat(128)),
    ] {
        let fixture = Fixture::new(&text);
        let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
        let mut read = 0;
        let mut found = false;
        for _ in 0..100_000 {
            let step = worker
                .step(MarkdownLineComposeBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    metadata_units: 1,
                })
                .unwrap();
            read += step.bytes;
            if let MarkdownLineItemAnswer::NeedStyle(request) = step.answer {
                assert!(read >= 128); // 不能在仍歧义/仍在同GCB内部时凭固定短窗封存。
                assert!(request.source_range().end >= 128);
                assert!(request.source_range().end <= request.paragraph_range().end);
                found = true;
                break;
            }
            assert!(matches!(step.answer, MarkdownLineItemAnswer::Yielded));
        }
        assert!(
            found,
            "必须由完整原算法最终交付请求，不能以永久Pending保留功能"
        );
    }
}

#[test]
fn rtl_without_uniform_certificate_keeps_original_complete_dependency_admission() {
    let fixture = Fixture::new(&format!("{}אבג", "中文".repeat(64)));
    let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let (request, _) = first_style(&mut worker, small());
    let payload = worker.payload.as_ref().unwrap();
    assert!(payload.scripts_ready && payload.breaks_ready && !payload.open_script);
    assert_eq!(request.script(), *b"Hani");
    assert_eq!(request.source_range(), 0..128);
}

#[test]
fn waiting_prefix_honors_source_errno_before_cancel_and_releases_real_dependencies() {
    let fixture = Fixture::new(&"中文".repeat(4096));
    let cancel = MarkdownLineComposeCancellation::new();
    let mut worker = fixture.coordinator(cancel.clone());
    let resources = worker.resources.clone();
    first_style(&mut worker, small());
    fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(matches!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    ));
    cancel.cancel();
    assert!(matches!(
        worker.step(small()),
        Err(MarkdownLineComposeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    let snapshot = resources.observer().snapshot().unwrap();
    assert!(
        !snapshot.scripts.unwrap().run_file_open
            && !snapshot.line_breaks.unwrap().metadata_file_open
    );
}
