//! Prefix admission through actual original Source/Script/grapheme requests. Font
//! construction cannot hide metadata pre-scanning in these scenarios.
use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    document: crate::MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    fn new(text: &str) -> Self {
        // The private directory owns this real synthetic UTF-8 file and captured
        // Source. No test reads or modifies the production document vault.
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
    // Observe only the original public bounded handshake and its reported work.
    // A finite guard catches lost progress without fabricating glyphs or EOF.
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
    let mut observed = Vec::new(); // Three fixed oracle results, never a product text cache.
    for length in [4096usize, 8192, 32768] {
        let fixture = Fixture::new(&"中文".repeat(length / 2));
        let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
        let (request, bytes) = first_style(&mut worker, small());
        assert_eq!(request.script(), *b"Hani");
        assert_eq!(request.paragraph_range(), 0..length);
        assert!(request.source_range().end >= 2 && request.source_range().end < length);
        assert!(bytes < 1024);
        observed.push((request.source_range(), bytes));
        let probe = worker.font_probe(&request).unwrap();
        assert_eq!(probe.source_range(), 0..1);
        assert!(probe.accepts_request(&request).unwrap());
        // The preferred-font hint copies only the first scalar's three bytes.
        // Source length must not make Native hints relocate a distant selection.
        // This callback copy count does not describe OS-page pin or lease extent.
        let (read, first) = probe.first_scalar();
        assert_eq!(first.unwrap(), '中');
        assert_eq!(read, '中'.len_utf8());
    }
    assert!(observed.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn ordinary_ltr_greek_keeps_original_script_intersection_without_forcing_line_eof() {
    let fixture = Fixture::new("hello αλφα world");
    let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let (request, _) = first_style(&mut worker, small());
    // Original Script_Extensions includes the trailing space in Latin's 0..6 run
    // before Greek. Its original paragraph still spans all 16 UTF-16 units; a style
    // range cannot create a new paragraph.
    assert_eq!(request.script(), *b"Latn");
    assert_eq!(request.source_range(), 0..6);
    assert_eq!(request.paragraph_range(), 0..16);
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
    assert!(bytes < fixture.source.byte_len());
    let probe = worker.font_probe(&request).unwrap();
    assert_eq!(probe.source_range(), 0..2); // One non-BMP grapheme occupies two UTF-16 units.
    // Deliver the complete actual four-byte scalar to font choice, rather than
    // independently select fonts for its two UTF-16 code units.
    let (read, first) = probe.first_scalar();
    assert_eq!(first.unwrap(), '𠮷');
    assert_eq!(read, '𠮷'.len_utf8());
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
                assert!(read >= 128); // An ambiguous run/unfinished grapheme needs real original work.
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
fn rtl_tail_preserves_original_hani_request_intersection() {
    let fixture = Fixture::new(&format!("{}אבג", "中文".repeat(64)));
    let mut worker = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let (request, _) = first_style(&mut worker, small());
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
    // Exercise the existing failure injection through real mapped backing reclaim.
    // The later cancel must retain that actual typed Source cause and close indexes.
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
