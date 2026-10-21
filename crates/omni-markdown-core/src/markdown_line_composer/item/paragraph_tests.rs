//! Exercise the original coordinator's real bidi→Script/grapheme→Style chain on
//! long CRLF Sources without scanning the complete suffix before first output.

use super::*;
use crate::MarkdownDocumentBuffer;
use std::io::Write;

#[test]
fn crlf_prefix_of_four_mebibyte_source_reaches_real_first_style_with_bounded_reads() {
    // Write the synthetic four-mebibyte suffix in fixed blocks. The captured Source
    // remains mmap-backed; only original bounded preparation can reach NeedStyle.
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"first\r\nsecond\r\n").unwrap();
    let block = [b'a'; 4096];
    for _ in 0..1024 {
        file.write_all(&block).unwrap();
    }
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 8 * 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mapping = document.original_backing_observer().unwrap();
    let payload = source.resource_observer();
    for budget in [
        MarkdownLineComposeBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
            metadata_units: 1,
        },
        MarkdownLineComposeBudget::default(),
    ] {
        let mut coordinator = MarkdownLineItemCoordinator::new(
            source.clone(),
            &authority,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        coordinator.enable_frontier().unwrap();
        let mut bytes = 0;
        let request = loop {
            let step = coordinator.step(budget).unwrap();
            assert!(
                step.bytes <= budget.bytes
                    && step.scalars <= budget.scalars
                    && step.metadata_units <= budget.metadata_units
            );
            bytes += step.bytes;
            match step.answer {
                MarkdownLineItemAnswer::NeedStyle(request) => break request,
                MarkdownLineItemAnswer::Yielded => {}
                _ => panic!("首个真实文本段必须交原样式请求，不能转为假EOF或能力拒绝"),
            }
        };
        assert_eq!(request.source_range(), 0..5);
        assert_eq!(request.paragraph_range(), 0..6); // Original UBA ends at CR; LF begins another B paragraph.
        assert!(bytes < 32 * 1024, "首样式不能先读取四MiB后继段");
        // Observe the actual original bidi resources before dropping the worker;
        // Source/payload release is checked independently after all worker rounds.
        let bidi = coordinator
            .payload
            .as_ref()
            .unwrap()
            .bidi
            .as_ref()
            .unwrap()
            .resource_observer();
        assert!(!bidi.snapshot().file_created && bidi.snapshot().cache_bytes == 0);
        drop((request, coordinator));
        assert!(bidi.is_released());
    }
    drop((source, document));
    assert!(payload.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn leading_and_intermediate_crlf_keep_original_dependencies_and_next_real_style() {
    use crate::{
        MarkdownLineComposeAnswer, MarkdownLineComposer, MarkdownLineFontCatalog,
        MarkdownLineParagraphStyle,
    };

    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"\r\nfirst\r\nsecond\r\n").unwrap();
    let block = [b'a'; 4096];
    for _ in 0..1024 {
        file.write_all(&block).unwrap();
    }
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 8 * 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mapping = document.original_backing_observer().unwrap();
    let payload = source.resource_observer();
    // Use the repository's actual Latin font for the first two finite words only.
    // The four-mebibyte suffix stays in the same mmap Source, outside font input.
    const ROBOTO: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
    ));
    let name = read_fonts::FontRef::from_index(ROBOTO, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let font =
        MarkdownLayoutFont::resolve_name_table_at_size(Arc::new(ROBOTO), &name, &[], 16.0).unwrap();
    let mut resources = Vec::new(); // Two actual worker observers retain no Source/file owner.
    for budget in [
        MarkdownLineComposeBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
            metadata_units: 1,
        },
        MarkdownLineComposeBudget::default(),
    ] {
        let catalog = MarkdownLineFontCatalog::new();
        let registered = catalog.register(&font).unwrap();
        let mut worker = MarkdownLineComposer::new_with_frontier(
            source.clone(),
            &authority,
            catalog,
            1024 * 64,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        resources.push(worker.worker_resource_observer());
        // Assert the externally issued text/control order, including all original
        // CRLF bytes; private preparation stages are not the expected outcome.
        let mut expected = [
            (true, 0..2),
            (false, 2..7),
            (true, 7..9),
            (false, 9..15),
            (true, 15..17),
        ]
        .into_iter();
        let mut bytes = 0;
        let mut reached_tail = false;
        for _ in 0..1_000_000 {
            let step = worker.step(budget).unwrap();
            assert!(
                step.bytes <= budget.bytes
                    && step.scalars <= budget.scalars
                    && step.records <= budget.records
                    && step.metadata_units <= budget.metadata_units
                    && step.shapes <= 1
            );
            bytes += step.bytes;
            match step.answer {
                MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    if let Some((control, next)) = expected.next() {
                        assert!(!control);
                        assert_eq!(range, next);
                        worker
                            .provide_style(
                                request
                                    .respond_registered(
                                        range,
                                        &registered,
                                        MarkdownLineComposeStyle {
                                            presentation: Default::default(),
                                            points: 16.0,
                                            language: None,
                                            features: Vec::new(),
                                        },
                                    )
                                    .unwrap(),
                            )
                            .unwrap();
                    } else {
                        assert_eq!(range.start, 17);
                        assert!(range.end > range.start);
                        assert_eq!(request.paragraph_range(), 17..source.utf16_len());
                        reached_tail = true;
                        break; // Reached the real suffix after two texts and three CRLFs.
                    }
                }
                MarkdownLineComposeAnswer::NeedsControl(request) => {
                    let (control, next) = expected.next().expect("真实CRLF不能重复或越过预期文本");
                    assert!(control);
                    assert_eq!(request.source_utf16, next);
                    assert_eq!(request.kind, MarkdownLineInlineControlKind::CarriageReturn);
                    worker
                        .provide_control(
                            request
                                .respond(
                                    &registered,
                                    16.0,
                                    MarkdownLineParagraphStyle {
                                        tab_stops: Vec::new(),
                                        default_tab_interval_26_6: None,
                                    },
                                )
                                .unwrap(),
                        )
                        .unwrap();
                }
                _ => panic!("原控制、文本和后继样式之间不能拒绝、假EOF或遗失原依赖"),
            }
        }
        assert!(reached_tail && bytes < 32 * 1024);
        // Release the unconsumed following request and this worker's real metadata;
        // no cache or consumer survives into the next budget round.
        drop((worker, registered));
    }
    drop((font, source, document));
    assert!(payload.payload_released());
    assert!(resources.iter().all(|observer| observer.is_released()));
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn actual_255_byte_crlf_prefix_and_four_mebibyte_tail_publish_a_real_tail_line_bounded() {
    use crate::{
        MarkdownLineComposeAnswer, MarkdownLineComposer, MarkdownLineFontCatalog,
        MarkdownLineParagraphStyle,
    };
    // Keep the actual 255-byte seam that precedes the long continuous suffix;
    // resizing or inserting another separator would bypass this continuation case.
    let prefix = format!("{}\r\nsecond word\r\n", "alpha ".repeat(40));
    assert_eq!(prefix.len(), 255);
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(prefix.as_bytes()).unwrap();
    file.write_all(b"X").unwrap();
    file.write_all(&[b'a'; 4095]).unwrap();
    for _ in 0..1023 {
        file.write_all(&[b'a'; 4096]).unwrap();
    }
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 8 * 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let payload = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    const ROBOTO: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
    ));
    let name = read_fonts::FontRef::from_index(ROBOTO, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let font =
        MarkdownLayoutFont::resolve_name_table_at_size(Arc::new(ROBOTO), &name, &[], 16.0).unwrap();
    let catalog = MarkdownLineFontCatalog::new();
    let registered = catalog.register(&font).unwrap();
    let mut worker = MarkdownLineComposer::new_with_frontier(
        source.clone(),
        &authority,
        catalog,
        640 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let resources = worker.worker_resource_observer();
    let mut bytes = 0;
    // Advance the real full composition chain until a committed soft line starts
    // in the suffix. A first Style request alone would not prove this regression.
    let mut tail = None;
    for _ in 0..200_000 {
        let budget = MarkdownLineComposeBudget::default();
        let step = worker.step(budget).unwrap();
        assert!(
            step.bytes <= budget.bytes
                && step.scalars <= budget.scalars
                && step.records <= budget.records
                && step.metadata_units <= budget.metadata_units
                && step.shapes <= 1
        );
        bytes += step.bytes;
        match step.answer {
            MarkdownLineComposeAnswer::Yielded => {}
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            MarkdownLineComposeAnswer::NeedsTableRow(_) => {
                panic!("未启用表格流的原文字 oracle 不应产生 Grid 请求")
            }
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(
                                range,
                                &registered,
                                MarkdownLineComposeStyle {
                                    presentation: Default::default(),
                                    points: 16.0,
                                    language: None,
                                    features: Vec::new(),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => {
                assert_eq!(request.kind, MarkdownLineInlineControlKind::CarriageReturn);
                worker
                    .provide_control(
                        request
                            .respond(
                                &registered,
                                16.0,
                                MarkdownLineParagraphStyle {
                                    tab_stops: Vec::new(),
                                    default_tab_interval_26_6: None,
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::LineReady(line) => {
                if line.source_utf16.start >= prefix.len() {
                    tail = Some(line);
                    break;
                }
            }
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("真实CRLF尾段仍缺原整形能力：reason={reason:?} actual_source_bytes={bytes}")
            }
            MarkdownLineComposeAnswer::NeedsResolution(choice) => {
                panic!("真实CRLF尾段仍需原断行决策：choice={choice:?} actual_source_bytes={bytes}")
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(_) => {
                panic!("无TAB夹具不得请求TAB放置：actual_source_bytes={bytes}")
            }
            MarkdownLineComposeAnswer::Complete => {
                panic!("四MiB尾段首行前不能完成整源：actual_source_bytes={bytes}")
            }
        }
    }
    let tail = tail.expect("必须跨真实CRLF后发布尾段真实软行，不能只取得首样式便宣称成功");
    assert_eq!(tail.source_utf16.start, prefix.len());
    assert!(tail.source_utf16.end > prefix.len() && tail.source_utf16.end < source.utf16_len());
    assert!(bytes < 32 * 1024, "真实尾段首行不能先扫描四MiB后继正文");
    // Close actual published output, worker indexes, fonts, and mapped Source.
    // Real backing observations remain available without extending their owners.
    worker.into_output().unwrap().close().unwrap();
    drop((registered, font, source, document));
    assert!(resources.is_released() && payload.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}
