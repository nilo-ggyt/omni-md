//! 主线程真实字体级联控制；无NSApplication、窗口、正文或缺字选择器。
use std::cell::Cell;

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFArray, CFRetained, CFString, CFType};
use objc2_core_text::{CTFont, CTFontDescriptor};
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::MarkdownLayoutFontBytesResourceObserver;
use omni_md_plugin::native_surface::{
    paged_font_cascade::{
        NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES, NativeMarkdownFontCascade,
        NativeMarkdownFontCascadeBudget, NativeMarkdownFontCascadeError,
    },
    paged_fonts::NativeMarkdownPagedFont,
};

const LANGUAGES: &[&str] = &["zh-Hans", "en", "ar"];
const MAX_FONT_BYTES: u64 = 512 * 1024 * 1024;

fn base(mtm: MainThreadMarker) -> NativeMarkdownPagedFont {
    let name = CFString::from_str("Helvetica");
    // SAFETY: 控制使用固定名称与有限正字号；没有正文或外部矩阵指针。
    let font = unsafe { CTFont::with_name(&name, 18.0, std::ptr::null()) };
    NativeMarkdownPagedFont::capture(&font, MAX_FONT_BYTES, mtm, &|| true)
        .expect("固定Helvetica必须实际捕获同源Native/Core字体")
}
fn released(observer: &MarkdownLayoutFontBytesResourceObserver) {
    assert!(observer.mapping_drop_completed());
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.source_descriptor_closed() && observer.backing_descriptor_closed());
    assert!(!observer.failed());
    assert_eq!(observer.first_errno(), None);
}
fn oracle_list(base: &NativeMarkdownPagedFont) -> CFRetained<CFArray> {
    let languages: Vec<_> = LANGUAGES
        .iter()
        .map(|value| CFString::from_str(value))
        .collect();
    let languages = CFArray::from_retained_objects(&languages);
    // SAFETY: 独立公开oracle与产品捕获使用同一个活base和固定语言列表，不创建文本查询。
    unsafe {
        base.native_font()
            .default_cascade_list_for_languages(Some(languages.as_opaque()))
    }
    .expect("当前正式机器必须提供真实系统回退列表")
}
fn oracle_descriptor(list: &CFArray, ordinal: usize) -> &CTFontDescriptor {
    assert!(ordinal < list.len());
    // SAFETY: 不可变Copy列表贯穿借用，ordinal刚核在范围内，公开约定值为CFType。
    let value = unsafe { list.value_at_index(ordinal as _) };
    assert!(!value.is_null());
    // SAFETY: 原列表保活该非空CF对象；先动态核type再作为descriptor使用。
    unsafe { &*value.cast::<CFType>() }
        .downcast_ref::<CTFontDescriptor>()
        .expect("系统列表真实元素类型")
}

fn ordered_candidates_and_exact_capture(mtm: MainThreadMarker) {
    let base = base(mtm);
    let base_bytes = base.bytes_observer();
    let authority = base.core_font().authority();
    let expected = oracle_list(&base);
    let mut cascade = NativeMarkdownFontCascade::capture(
        &base,
        &authority,
        LANGUAGES,
        NativeMarkdownFontCascadeBudget::default(),
        mtm,
        &|| true,
    )
    .unwrap();
    assert_eq!(cascade.candidate_count(), expected.len());
    assert!((2..=NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES).contains(&expected.len()));
    let mut first = None;
    for ordinal in 0..expected.len() {
        let candidate = cascade.next(&base, &authority, &|| true).unwrap().unwrap();
        assert_eq!(candidate.ordinal(), ordinal);
        assert_eq!(
            candidate.descriptor(),
            oracle_descriptor(&expected, ordinal)
        );
        if ordinal == 0 {
            first = Some(candidate);
        } // 只保第一项供一次真实Font捕获；其他描述符按遍历立即归还，没有候选Font数组。
    }
    assert!(cascade.next(&base, &authority, &|| true).unwrap().is_none());
    let first = first.unwrap();
    let chosen = first
        .capture(&base, &authority, MAX_FONT_BYTES, &|| true)
        .unwrap();
    let chosen_bytes = chosen.bytes_observer();
    let metadata = chosen.core_font().metadata();
    assert!(metadata.glyph_count > 0 && metadata.face_index < metadata.face_count);
    assert_eq!(chosen.point_size(), base.point_size());
    assert_eq!(chosen.source_face_postscript(), metadata.postscript_name);
    let chosen_core = chosen.core_font().clone();
    let chosen_payload = chosen_core.resource_observer();
    let count = cascade.candidate_count();
    cascade.close();
    let after_close_calls = Cell::new(0usize);
    assert!(matches!(
        first.capture(&base, &authority, MAX_FONT_BYTES, &|| {
            after_close_calls.set(after_close_calls.get() + 1);
            true
        }),
        Err(NativeMarkdownFontCascadeError::Revoked)
    ));
    assert_eq!(after_close_calls.get(), 0); // 原Weak签发者已死，不能先调用Native再事后发现关闭。
    assert_eq!(first.descriptor(), oracle_descriptor(&expected, 0)); // 旧不可变descriptor没有被强制清空。
    drop(base);
    released(&base_bytes); // 活candidate/列表oracle只保Native元数据，不保base的Core snapshot。
    drop(chosen);
    assert!(!chosen_payload.payload_released() && !chosen_bytes.mapping_drop_completed());
    drop(chosen_core);
    assert!(chosen_payload.payload_released());
    released(&chosen_bytes); // 真正最后Font归还后核内核munmap与两个FD，不把cohort步骤当收据。
    drop(first);
    drop(expected);
    println!(
        "paged_font_cascade case=ordered_capture candidates={count} full_order_equal=true exact_source=true actual_snapshot_unmapped=true shaping_fallback_verified=false"
    );
}

fn budget_and_language_failures_are_explicit(mtm: MainThreadMarker) {
    let base = base(mtm);
    let authority = base.core_font().authority();
    let expected = oracle_list(&base).len();
    assert!(expected > 1); // 当前正式机器上的固定Helvetica正面对照，不能伪造一个被截断的系统列表。
    let visits = Cell::new(0usize);
    let current = || {
        visits.set(visits.get() + 1);
        true
    };
    for maximum_candidates in [0, NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES + 1] {
        assert!(matches!(
            NativeMarkdownFontCascade::capture(
                &base,
                &authority,
                LANGUAGES,
                NativeMarkdownFontCascadeBudget { maximum_candidates },
                mtm,
                &current
            ),
            Err(NativeMarkdownFontCascadeError::InvalidBudget)
        ));
    }
    for languages in [
        &["不是语言标签"][..],
        &["en\0ar"][..],
        &["en"; 9][..],
        &[""][..],
    ] {
        assert!(matches!(
            NativeMarkdownFontCascade::capture(
                &base,
                &authority,
                languages,
                NativeMarkdownFontCascadeBudget::default(),
                mtm,
                &current
            ),
            Err(NativeMarkdownFontCascadeError::InvalidLanguages)
        ));
    }
    assert_eq!(visits.get(), 0); // 纯参数失败在任何CF构造和外部权限回调之前拒绝。
    assert!(matches!(
        NativeMarkdownFontCascade::capture(
            &base,
            &authority,
            LANGUAGES,
            NativeMarkdownFontCascadeBudget { maximum_candidates: 1 },
            mtm,
            &|| true
        ),
        Err(NativeMarkdownFontCascadeError::CandidateBudgetExceeded { actual, maximum: 1 }) if actual == expected
    ));
    println!(
        "paged_font_cascade case=budget invalid_parameters_rejected=true actual_system_overflow_rejected=true"
    );
}

fn foreign_same_named_payload_never_acquires_original_authority(mtm: MainThreadMarker) {
    let first = base(mtm);
    let second = base(mtm);
    assert_eq!(first.core_font().metadata(), second.core_font().metadata());
    let first_authority = first.core_font().authority();
    let second_authority = second.core_font().authority();
    assert!(!first.core_font().permits(&second_authority));
    assert!(!second.core_font().permits(&first_authority));
    assert!(matches!(
        NativeMarkdownFontCascade::capture(
            &first,
            &second_authority,
            LANGUAGES,
            NativeMarkdownFontCascadeBudget::default(),
            mtm,
            &|| panic!("外来payload不能进入Native票")
        ),
        Err(NativeMarkdownFontCascadeError::FontAuthorityMismatch)
    ));
    let mut cascade = NativeMarkdownFontCascade::capture(
        &first,
        &first_authority,
        LANGUAGES,
        NativeMarkdownFontCascadeBudget::default(),
        mtm,
        &|| true,
    )
    .unwrap();
    let candidate = cascade
        .next(&first, &first_authority, &|| true)
        .unwrap()
        .unwrap();
    assert!(matches!(
        candidate.capture(&second, &second_authority, MAX_FONT_BYTES, &|| panic!(
            "不能重签同名Font"
        )),
        Err(NativeMarkdownFontCascadeError::FontAuthorityMismatch)
    ));
    assert!(matches!(
        cascade.next(&first, &first_authority, &|| true),
        Err(NativeMarkdownFontCascadeError::Revoked)
    )); // 错误authority已经永久撤权，不能下一次传回正确值就恢复原游标。
    println!(
        "paged_font_cascade case=identity same_name_distinct_payload_rejected=true sticky_revocation=true"
    );
}

fn revoked_primitives_and_closed_candidates_never_retry(mtm: MainThreadMarker) {
    let base = base(mtm);
    let authority = base.core_font().authority();
    let calls = Cell::new(0usize);
    assert!(matches!(
        NativeMarkdownFontCascade::capture(
            &base,
            &authority,
            LANGUAGES,
            NativeMarkdownFontCascadeBudget::default(),
            mtm,
            &|| {
                calls.set(calls.get() + 1);
                calls.get() < 3
            }
        ),
        Err(NativeMarkdownFontCascadeError::Revoked)
    )); // 语言CF构造前后实际撤权，已经取得的临时+1不能泄漏到成功owner。
    let mut cascade = NativeMarkdownFontCascade::capture(
        &base,
        &authority,
        LANGUAGES,
        NativeMarkdownFontCascadeBudget::default(),
        mtm,
        &|| true,
    )
    .unwrap();
    let candidate = cascade.next(&base, &authority, &|| true).unwrap().unwrap();
    calls.set(0);
    assert!(matches!(
        candidate.capture(&base, &authority, MAX_FONT_BYTES, &|| {
            calls.set(calls.get() + 1);
            calls.get() < 4
        }),
        Err(NativeMarkdownFontCascadeError::Revoked)
    )); // 实际best-match构造完成后，在原捕获入口撤权，临时Native字体仍真实归还。
    assert!(matches!(
        cascade.next(&base, &authority, &|| true),
        Err(NativeMarkdownFontCascadeError::Revoked)
    ));
    println!(
        "paged_font_cascade case=revoke constructor_mid_revoked=true candidate_mid_revoked=true no_implicit_retry=true"
    );
}

fn main() {
    assert!(
        std::env::args_os().nth(1).is_none(),
        "字体候选控制不接受未知参数"
    );
    let mtm = MainThreadMarker::new().expect("真实Native字体控制必须运行于主线程");
    autoreleasepool(|_| {
        ordered_candidates_and_exact_capture(mtm);
        budget_and_language_failures_are_explicit(mtm);
        foreign_same_named_payload_never_acquires_original_authority(mtm);
        revoked_primitives_and_closed_candidates_never_retry(mtm);
    });
    println!(
        "paged_font_cascade_control cases=4 body_input=false creates_application=false glyph_coverage_verdict=false missing_glyph_fallback_verified=false formal_memory=false"
    );
}
