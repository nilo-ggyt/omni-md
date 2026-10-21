//! Actual long words under the original UIFont body positioning plan, checked against independent
//! complete jobs and independently reshaped prefixes.
use super::*;
use crate::{
    MarkdownEmergencyGlyphSliceAnswer, MarkdownEmergencyGlyphSliceJob,
    MarkdownSoftLineResolveBudget,
};

fn body_font() -> MarkdownLayoutFont {
    // Use the captured actual body role, axes, and 14-point size. This explicit ignored test requires
    // its real Native input; substituting another role would not establish body-font behavior.
    let root = std::path::PathBuf::from(
        std::env::var_os("OMNI_FRONTIER_UIFONT_FIXTURES").expect("需要真实UIFont输入"),
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("fonts.json")).unwrap()).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["label"] == "body_system")
        .unwrap();
    assert_eq!(
        row["points"].as_f64().unwrap(),
        14.0,
        "本控制必须使用真实14pt正文，不能改用monospace"
    );
    let axes: Vec<_> = row["axes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|axis| crate::MarkdownLayoutFontAxis {
            tag: (axis["tag"].as_u64().unwrap() as u32).to_be_bytes(),
            value: axis["value"].as_f64().unwrap() as f32,
        })
        .collect();
    MarkdownLayoutFont::resolve_name_table_at_size(
        Arc::new(std::fs::read(row["path"].as_str().unwrap()).unwrap()),
        &std::fs::read(root.join(row["name_file"].as_str().unwrap())).unwrap(),
        &axes,
        14.0,
    )
    .unwrap()
}

fn first_word(
    fixture: &mut Fixture,
) -> (
    MarkdownShapingFrontierJob,
    MarkdownShapingJob,
    MarkdownShapingFrontierContinuation,
    (usize, usize, usize, usize, usize),
) {
    let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
    let mut work = (0, 0, 0, 0, 0);
    for _ in 0..2_000_000 {
        let step = frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget {
                    bytes: 17,
                    scalars: 17,
                },
                1,
            )
            .unwrap();
        assert!(step.source_bytes + step.input_bytes <= 17);
        assert!(step.plan_records + step.metadata_glyphs <= 1);
        work.0 += step.source_bytes;
        work.1 += step.input_bytes;
        work.2 += step.context_bytes;
        work.3 += step.plan_records;
        work.4 += step.metadata_glyphs;
        match step.answer {
            MarkdownShapingFrontierAnswer::Pending => {}
            MarkdownShapingFrontierAnswer::PartReady => {
                assert!(frontier.positioned_proof.is_some());
                let (job, ticket) = frontier
                    .take_part(&fixture.authority, &fixture.font_authority)
                    .unwrap()
                    .into_parts();
                return (frontier, job, ticket, work);
            }
            other => panic!("原正文尚未取得真实定位证书: {other:?}; work={work:?}"),
        }
    }
    panic!("真实有限字体记录没有完成")
}

fn measured(
    fixture: &mut Fixture,
    job: &mut MarkdownShapingJob,
    width: i64,
) -> (
    MarkdownGlyphClusterIndex,
    MarkdownSoftLineMeasureJob,
    MarkdownSoftLineMeasureChoice,
) {
    let mut clusters =
        MarkdownGlyphClusterIndex::new(job, &fixture.authority, &fixture.font_authority).unwrap();
    while clusters.status() != MarkdownGlyphClusterStatus::Ready {
        clusters.step(job, 7).unwrap();
    }
    let mut measure = MarkdownSoftLineMeasureJob::new(
        fixture.source.clone(),
        &fixture.authority,
        &fixture.font_authority,
        job,
        &mut fixture.breaks,
        MarkdownSoftLineMeasureRequest {
            start_utf16: 0,
            width_26_6: width,
            cancellation: MarkdownSoftLineMeasureCancellation::default(),
        },
    )
    .unwrap();
    for _ in 0..4096 {
        let step = measure
            .step(
                job,
                &mut clusters,
                &mut fixture.breaks,
                MarkdownSoftLineMeasureBudget {
                    clusters: 1,
                    break_queries: 1,
                },
            )
            .unwrap();
        assert!(step.clusters <= 1 && step.break_queries <= 1);
        if let MarkdownSoftLineMeasureAnswer::Selected(choice) = step.answer {
            return (clusters, measure, choice);
        }
    }
    panic!("有界正文前缀未选中首行")
}

/// Reshape actual finite GCB prefixes independently of frontier certificates and the measure
/// selector, locating the last fitting prefix for this kerning regression.
fn independent_line(fixture: &Fixture, width: i64) -> (usize, i64) {
    let mut last = (0, 0);
    let mut prior = 0;
    for end in 1..=64 {
        let mut options = fixture.options();
        options.end_of_text = end == fixture.source.utf16_len();
        let mut job = MarkdownShapingJob::new(
            fixture.source.clone(),
            &fixture.authority,
            fixture.font.clone(),
            &fixture.font_authority,
            0..end,
            options,
        )
        .unwrap();
        while job.status() == MarkdownShapingStatus::Ingesting {
            job.step(
                &fixture.authority,
                &fixture.font_authority,
                MarkdownShapingBudget::default(),
            )
            .unwrap();
        }
        job.shape(&fixture.authority, &fixture.font_authority)
            .unwrap();
        let mut advance = 0;
        for i in 0..job.profile().unwrap().glyph_count {
            advance += i64::from(
                job.glyph(&fixture.authority, &fixture.font_authority, i)
                    .unwrap()
                    .unwrap()
                    .x_advance,
            );
        }
        assert!(
            advance >= prior,
            "本有限oracle的每个实际候选宽度必须递增，否则不能以首超宽处停止"
        );
        if advance > width {
            return last;
        }
        prior = advance;
        last = (end, advance);
    }
    panic!("本控制应在64个真实候选内超宽")
}

#[test]
#[ignore = "显式实际Body14pt字体输入与长词工作计量"]
fn actual_body_positioned_word_first_line_is_constant_and_exact_for_4k_8k_32k() {
    let mut observed = Vec::new(); // Three finite scalar observations, not a production Vec growing with body length.
    for size in [4096, 8192, 32768] {
        let mut fixture = Fixture::new(&"A".repeat(size), body_font());
        let source = fixture.source.resource_observer();
        let backing = fixture.document.original_backing_observer().unwrap();
        let font = fixture.font.resource_observer();
        let (mut frontier, mut child, ticket, work) = first_word(&mut fixture);
        assert_eq!(child.source_range(), 0..256);
        assert_eq!(
            child
                .frontier_word_end(&fixture.authority, &fixture.font_authority)
                .unwrap(),
            Some(size)
        );
        assert!(work.1 < 512 && work.2 <= 40);
        let (clusters, measure, choice) = measured(&mut fixture, &mut child, 5000);
        let MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak {
            source_utf16,
            x_advance_26_6,
            ..
        } = choice
        else {
            panic!("A链应可真实安全复用")
        };
        let oracle = independent_line(&fixture, 5000);
        assert_eq!((source_utf16.end, x_advance_26_6), oracle);
        let mut full = fixture.oracle(0..size);
        for i in 0..child.profile().unwrap().glyph_count {
            assert_eq!(
                child
                    .glyph(&fixture.authority, &fixture.font_authority, i)
                    .unwrap(),
                full.glyph(&fixture.authority, &fixture.font_authority, i)
                    .unwrap()
            );
        }
        observed.push((work, source_utf16.end, x_advance_26_6));
        drop(full);
        drop(measure);
        drop(clusters);
        let resources = child.resource_observer();
        drop(child);
        assert_eq!(resources.snapshot().files_open, 0);
        assert_eq!(resources.snapshot().mappings_open, 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, ticket)
            .unwrap();
        drop(frontier);
        drop(fixture);
        assert!(source.payload_released() && font.payload_released() && backing.unmap_succeeded());
    }
    eprintln!("actual_body_word_first work={observed:?}");
    assert!(observed.windows(2).all(|p| p[0] == p[1]));
}

#[test]
#[ignore = "显式实际Body14pt kern链与独立行断点oracle"]
fn actual_body_kern_chain_does_not_choose_an_earlier_safe_cut() {
    // Compare the actual selected/resolved line against independent prefix shaping; choosing an
    // earlier reusable boundary would lose fitting content even if that earlier cut were safe.
    let text = format!("XX{}", "AV".repeat(4095));
    let mut fixture = Fixture::new(&text, body_font());
    let (frontier, mut child, _ticket, _work) = first_word(&mut fixture);
    let (mut clusters, measure, choice) = measured(&mut fixture, &mut child, 5000);
    let expected = independent_line(&fixture, 5000);
    let actual = match choice {
        MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak {
            source_utf16,
            x_advance_26_6,
            ..
        } => (source_utf16.end, x_advance_26_6),
        MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix { .. } => {
            let mut resolve =
                MarkdownEmergencyGlyphSliceJob::new_prefix_only(&measure, &mut child).unwrap();
            let mut found = None;
            for _ in 0..4096 {
                let step = resolve
                    .step(
                        &measure,
                        &mut child,
                        &mut clusters,
                        &mut fixture.breaks,
                        MarkdownSoftLineResolveBudget::default(),
                    )
                    .unwrap();
                match step.answer {
                    MarkdownEmergencyGlyphSliceAnswer::ReusableLine(boundary) => {
                        found = Some((boundary.source_utf16.end, boundary.x_advance_26_6));
                        break;
                    }
                    MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(boundary) => {
                        found = Some((boundary.source_utf16.end, boundary.x_advance_26_6));
                        break;
                    }
                    MarkdownEmergencyGlyphSliceAnswer::Yielded
                    | MarkdownEmergencyGlyphSliceAnswer::PendingDependencies => {}
                    other => panic!("原真实prefix resolver没有完成: {other:?}"),
                }
            }
            resolve.close().unwrap();
            found.expect("有限kern链未完成真实子行")
        }
        other => panic!("原Body词缀未能选行: {other:?}"),
    };
    assert_eq!(actual, expected, "不能为了复用切回更早的安全边界");
    drop(frontier);
}
