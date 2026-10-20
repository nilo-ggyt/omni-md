//! 真实PingFang与原完整SCX/GCB/LB/整形双路差分；测试镜像有明确有限大小。
use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownLayoutFont, MarkdownShapedGlyph};
use std::ops::Range;

const EMPTY: MarkdownShapedGlyph = MarkdownShapedGlyph {
    glyph_id: 0,
    source_utf16_cluster: 0,
    flags: 0,
    x_advance: 0,
    y_advance: 0,
    x_offset: 0,
    y_offset: 0,
};
struct Fixture {
    _directory: tempfile::TempDir,
    _document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("实际Unicode按需行.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        Self {
            _directory: directory,
            _document: document,
            source,
        }
    }
    fn worker(
        &self,
        font: &MarkdownLayoutFont,
        frontier: bool,
    ) -> (MarkdownLineComposer, MarkdownLineRegisteredFont) {
        let catalog = MarkdownLineFontCatalog::new();
        let registered = catalog.register(font).unwrap();
        let constructor = if frontier {
            MarkdownLineComposer::new_with_frontier
        } else {
            MarkdownLineComposer::new
        };
        (
            constructor(
                self.source.clone(),
                &self.source.authority(),
                catalog,
                80 * 64,
                MarkdownLineComposeCancellation::new(),
            )
            .unwrap(),
            registered,
        )
    }
}
fn font() -> (
    MarkdownLayoutFont,
    crate::MarkdownLayoutFontBytesResourceObserver,
) {
    // 路径来自真实系统回退控制；产品的准入完全由同Font/axes/plan证书决定。
    let path = std::path::PathBuf::from(
        std::env::var_os("OMNI_FRONTIER_CJK_FONT").expect("需要真实PF回退源"),
    );
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(&path, 128 * 1024 * 1024).unwrap();
    let observer = bytes.resource_observer();
    let name = read_fonts::FontRef::from_index(bytes.as_ref(), 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let axes = [
        crate::MarkdownLayoutFontAxis {
            tag: *b"HGHT",
            value: 500.0,
        },
        crate::MarkdownLayoutFontAxis {
            tag: *b"WDTH",
            value: 500.0,
        },
        crate::MarkdownLayoutFontAxis {
            tag: *b"wght",
            value: 400.0,
        },
    ];
    let font =
        MarkdownLayoutFont::resolve_name_table_at_size(bytes.blob(), &name, &axes, 14.0).unwrap();
    assert_eq!(
        (font.metadata().face_index, font.metadata().glyph_count),
        (0, 47102)
    );
    (font, observer)
}
fn respond(
    worker: &mut MarkdownLineComposer,
    request: MarkdownLineStyleRequest,
    font: &MarkdownLineRegisteredFont,
    length: usize,
) {
    assert_eq!(request.paragraph_range(), 0..length);
    let probe = worker.font_probe(&request).unwrap();
    assert!(probe.accepts_request(&request).unwrap());
    let coverage = request.source_range();
    worker
        .provide_style(
            request
                .respond_registered(
                    coverage,
                    font,
                    MarkdownLineComposeStyle {
                        presentation: Default::default(),
                        points: 14.0,
                        language: None,
                        features: Vec::new(),
                    },
                )
                .unwrap(),
        )
        .unwrap();
}
fn step(
    worker: &mut MarkdownLineComposer,
    budget: MarkdownLineComposeBudget,
) -> MarkdownLineComposeStep {
    let value = worker.step(budget).unwrap();
    assert!(
        value.bytes <= budget.bytes
            && value.scalars <= budget.scalars
            && value.records <= budget.records
            && value.metadata_units <= budget.metadata_units
            && value.shapes <= 1
    );
    value
}
fn complete(
    worker: &mut MarkdownLineComposer,
    font: &MarkdownLineRegisteredFont,
    length: usize,
    budget: MarkdownLineComposeBudget,
    label: &str,
) {
    for _ in 0..2_000_000 {
        match step(worker, budget).answer {
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            MarkdownLineComposeAnswer::NeedsTableRow(_) => {
                panic!("未启用表格流的原文字 oracle 不应产生 Grid 请求")
            }
            MarkdownLineComposeAnswer::NeedStyle(request) => respond(worker, request, font, length),
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::Complete => return,
            MarkdownLineComposeAnswer::NeedsResolution(choice) => {
                panic!("{label}: 真实NeedsResolution={choice:?}; source_len={length}")
            }
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("{label}: 真实NeedsFrontier={reason:?}; source_len={length}")
            }
            MarkdownLineComposeAnswer::NeedsControl(control) => {
                panic!("{label}: 未响应控制={control:?}; source_len={length}")
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(_) => {
                panic!("{label}: 未响应Tab; source_len={length}")
            }
        }
    }
    panic!("{label}: 有限Unicode排版没有到真实EOF; source_len={length}")
}
#[derive(Debug, Eq, PartialEq)]
struct Record {
    range: Range<usize>,
    width: i64,
    height: i64,
    kind: MarkdownComposedLineKind,
    glyphs: Vec<MarkdownShapedGlyph>,
}
fn records(output: &mut MarkdownComposedLines) -> Vec<Record> {
    let mut result = Vec::new(); // 有限独立oracle，不进入产品状态或字体目录。
    let mut next = output.last_line().unwrap();
    while let Some(line) = next {
        let mut glyphs = Vec::new();
        for ordinal in 0..line.facts().parts {
            let part = output.part(&line, ordinal).unwrap().unwrap();
            for glyph in 0..part.facts().glyphs {
                let mut one = [EMPTY];
                output
                    .read_batch(&part, glyph..glyph + 1, &mut one)
                    .unwrap();
                glyphs.push(one[0]);
            }
        }
        result.push(Record {
            range: line.facts().source_utf16.clone(),
            width: line.facts().x_advance_26_6,
            height: line.facts().y_advance_26_6,
            kind: line.facts().kind,
            glyphs,
        });
        next = output.previous_line(&line).unwrap();
    }
    result.reverse();
    result
}

#[test]
#[ignore = "真实PF字体差分，设置OMNI_FRONTIER_CJK_FONT"]
fn pingfang_hani_first_line_work_is_independent_of_4k_8k_32k_source() {
    let (font, font_observer) = font();
    let mut observed = Vec::new();
    for length in [4096usize, 8192, 32768] {
        let fixture = Fixture::new(&"中文".repeat(length / 2));
        let backing = fixture.source.original_backing_observer().unwrap();
        let source_observer = fixture.source.resource_observer();
        let (mut worker, registered) = fixture.worker(&font, true);
        let resources = worker.worker_resource_observer();
        let (mut bytes, mut records_read, mut first) = (0, 0, None);
        for _ in 0..100_000 {
            let progress = step(&mut worker, MarkdownLineComposeBudget::default());
            bytes += progress.bytes;
            records_read += progress.metadata_units;
            match progress.answer {
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    assert!(request.source_range().end < length);
                    fixture
                        ._document
                        .reclaim_original_read_windows_for_test()
                        .unwrap();
                    let before = (backing.managed_resident_bytes(), backing.reclaim_attempts());
                    assert_eq!(before.0, 0);
                    respond(&mut worker, request, &registered, length);
                    assert_eq!(
                        (backing.managed_resident_bytes(), backing.reclaim_attempts()),
                        before,
                        "开放构造必须消费已证GCB byte票，不能再读原Source定位UTF16"
                    );
                }
                MarkdownLineComposeAnswer::LineReady(line) => {
                    first = Some(line);
                    break;
                }
                MarkdownLineComposeAnswer::Yielded => {}
                _ => panic!("真实Hani首行不能转为全源处理或虚假EOF"),
            }
        }
        let first = first.expect("真实首LineEnd");
        let (_, breaks) = worker
            .payload
            .as_mut()
            .unwrap()
            .coordinator
            .dependencies()
            .unwrap();
        let prefix = breaks
            .grapheme_prefix_facts(&fixture.source.authority())
            .unwrap();
        assert!(!prefix.complete && prefix.total_count.is_none());
        let lb = breaks.line_source_bytes_read_for_test();
        assert!(bytes < 4096 && lb < 1024 && first.source_utf16.end < length);
        observed.push((
            bytes,
            records_read,
            lb,
            first.source_utf16,
            worker.frontier_work().unwrap(),
        ));
        let output = worker.into_output().unwrap();
        assert!(output.source_range().end < length);
        output.close().unwrap();
        drop((registered, fixture));
        assert!(resources.is_released() && source_observer.payload_released());
        assert!(backing.unmap_succeeded() && !backing.failed());
    }
    assert!(observed.windows(2).all(|pair| pair[0] == pair[1]));
    eprintln!("hani_prefix_actual={observed:?}");
    drop(font);
    assert!(font_observer.mapping_drop_completed());
}

#[test]
#[ignore = "真实PF字体差分，设置OMNI_FRONTIER_CJK_FONT"]
fn pingfang_alternating_hani_latin_and_punctuation_matches_original_complete_output() {
    let (font, _) = font();
    for (fixture_index, text) in [
        "中文abc，测试xyz。".repeat(12),
        format!("({})", "中文A中文BC".repeat(8)),
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = Fixture::new(&text);
        let (mut lazy, lazy_font) = fixture.worker(&font, true);
        let (mut full, full_font) = fixture.worker(&font, false);
        let length = fixture.source.utf16_len();
        complete(
            &mut lazy,
            &lazy_font,
            length,
            MarkdownLineComposeBudget::default(),
            &format!("lazy mixed fixture={fixture_index}"),
        );
        complete(
            &mut full,
            &full_font,
            length,
            MarkdownLineComposeBudget::default(),
            &format!("full mixed fixture={fixture_index}"),
        );
        let mut lazy = lazy.into_output().unwrap();
        let mut full = full.into_output().unwrap();
        assert_eq!(records(&mut lazy), records(&mut full));
        for offset in 0..=length {
            assert_eq!(
                lazy.grapheme_query(offset, crate::MarkdownGraphemeQuery::IsBoundary)
                    .unwrap(),
                full.grapheme_query(offset, crate::MarkdownGraphemeQuery::IsBoundary)
                    .unwrap()
            );
        }
        lazy.close().unwrap();
        full.close().unwrap();
    }
}

#[test]
#[ignore = "真实PF字体差分，设置OMNI_FRONTIER_CJK_FONT"]
fn pingfang_quota_one_non_bmp_continuation_keeps_old_output_prefix_and_exact_global_clusters() {
    let (font, _) = font();
    let fixture = Fixture::new(&"𠮷中文".repeat(16));
    let length = fixture.source.utf16_len();
    let (mut worker, registered) = fixture.worker(&font, true);
    let one = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    let mut old = None;
    for _ in 0..2_000_000 {
        match step(&mut worker, one).answer {
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                respond(&mut worker, request, &registered, length)
            }
            MarkdownLineComposeAnswer::LineReady(_) => {
                old = worker.publish_ready_prefix().unwrap();
                break;
            }
            MarkdownLineComposeAnswer::Yielded => {}
            _ => panic!("真实非BMP首行未交付"),
        }
    }
    let mut old = old.expect("原只读前缀");
    let old_range = old.source_range();
    let old_records = records(&mut old);
    assert!(old_range.end < length);
    assert_eq!(
        old.grapheme_query(length, crate::MarkdownGraphemeQuery::Next)
            .unwrap(),
        crate::MarkdownGraphemeAnswer::Pending
    );
    complete(&mut worker, &registered, length, one, "lazy nonBMP quota1");
    let mut actual = worker.into_output().unwrap();
    let (mut full, full_font) = fixture.worker(&font, false);
    complete(
        &mut full,
        &full_font,
        length,
        MarkdownLineComposeBudget::default(),
        "full nonBMP oracle",
    );
    let mut expected = full.into_output().unwrap();
    assert_eq!(records(&mut actual), records(&mut expected));
    assert_eq!(old.source_range(), old_range);
    assert_eq!(records(&mut old), old_records);
    assert_eq!(
        old.grapheme_query(length, crate::MarkdownGraphemeQuery::IsBoundary)
            .unwrap(),
        crate::MarkdownGraphemeAnswer::Pending
    );
    old.close().unwrap();
    actual.close().unwrap();
    expected.close().unwrap();
}

// 此夹具主动把独立Hani计划切成显式样式项，仅检验响应时序及同源坐标；
// 逐GCB响应不是通用字体fallback策略，不能据此拆开Latin连字或Arabic上下文。
fn respond_one_grapheme(
    worker: &mut MarkdownLineComposer,
    request: MarkdownLineStyleRequest,
    font: &MarkdownLineRegisteredFont,
    length: usize,
) {
    assert_eq!(request.paragraph_range(), 0..length);
    let probe = worker.font_probe(&request).unwrap();
    let coverage = probe.source_range();
    assert!(probe.accepts_request(&request).unwrap());
    worker
        .provide_style(
            request
                .respond_registered(
                    coverage,
                    font,
                    MarkdownLineComposeStyle {
                        presentation: Default::default(),
                        points: 14.0,
                        language: None,
                        features: Vec::new(),
                    },
                )
                .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
}
fn complete_short_style(
    worker: &mut MarkdownLineComposer,
    font: &MarkdownLineRegisteredFont,
    length: usize,
    budget: MarkdownLineComposeBudget,
) {
    for _ in 0..2_000_000 {
        match step(worker, budget).answer {
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                respond_one_grapheme(worker, request, font, length)
            }
            MarkdownLineComposeAnswer::LineReady(_) | MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::Complete => return,
            MarkdownLineComposeAnswer::NeedsResolution(choice) => {
                panic!("短样式真实NeedsResolution={choice:?}")
            }
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("短样式真实NeedsFrontier={reason:?}")
            }
            _ => panic!("无控制正控不能转为未消费控制"),
        }
    }
    panic!("有限真实短样式未到EOF")
}

#[test]
#[ignore = "真实PF短样式及旧prefix差分，设置OMNI_FRONTIER_CJK_FONT"]
fn pingfang_short_style_queued_endpoint_preserves_old_prefix_and_matches_synchronous_original() {
    let (font, _) = font();
    let fixture = Fixture::new(&"中𠮷文".repeat(12));
    let length = fixture.source.utf16_len();
    let (mut worker, registered) = fixture.worker(&font, true);
    let one = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    let mut old = None;
    for _ in 0..2_000_000 {
        match step(&mut worker, one).answer {
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                respond_one_grapheme(&mut worker, request, &registered, length)
            }
            MarkdownLineComposeAnswer::LineReady(_) => {
                old = worker.publish_ready_prefix().unwrap();
                break;
            }
            MarkdownLineComposeAnswer::Yielded => {}
            _ => panic!("短样式首行没有交付真实LineEnd"),
        }
    }
    let mut old = old.expect("真实旧LineEnd前缀");
    let old_range = old.source_range();
    let old_records = records(&mut old);
    assert!(old_range.end < length);
    complete_short_style(&mut worker, &registered, length, one);
    let mut actual = worker.into_output().unwrap();
    let (mut full, full_font) = fixture.worker(&font, false);
    complete_short_style(
        &mut full,
        &full_font,
        length,
        MarkdownLineComposeBudget::default(),
    );
    let mut expected = full.into_output().unwrap();
    assert_eq!(records(&mut actual), records(&mut expected));
    assert_eq!(old.source_range(), old_range);
    assert_eq!(records(&mut old), old_records);
    assert_eq!(
        old.grapheme_query(length, crate::MarkdownGraphemeQuery::IsBoundary)
            .unwrap(),
        crate::MarkdownGraphemeAnswer::Pending
    );
    old.close().unwrap();
    actual.close().unwrap();
    expected.close().unwrap();
}
