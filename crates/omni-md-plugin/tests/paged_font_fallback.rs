//! 真正串接Source/GCB/Script/系统级联/HarfRust的主线程控制；线程只由本driver显式创建。
use std::{
    ops::Range,
    sync::{Arc, Barrier},
};

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFRetained, CFString, CFURL};
use objc2_core_graphics::CGFont;
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontOptions, kCTFontNameAttribute, kCTFontURLAttribute,
};
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownGraphemeBudget, MarkdownGraphemeJob,
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource, MarkdownScriptBudget,
    MarkdownScriptCancellation, MarkdownScriptJob, MarkdownScriptStatus, MarkdownShapingDirection,
    MarkdownShapingError, MarkdownShapingOptions,
};
use omni_md_plugin::native_surface::{
    paged_font_cascade::{NativeMarkdownFontCascade, NativeMarkdownFontCascadeBudget},
    paged_font_fallback::{
        NativeMarkdownFontFallback, NativeMarkdownFontFallbackCompletion,
        NativeMarkdownFontFallbackError, NativeMarkdownFontFallbackRequest,
        NativeMarkdownFontFallbackStep, NativeMarkdownFontOrigin, NativeMarkdownFontTrial,
    },
    paged_fonts::NativeMarkdownPagedFont,
};

const MAX_FONT_BYTES: u64 = 512 * 1024 * 1024;
const LANGUAGES: &[&str] = &["zh-Hans", "en", "ar"];
#[cfg(feature = "native-paged-compose-probe")]
#[path = "paged_font_fallback/composed.rs"]
mod composed;
#[path = "paged_font_fallback/request_boundaries.rs"]
mod request_boundaries;
#[path = "paged_font_fallback/worker.rs"]
mod worker;
struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    graphemes: MarkdownGraphemeJob,
    scripts: MarkdownScriptJob,
}
impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("真实回退字素.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let graphemes = MarkdownGraphemeJob::new(source.clone()).unwrap();
        let scripts = MarkdownScriptJob::new(
            source.clone(),
            &authority,
            MarkdownScriptCancellation::new(),
        )
        .unwrap();
        Self {
            _directory: directory,
            document,
            source,
            authority,
            graphemes,
            scripts,
        }
    }
    fn prepare(&mut self) {
        while !self.graphemes.is_ready() {
            self.graphemes
                .step(&self.authority, MarkdownGraphemeBudget::default())
                .unwrap();
        }
        loop {
            if self
                .scripts
                .step(&self.authority, MarkdownScriptBudget::default())
                .unwrap()
                .status
                == MarkdownScriptStatus::Ready
            {
                break;
            }
        } // 原全局索引实际完成后再消费，chunk不是GCB/script边界。
    }
}
fn base(mtm: MainThreadMarker) -> NativeMarkdownPagedFont {
    let name = CFString::from_str("Helvetica");
    // SAFETY: 固定字体名与18pt正字号，没有文本输入或矩阵指针。
    let native = unsafe { CTFont::with_name(&name, 18.0, std::ptr::null()) };
    NativeMarkdownPagedFont::capture(&native, MAX_FONT_BYTES, mtm, &|| true).unwrap()
}

fn inspect_second_candidate_source(mtm: MainThreadMarker) {
    let base = base(mtm);
    let authority = base.core_font().authority();
    let mut cascade = NativeMarkdownFontCascade::capture(
        &base,
        &authority,
        LANGUAGES,
        NativeMarkdownFontCascadeBudget::default(),
        mtm,
        &|| true,
    )
    .unwrap();
    for ordinal in 0..2 {
        let candidate = cascade.next(&base, &authority, &|| true).unwrap().unwrap();
        assert_eq!(candidate.ordinal(), ordinal);
        // SAFETY: 本控制只从原固定级联descriptor构造真实字体；不接收正文或外部矩阵。
        let native = unsafe {
            CTFont::with_font_descriptor_and_options(
                candidate.descriptor(),
                base.point_size(),
                std::ptr::null(),
                CTFontOptions::PreventAutoActivation | CTFontOptions::PreventAutoDownload,
            )
        };
        // SAFETY: 两个公开Copy getter只观察本控制持有的Native字体，结果不存进业务owner。
        let name = unsafe { native.attribute(kCTFontNameAttribute) };
        let name = name
            .as_deref()
            .and_then(|value| value.downcast_ref::<CFString>())
            .filter(|value| value.length() <= 1024)
            .map(|value| value.to_string());
        // SAFETY: 公开字体URL键与同一活CTFont；只输出有限来源元数据，不读字体正文。
        let url = unsafe { native.attribute(kCTFontURLAttribute) };
        let url = url
            .as_deref()
            .and_then(|value| value.downcast_ref::<CFURL>());
        let scheme = url.and_then(CFURL::scheme);
        let scheme = scheme
            .as_deref()
            .filter(|value| value.length() <= 64)
            .map(|value| value.to_string());
        println!(
            "paged_font_fallback candidate_source ordinal={ordinal} name={name:?} url_present={} url_scheme={scheme:?}",
            url.is_some()
        );
        let mut descriptor = std::ptr::null();
        // SAFETY: 原字体强claim贯穿Copy；out参数指向独立有效栈槽，非空结果接管其+1。
        let graphics = unsafe { native.graphics_font(&mut descriptor) };
        let descriptor = std::ptr::NonNull::new(descriptor.cast_mut()).map(|pointer| {
            // SAFETY: 指针刚由公开Copy out参数返回，尚未归还或交给另一owner。
            unsafe { CFRetained::<CTFontDescriptor>::from_raw(pointer) }
        });
        let graphics_name = CGFont::post_script_name(Some(&graphics));
        let graphics_name = graphics_name
            .as_deref()
            .filter(|value| value.length() <= 1024)
            .map(|value| value.to_string());
        let graphics_url = descriptor.as_deref().and_then(|descriptor| {
            // SAFETY: 本栈真实Copy descriptor仍活，公开URL键不触发正文或字体表复制。
            unsafe { descriptor.attribute(kCTFontURLAttribute) }
        });
        let graphics_url_present = graphics_url
            .as_deref()
            .and_then(|value| value.downcast_ref::<CFURL>())
            .is_some();
        println!(
            "paged_font_fallback candidate_graphics ordinal={ordinal} name={graphics_name:?} descriptor_present={} url_present={graphics_url_present}",
            descriptor.is_some()
        );
    } // 精确核实际失败项，不以名字列表推断某字体可捕获或可显示。
}
fn request(
    range: Range<usize>,
    script: [u8; 4],
    direction: MarkdownShapingDirection,
    source_len: usize,
) -> NativeMarkdownFontFallbackRequest {
    NativeMarkdownFontFallbackRequest {
        options: MarkdownShapingOptions {
            direction,
            script,
            language: None,
            points: 18.0,
            beginning_of_text: range.start == 0,
            end_of_text: range.end == source_len,
            features: Vec::new(),
        },
        source_utf16: range,
        languages: LANGUAGES,
        cascade_budget: NativeMarkdownFontCascadeBudget::default(),
        maximum_font_bytes: MAX_FONT_BYTES,
    }
}
fn stores_released(
    result: &omni_md_plugin::native_surface::paged_font_fallback::NativeMarkdownFontTrialResult,
) {
    let facts = result
        .resources()
        .expect("真实完成试次必须交出已Drop的字形存储收据");
    assert_eq!(facts.files_open, 0);
    assert_eq!(facts.mappings_open, 0);
    assert_eq!(facts.mapped_bytes, 0);
    assert!(!facts.cleanup_failed && facts.first_error.is_none());
    assert!(facts.shaper_access_certified);
}

fn selected_case(
    text: &str,
    script: [u8; 4],
    direction: MarkdownShapingDirection,
    expect_base: bool,
    mtm: MainThreadMarker,
) {
    let mut fixture = Fixture::new(text);
    let base = base(mtm);
    let font_authority = base.core_font().authority();
    let range = 0..fixture.source.utf16_len();
    let mut fallback = NativeMarkdownFontFallback::new(
        fixture.source.clone(),
        &fixture.authority,
        &base,
        &font_authority,
        request(range.clone(), script, direction, fixture.source.utf16_len()),
        mtm,
    )
    .unwrap();
    assert!(matches!(
        fallback
            .next_trial(
                &base,
                &font_authority,
                &mut fixture.graphemes,
                &mut fixture.scripts,
                &|| true
            )
            .unwrap(),
        NativeMarkdownFontFallbackStep::PendingDependencies
    ));
    fixture.prepare();
    let mut rejected = 0usize;
    let mut unavailable = 0usize;
    let mut expected_ordinal = 0usize;
    for _ in 0..257 {
        let trial = match fallback
            .next_trial(
                &base,
                &font_authority,
                &mut fixture.graphemes,
                &mut fixture.scripts,
                &|| true,
            )
            .unwrap()
        {
            NativeMarkdownFontFallbackStep::Trial(trial) => trial,
            NativeMarkdownFontFallbackStep::UnavailableCandidate { ordinal, cause } => {
                assert_eq!(ordinal, expected_ordinal);
                expected_ordinal += 1;
                unavailable += 1;
                println!(
                    "paged_font_fallback unavailable=Cascade({ordinal}) cause={cause:?} shaped=false missing_fact=false"
                );
                continue; // 本控制明确采用首个可捕获且真实shape无缺字策略；不等同首个系统字体。
            }
            NativeMarkdownFontFallbackStep::PendingDependencies => panic!("实际索引已完成"),
            NativeMarkdownFontFallbackStep::Exhausted { tried } => {
                panic!("真实候选全部缺字，不能伪称选择成功: tried={tried}")
            }
        };
        let result = std::thread::spawn(move || trial.run())
            .join()
            .expect("实际Font worker不得panic");
        stores_released(&result);
        assert!(
            result.outcome().is_ok(),
            "真实trial错误: {:?}",
            result.outcome()
        );
        match fallback
            .complete_trial(
                &base,
                &font_authority,
                &mut fixture.graphemes,
                &mut fixture.scripts,
                result,
                &|| true,
            )
            .unwrap()
        {
            NativeMarkdownFontFallbackCompletion::Rejected { origin, facts } => {
                assert!(facts.missing_glyphs > 0);
                println!(
                    "paged_font_fallback rejected={origin:?} glyphs={} missing={} source_utf16={}",
                    facts.glyph_count,
                    facts.missing_glyphs,
                    fixture.source.utf16_len()
                ); // 每次拒绝必须来自真实shape；候选捕获错误由独立typed错误保留，不跳过。
                if rejected == 0 {
                    assert_eq!(origin, NativeMarkdownFontOrigin::Base);
                } else {
                    assert_eq!(origin, NativeMarkdownFontOrigin::Cascade(expected_ordinal));
                    expected_ordinal += 1;
                }
                rejected += 1;
            }
            NativeMarkdownFontFallbackCompletion::Selected(choice) => {
                assert_eq!(choice.source_range(), range);
                assert_eq!(choice.facts().missing_glyphs, 0);
                assert_eq!(choice.facts().input_scalars, text.chars().count());
                assert!(choice.facts().source_borrow_max_bytes <= 4096);
                assert_eq!(
                    choice.origin() == NativeMarkdownFontOrigin::Base,
                    expect_base
                );
                if !expect_base {
                    assert_eq!(
                        choice.origin(),
                        NativeMarkdownFontOrigin::Cascade(expected_ordinal)
                    );
                }
                let chosen = choice.font(&base, &fixture.source).unwrap();
                let bytes = chosen.bytes_observer();
                assert_eq!(
                    chosen.source_face_postscript(),
                    chosen.core_font().metadata().postscript_name
                );
                let origin = choice.origin();
                let count = choice.facts().glyph_count;
                drop(choice);
                if !expect_base {
                    assert!(bytes.mapping_drop_completed() && bytes.unmap_succeeded());
                    assert_eq!(bytes.mapped_bytes(), 0);
                    assert!(
                        bytes.source_descriptor_closed()
                            && bytes.backing_descriptor_closed()
                            && !bytes.failed()
                    );
                } // Base仍由原caller独立持有；只能在fallback独立Font最后owner归还后核unmap。
                println!(
                    "paged_font_fallback selected={origin:?} rejected={rejected} unavailable={unavailable} glyphs={count} input_scalars={} missing=0 source_utf16={} glyph_store_released=true full_string=false policy=first_capturable_shaped_no_missing",
                    text.chars().count(),
                    fixture.source.utf16_len()
                );
                return;
            }
        }
    }
    panic!("候选次数超过原显式预算");
}

fn invalid_cluster_and_script_are_rejected(mtm: MainThreadMarker) {
    let base = base(mtm);
    let authority = base.core_font().authority();
    for (text, script) in [("ab", *b"Latn"), ("中", *b"Latn")] {
        let mut fixture = Fixture::new(text);
        fixture.prepare();
        let mut fallback = NativeMarkdownFontFallback::new(
            fixture.source.clone(),
            &fixture.authority,
            &base,
            &authority,
            request(
                0..fixture.source.utf16_len(),
                script,
                MarkdownShapingDirection::LeftToRight,
                fixture.source.utf16_len(),
            ),
            mtm,
        )
        .unwrap();
        assert!(matches!(
            fallback.next_trial(
                &base,
                &authority,
                &mut fixture.graphemes,
                &mut fixture.scripts,
                &|| true
            ),
            Err(NativeMarkdownFontFallbackError::InvalidCluster)
        ));
        assert!(
            fallback
                .next_trial(
                    &base,
                    &authority,
                    &mut fixture.graphemes,
                    &mut fixture.scripts,
                    &|| true
                )
                .is_err()
        );
    }
    println!(
        "paged_font_fallback invalid_multiple_gcb=true invalid_script=true permanently_closed=true"
    );
}

fn original_trial_ticket_and_close_cancellation_are_real(mtm: MainThreadMarker) {
    let base = base(mtm);
    let font_authority = base.core_font().authority();
    let mut fixture = Fixture::new("a");
    fixture.prepare();
    let mut first = NativeMarkdownFontFallback::new(
        fixture.source.clone(),
        &fixture.authority,
        &base,
        &font_authority,
        request(0..1, *b"Latn", MarkdownShapingDirection::LeftToRight, 1),
        mtm,
    )
    .unwrap();
    let mut second = NativeMarkdownFontFallback::new(
        fixture.source.clone(),
        &fixture.authority,
        &base,
        &font_authority,
        request(0..1, *b"Latn", MarkdownShapingDirection::LeftToRight, 1),
        mtm,
    )
    .unwrap();
    let NativeMarkdownFontFallbackStep::Trial(trial) = first
        .next_trial(
            &base,
            &font_authority,
            &mut fixture.graphemes,
            &mut fixture.scripts,
            &|| true,
        )
        .unwrap()
    else {
        panic!("真实试次未签发")
    };
    let NativeMarkdownFontFallbackStep::Trial(other) = second
        .next_trial(
            &base,
            &font_authority,
            &mut fixture.graphemes,
            &mut fixture.scripts,
            &|| true,
        )
        .unwrap()
    else {
        panic!("第二真实试次未签发")
    };
    let result = std::thread::spawn(move || trial.run()).join().unwrap();
    assert!(matches!(
        second.complete_trial(
            &base,
            &font_authority,
            &mut fixture.graphemes,
            &mut fixture.scripts,
            result,
            &|| true
        ),
        Err(NativeMarkdownFontFallbackError::TrialMismatch)
    ));
    drop(other);
    first.close();
    second.close();

    let mut closing = NativeMarkdownFontFallback::new(
        fixture.source.clone(),
        &fixture.authority,
        &base,
        &font_authority,
        request(0..1, *b"Latn", MarkdownShapingDirection::LeftToRight, 1),
        mtm,
    )
    .unwrap();
    let NativeMarkdownFontFallbackStep::Trial(trial) = closing
        .next_trial(
            &base,
            &font_authority,
            &mut fixture.graphemes,
            &mut fixture.scripts,
            &|| true,
        )
        .unwrap()
    else {
        panic!("关闭控制未签发真实packet")
    };
    let source = fixture.source.resource_observer();
    let mapping = fixture.document.original_backing_observer().unwrap();
    let gate = Arc::new(Barrier::new(2));
    let worker_gate = gate.clone();
    let worker = std::thread::spawn(move || {
        worker_gate.wait();
        trial.run()
    });
    closing.close();
    drop(fixture);
    assert!(!source.payload_released() && !mapping.unmap_succeeded());
    gate.wait();
    let result = worker.join().unwrap();
    assert!(matches!(
        result.outcome(),
        Err(NativeMarkdownFontFallbackError::Shaping(
            MarkdownShapingError::Cancelled
        ))
    ));
    assert!(source.payload_released() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
    println!(
        "paged_font_fallback foreign_trial_rejected=true close_requests_original_cancel=true joined=true actual_source_unmapped=true"
    );
}

fn main() {
    fn is_send<T: Send>() {}
    is_send::<NativeMarkdownFontTrial>(); // 编译器验证packet真实Send，不给ShapeJob或NativeFont加unsafe承诺。
    let mut arguments = std::env::args_os().skip(1);
    let isolated_emfile = match (arguments.next(), arguments.next()) {
        (None, None) => false,
        (Some(argument), None) if argument == "--trial-emfile" => true,
        _ => panic!("真实字体回退控制不接受未知参数"),
    };
    let mtm = MainThreadMarker::new().expect("Native候选构造只能在主线程");
    if isolated_emfile {
        autoreleasepool(|_| worker::verify_emfile(mtm));
        return; // 故障只在显式独立子进程执行，不夹在默认控制或正式内存负载之前。
    }
    autoreleasepool(|_| {
        #[cfg(feature = "native-paged-compose-probe")]
        composed::verify(mtm);
        request_boundaries::verify(mtm);
        worker::verify(mtm);
        inspect_second_candidate_source(mtm);
        selected_case(
            "a",
            *b"Latn",
            MarkdownShapingDirection::LeftToRight,
            true,
            mtm,
        );
        selected_case(
            "中",
            *b"Hani",
            MarkdownShapingDirection::LeftToRight,
            false,
            mtm,
        );
        selected_case(
            "بَ",
            *b"Arab",
            MarkdownShapingDirection::RightToLeft,
            false,
            mtm,
        );
        selected_case(
            "👩🏽‍💻",
            *b"Zyyy",
            MarkdownShapingDirection::LeftToRight,
            false,
            mtm,
        );
        let giant = format!("a{}", "\u{0301}".repeat(16_384)); // 仅有限控制输入，实际整形packet不保全文String。
        selected_case(
            &giant,
            *b"Latn",
            MarkdownShapingDirection::LeftToRight,
            true,
            mtm,
        );
        invalid_cluster_and_script_are_rejected(mtm);
        original_trial_ticket_and_close_cancellation_are_real(mtm);
    });
    println!(
        "paged_font_fallback_control selected_cases=5 invalid_cases=2 close_cases=1 creates_application=false final_run_ready=false softline_ready=false formal_memory=false"
    );
}
