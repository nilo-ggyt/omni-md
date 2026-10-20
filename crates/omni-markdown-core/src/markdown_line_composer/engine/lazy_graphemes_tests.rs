//! 验证真实公开Style握手与Ready item之间的延迟复制，不用直接调用Store假装接通Engine。
use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownLayoutFont};
use std::sync::Arc;

#[test]
fn style_needs_no_output_gcb_and_ready_item_copies_only_its_actual_prefix_with_real_quotas() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("真实延迟字素.md");
    let text = "aa ".repeat(128); // 固定有限oracle；生产只保原Source。
    std::fs::write(&path, &text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let catalog = MarkdownLineFontCatalog::new();
    let font = MarkdownLayoutFont::resolve(
        Arc::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
            ))
            .as_slice(),
        ),
        "Roboto-Regular",
        &[],
    )
    .unwrap();
    let font = catalog.register(&font).unwrap();
    let mut worker = MarkdownLineComposer::new(
        source.clone(),
        &source.authority(),
        catalog,
        40 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let budget = MarkdownLineComposeBudget {
        bytes: 4096,
        scalars: 4096,
        records: 1,
        metadata_units: 1,
    };
    let mut request = None;
    for _ in 0..100_000 {
        let progress = worker.step(budget).unwrap();
        if let MarkdownLineComposeAnswer::NeedStyle(value) = progress.answer {
            request = Some(value);
            break;
        }
    }
    let request = request.expect("原元数据完成后真实NeedStyle");
    {
        let payload = worker.payload.as_mut().unwrap();
        assert_eq!(payload.store.record_count().unwrap(), 0);
        let (_, breaks) = payload.coordinator.dependencies().unwrap();
        assert_eq!(
            breaks.ready_grapheme_count(&source.authority()).unwrap(),
            Some(text.len() + 1)
        );
    } // 原GCB已完整Ready不要求Store也复制全源，样式请求不授予输出glyph权限。
    worker
        .provide_style(
            request
                .respond_registered(
                    0..12,
                    &font,
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
    let (mut reads, mut writes) = (0, 0);
    for _ in 0..100_000 {
        let migrating = matches!(worker.payload.as_ref().unwrap().stage, Stage::Graphemes);
        let progress = worker.step(budget).unwrap();
        if migrating {
            assert!(progress.metadata_units <= 1 && progress.records <= 1);
            assert_eq!(
                (progress.bytes, progress.scalars, progress.shapes),
                (0, 0, 0)
            );
            reads += progress.metadata_units;
            writes += progress.records;
        }
        if matches!(worker.payload.as_ref().unwrap().stage, Stage::Capture) {
            let payload = worker.payload.as_ref().unwrap();
            assert_eq!(payload.item.as_ref().unwrap().facts().source_utf16, 0..12);
            assert_eq!(
                (reads, writes, payload.store.record_count().unwrap()),
                (13, 13, 13)
            );
            return;
        }
    }
    panic!("真实12unit item未进入原capture阶段");
}
