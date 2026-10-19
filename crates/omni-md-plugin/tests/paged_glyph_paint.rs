//! 真实Core字形到有限CPU位图的控制；不创建NSApplication、文字段落或窗口。

use std::{cell::Cell, io::Write};

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFRange, CFRetained, CFString, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    kCGColorSpaceSRGB,
};
use objc2_core_text::CTFont;
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownLayoutFont, MarkdownLayoutFontAuthority,
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource, MarkdownShapedSpool,
    MarkdownShapedSpoolBuilder, MarkdownShapedSpoolError, MarkdownShapingBudget,
    MarkdownShapingCancellation, MarkdownShapingDirection, MarkdownShapingJob,
    MarkdownShapingOptions, MarkdownShapingResourceObserver, MarkdownShapingStatus,
};
use omni_md_plugin::native_surface::{
    paged_fonts::NativeMarkdownPagedFont,
    paged_glyph_paint::{
        NativeMarkdownPagedGlyphOrigin, NativeMarkdownPagedGlyphPaintCause,
        NativeMarkdownPagedGlyphPaintRequest, paint_paged_glyphs, paint_paged_spool_glyphs,
    },
};

const WIDTH: usize = 640;
const HEIGHT: usize = 160;
const PIXEL_BYTES: usize = WIDTH * HEIGHT * 4;
const MAX_FONT_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(feature = "native-paged-compose-probe")]
#[path = "paged_glyph_paint/composed_lines.rs"]
mod composed_lines;
#[cfg(feature = "native-paged-compose-probe")]
#[path = "paged_glyph_paint/control_tabs.rs"]
mod control_tabs;
#[cfg(feature = "native-markdown-runtime-probe")]
#[path = "paged_glyph_paint/system_fonts.rs"]
mod system_fonts;

struct Canvas {
    context: CFRetained<CGContext>, // 先释放借用像素的context，再Drop后面的实际Vec。
    pixels: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        let mut pixels = vec![255u8; PIXEL_BYTES];
        // SAFETY: 框架导出的固定sRGB名称在进程期有效，没有读取任何正文或用户路径。
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        // SAFETY: 固定RGBA8数组有完整row_bytes*height，移动Vec不移动allocation；context字段先析构。
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                WIDTH,
                HEIGHT,
                8,
                WIDTH * 4,
                Some(&space),
                CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .expect("有限CPUcontext");
        CGContext::set_rgb_fill_color(Some(&context), 0.0, 0.0, 0.0, 1.0);
        Self { context, pixels }
    }

    fn pixel_facts(&mut self) -> (usize, usize) {
        CGContext::flush(Some(&self.context));
        let mut ink = 0;
        let mut colored = 0;
        // PIXEL_BYTES固定为RGBA槽的整倍数，不存在可丢弃的尾字节。
        for pixel in self.pixels.as_chunks::<4>().0 {
            let low = pixel[0].min(pixel[1]).min(pixel[2]);
            let high = pixel[0].max(pixel[1]).max(pixel[2]);
            ink += usize::from(low < 250);
            colored += usize::from(high - low > 16);
        }
        (ink, colored)
    }
}

fn native_font(base: &CTFont, finite_case: &str, mtm: MainThreadMarker) -> NativeMarkdownPagedFont {
    assert!(finite_case.chars().count() <= 64); // 控制case全部传入，不把这一上限当生产grapheme截断。
    let value = CFString::from_str(finite_case);
    // SAFETY: CFString和UTF16 range为本次完整有限字体回退case，返回+1原生字体。
    let native = unsafe {
        base.for_string(
            &value,
            CFRange::new(0, finite_case.encode_utf16().count() as _),
        )
    };
    NativeMarkdownPagedFont::capture(&native, MAX_FONT_BYTES, mtm, &|| true).unwrap()
}

fn fixture(text: &str) -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(text.as_bytes()).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 4096).unwrap();
    (file, document)
}

fn new_job(
    source: &MarkdownLayoutTextSource,
    font: &NativeMarkdownPagedFont,
    direction: MarkdownShapingDirection,
    script: [u8; 4],
    points: f32,
) -> (
    MarkdownShapingJob,
    MarkdownLayoutTextAuthority,
    MarkdownLayoutFontAuthority,
) {
    let source_authority = source.authority();
    let font_authority = font.core_font().authority();
    let job = MarkdownShapingJob::new(
        source.clone(),
        &source_authority,
        font.core_font().clone(),
        &font_authority,
        0..source.utf16_len(),
        MarkdownShapingOptions {
            direction,
            script,
            language: None,
            points,
            beginning_of_text: true,
            end_of_text: true,
            features: Vec::new(),
        },
    )
    .unwrap();
    (job, source_authority, font_authority)
}

fn ready(
    job: &mut MarkdownShapingJob,
    source: &MarkdownLayoutTextAuthority,
    font: &MarkdownLayoutFontAuthority,
) {
    while job.status() == MarkdownShapingStatus::Ingesting {
        job.step(
            source,
            font,
            MarkdownShapingBudget {
                bytes: 3,
                scalars: 1,
            },
        )
        .unwrap();
    }
    assert_eq!(
        job.shape(source, font).unwrap(),
        MarkdownShapingStatus::Ready
    );
}

fn origin() -> NativeMarkdownPagedGlyphOrigin {
    NativeMarkdownPagedGlyphOrigin {
        baseline: CGPoint { x: 16.0, y: 64.0 },
        pen_x_26_6: 0,
        pen_y_26_6: 0,
    }
}

fn case(
    base: &CTFont,
    mtm: MainThreadMarker,
    text: &str,
    script: [u8; 4],
    rtl: bool,
    color: bool,
    label: &str,
) {
    let font = native_font(base, text, mtm);
    let font_bytes = font.bytes_observer();
    let font_payload = font.core_font().resource_observer();
    let (_file, document) = fixture(text);
    let original = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_payload = source.resource_observer();
    let (mut job, source_authority, font_authority) = new_job(
        &source,
        &font,
        if rtl {
            MarkdownShapingDirection::RightToLeft
        } else {
            MarkdownShapingDirection::LeftToRight
        },
        script,
        32.0,
    );
    ready(&mut job, &source_authority, &font_authority);
    let count = job.profile().unwrap().glyph_count;
    assert!((1..=256).contains(&count));
    let resources = job.resource_observer();
    let mut canvas = Canvas::new();
    let receipt = paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &source_authority,
            font_authority: &font_authority,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &canvas.context,
        &|| true,
    )
    .unwrap();
    assert_eq!(receipt.glyph_range, 0..count);
    assert_eq!(receipt.source_utf16_range, 0..source.utf16_len());
    assert_eq!(
        receipt.missing_glyphs, 0,
        "真实case出现.notdef不能宣称字形覆盖"
    );
    let (ink, colored) = canvas.pixel_facts();
    assert!(ink > 0, "真实CTFontDrawGlyphs没有产生墨迹");
    if color {
        assert!(colored > 0, "彩色emoji不得按单色占位通过");
    }
    println!(
        "native_paged_glyph_paint case={label} glyphs={count} ink={ink} colored={colored} same_ready_job=true actual_ctfont_draw=true formal=false"
    );
    drop(document);
    drop(source);
    drop(font);
    assert!(!original.unmap_succeeded()); // 同一job仍合法持原source/font，不能提前报资源归还。
    job.cancel();
    drop(job);
    assert!(source_payload.payload_released() && font_payload.payload_released());
    assert!(resources.resources_released());
    assert!(original.unmap_succeeded() && original.mapped_bytes() == 0 && !original.failed());
    assert!(font_bytes.unmap_succeeded() && font_bytes.mapped_bytes() == 0 && !font_bytes.failed());
}

fn rejection_and_clip_controls(base: &CTFont, mtm: MainThreadMarker) {
    let font = native_font(base, "Ab", mtm);
    let (_file, document) = fixture("Ab office");
    let source = MarkdownLayoutTextSource::capture(&document);
    let (mut job, sa, fa) = new_job(
        &source,
        &font,
        MarkdownShapingDirection::LeftToRight,
        *b"Latn",
        32.0,
    );
    let mut canvas = Canvas::new();
    // 超限先于Ready/profile/glyph；原输入阶段不能被这个拒绝推进或撤权。
    let before = job.status();
    let oversized = paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 0..257,
            origin: origin(),
        },
        &canvas.context,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        oversized.cause,
        NativeMarkdownPagedGlyphPaintCause::BatchLimit
    ));
    assert!(!oversized.native_draw_returned && job.status() == before);
    ready(&mut job, &sa, &fa);
    let count = job.profile().unwrap().glyph_count;
    let denied = paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &canvas.context,
        &|| false,
    )
    .unwrap_err();
    assert!(matches!(
        denied.cause,
        NativeMarkdownPagedGlyphPaintCause::Revoked
    ));
    assert!(!denied.native_draw_returned && canvas.pixel_facts().0 == 0);
    let invalid_origin = NativeMarkdownPagedGlyphOrigin {
        pen_x_26_6: i64::MIN,
        ..origin()
    };
    let invalid = paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 0..count,
            origin: invalid_origin,
        },
        &canvas.context,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        invalid.cause,
        NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate
    ));
    assert!(!invalid.native_draw_returned && canvas.pixel_facts().0 == 0);
    let clip = CGRect {
        origin: CGPoint { x: 48.0, y: 0.0 },
        size: CGSize {
            width: 64.0,
            height: HEIGHT as f64,
        },
    };
    CGContext::clip_to_rect(Some(&canvas.context), clip);
    let prior_matrix = CGContext::text_matrix(Some(&canvas.context));
    let prior_position = CGContext::text_position(Some(&canvas.context));
    paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &canvas.context,
        &|| true,
    )
    .unwrap();
    assert_eq!(CGContext::text_matrix(Some(&canvas.context)), prior_matrix);
    assert_eq!(
        CGContext::text_position(Some(&canvas.context)),
        prior_position
    );
    assert!(canvas.pixel_facts().0 > 0);
    for (index, pixel) in canvas.pixels.as_chunks::<4>().0.iter().enumerate() {
        if !(48..112).contains(&(index % WIDTH)) {
            assert_eq!(*pixel, [255, 255, 255, 255]);
        }
    }
    // 此current只在Native同步返回间隙检查有限CPU像素，制造真实draw后撤权的负面对照。
    let mut discard_canvas = Canvas::new();
    let address = discard_canvas.pixels.as_ptr();
    let revocations = Cell::new(0);
    let current = || {
        CGContext::flush(Some(&discard_canvas.context));
        // SAFETY: 固定Vec全程不移动或重分配，当前线程没有并行Native写入；借用在回调返回前结束。
        let pixels = unsafe { std::slice::from_raw_parts(address, PIXEL_BYTES) };
        let clear = pixels.iter().all(|byte| *byte == 255);
        if !clear {
            revocations.set(revocations.get() + 1);
        }
        clear
    };
    let discarded = paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &discard_canvas.context,
        &current,
    )
    .unwrap_err();
    assert!(discarded.native_draw_returned && revocations.get() > 0);
    assert!(matches!(
        discarded.cause,
        NativeMarkdownPagedGlyphPaintCause::Revoked
    ));
    assert!(discard_canvas.pixel_facts().0 > 0); // 已绘候选必须discard，不能把Err解释为目标未修改。

    let (_other_file, other_document) = fixture("Ab office");
    let other_source = MarkdownLayoutTextSource::capture(&other_document);
    let foreign = other_source.authority();
    let (mut wrong_source_job, _, original_font) = new_job(
        &source,
        &font,
        MarkdownShapingDirection::LeftToRight,
        *b"Latn",
        32.0,
    );
    ready(&mut wrong_source_job, &sa, &original_font);
    let count = wrong_source_job.profile().unwrap().glyph_count;
    let mut untouched = Canvas::new();
    let wrong_source = paint_paged_glyphs(
        &mut wrong_source_job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &foreign,
            font_authority: &original_font,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &untouched.context,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        wrong_source.cause,
        NativeMarkdownPagedGlyphPaintCause::Shaping(_)
    ));
    assert!(!wrong_source.native_draw_returned && untouched.pixel_facts().0 == 0);
    let second_font = native_font(base, "Ab", mtm);
    let (mut wrong_font_job, sa2, fa2) = new_job(
        &source,
        &font,
        MarkdownShapingDirection::LeftToRight,
        *b"Latn",
        32.0,
    );
    ready(&mut wrong_font_job, &sa2, &fa2);
    let count = wrong_font_job.profile().unwrap().glyph_count;
    let wrong_font = paint_paged_glyphs(
        &mut wrong_font_job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa2,
            font_authority: &fa2,
            native_font: &second_font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &untouched.context,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        wrong_font.cause,
        NativeMarkdownPagedGlyphPaintCause::Shaping(_)
    ));
    assert!(!wrong_font.native_draw_returned && untouched.pixel_facts().0 == 0);
    println!(
        "native_paged_glyph_paint_negative oversized=true revoked=true invalid_coordinate=true exact_source=true exact_font=true clip=true actual_draw_then_discard=true formal=false"
    );
}

fn two_exact_batches(base: &CTFont, mtm: MainThreadMarker) {
    let font = native_font(base, "A", mtm);
    let (_file, document) = fixture(&"A".repeat(300)); // 此有限控制故意跨256边界，不是生产正文副本。
    let source = MarkdownLayoutTextSource::capture(&document);
    let (mut job, sa, fa) = new_job(
        &source,
        &font,
        MarkdownShapingDirection::LeftToRight,
        *b"Latn",
        32.0,
    );
    ready(&mut job, &sa, &fa);
    assert_eq!(job.profile().unwrap().glyph_count, 300);
    let mut canvas = Canvas::new();
    let too_large = paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 0..300,
            origin: origin(),
        },
        &canvas.context,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        too_large.cause,
        NativeMarkdownPagedGlyphPaintCause::BatchLimit
    ));
    assert!(!too_large.native_draw_returned && canvas.pixel_facts().0 == 0);
    let first = paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 0..256,
            origin: origin(),
        },
        &canvas.context,
        &|| true,
    )
    .unwrap();
    let following_origin = NativeMarkdownPagedGlyphOrigin {
        pen_x_26_6: first.final_pen_x_26_6,
        pen_y_26_6: first.final_pen_y_26_6,
        ..origin()
    };
    let second = paint_paged_glyphs(
        &mut job,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 256..300,
            origin: following_origin,
        },
        &canvas.context,
        &|| true,
    )
    .unwrap();
    assert_eq!(first.glyph_range, 0..256);
    assert_eq!(second.glyph_range, 256..300);
    let (mut expected_x, mut expected_y) = (0i64, 0i64);
    for index in 0..300 {
        let glyph = job.glyph(&sa, &fa, index).unwrap().unwrap();
        expected_x = expected_x.checked_add(i64::from(glyph.x_advance)).unwrap();
        expected_y = expected_y.checked_add(i64::from(glyph.y_advance)).unwrap();
    }
    assert_eq!(
        (second.final_pen_x_26_6, second.final_pen_y_26_6),
        (expected_x, expected_y)
    );
    assert!(canvas.pixel_facts().0 > 0);
    println!(
        "native_paged_glyph_paint_batches first=256 second=44 actual_total=300 exact_advance=true oversized_rejected=true formal=false"
    );
}

// 只移交sealed文件与纯票/标量收据；Job始终在worker构造、使用并实际Drop。
struct SealedHandoff {
    spool: MarkdownShapedSpool,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    cancellation: MarkdownShapingCancellation,
    job_resources: MarkdownShapingResourceObserver,
}

fn worker_spool(
    source: MarkdownLayoutTextSource,
    font: MarkdownLayoutFont,
    direction: MarkdownShapingDirection,
    script: [u8; 4],
) -> SealedHandoff {
    std::thread::spawn(move || {
        let sa = source.authority();
        let fa = font.authority();
        let mut job = MarkdownShapingJob::new(
            source.clone(),
            &sa,
            font.clone(),
            &fa,
            0..source.utf16_len(),
            MarkdownShapingOptions {
                direction,
                script,
                language: None,
                points: 32.0,
                beginning_of_text: true,
                end_of_text: true,
                features: Vec::new(),
            },
        )
        .unwrap();
        ready(&mut job, &sa, &fa);
        let count = job.profile().unwrap().glyph_count;
        let cancellation = job.cancellation();
        let job_resources = job.resource_observer();
        let mut builder = MarkdownShapedSpoolBuilder::new(&mut job, &sa, &fa).unwrap();
        let mut consumed = 0;
        while consumed < count {
            let step = builder.step(&mut job, 256).unwrap();
            assert!((1..=256).contains(&step.glyphs));
            consumed += step.glyphs;
        }
        let spool = builder.seal(&mut job).unwrap();
        drop(job);
        drop(source);
        drop(font);
        // join之前真实归还worker可变shaper的文件和映射；Spool合法保留原Source/Font。
        let released = job_resources.snapshot();
        assert_eq!((released.files_open, released.mappings_open), (0, 0));
        SealedHandoff {
            spool,
            source: sa,
            font: fa,
            cancellation,
            job_resources,
        }
    })
    .join()
    .expect("原Job不能跨线程移交，worker必须正常完成seal与Drop")
}

fn spool_case(
    base: &CTFont,
    mtm: MainThreadMarker,
    text: &str,
    script: [u8; 4],
    rtl: bool,
    color: bool,
    label: &str,
) {
    let font = native_font(base, text, mtm);
    let font_bytes = font.bytes_observer();
    let font_payload = font.core_font().resource_observer();
    let (_file, document) = fixture(text);
    let original = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_payload = source.resource_observer();
    let direction = if rtl {
        MarkdownShapingDirection::RightToLeft
    } else {
        MarkdownShapingDirection::LeftToRight
    };
    let mut handoff = worker_spool(source.clone(), font.core_font().clone(), direction, script);
    let output = handoff.spool.authority();
    let resources = handoff.spool.resource_observer();
    let count = handoff.spool.glyph_count();
    assert!((1..=256).contains(&count));
    let mut canvas = Canvas::new();
    let prior_matrix = CGContext::text_matrix(Some(&canvas.context));
    let prior_position = CGContext::text_position(Some(&canvas.context));
    let receipt = paint_paged_spool_glyphs(
        &mut handoff.spool,
        &output,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &handoff.source,
            font_authority: &handoff.font,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &canvas.context,
        &|| true,
    )
    .unwrap();
    assert_eq!(receipt.source_utf16_range, 0..source.utf16_len());
    assert_eq!(receipt.missing_glyphs, 0);
    assert_eq!(CGContext::text_matrix(Some(&canvas.context)), prior_matrix);
    assert_eq!(
        CGContext::text_position(Some(&canvas.context)),
        prior_position
    );
    let (ink, colored) = canvas.pixel_facts();
    assert!(ink > 0, "sealed交付必须真实绘出字形，不能仅凭记录数通过");
    if color {
        assert!(colored > 0, "sealed交付也必须保留实际彩色emoji");
    }
    // 同一不可变Source/Font再建有限旧路线oracle，逐像素与完整receipt事实核对；不复用nonce。
    let (mut oracle, sa, fa) = new_job(&source, &font, direction, script, 32.0);
    ready(&mut oracle, &sa, &fa);
    let mut reference = Canvas::new();
    let expected = paint_paged_glyphs(
        &mut oracle,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &sa,
            font_authority: &fa,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &reference.context,
        &|| true,
    )
    .unwrap();
    reference.pixel_facts();
    assert_eq!(
        canvas.pixels, reference.pixels,
        "交付不能改变字形坐标或颜色"
    );
    assert_eq!(receipt.glyph_range, expected.glyph_range);
    assert_eq!(receipt.first_source_cluster, expected.first_source_cluster);
    assert_eq!(receipt.last_source_cluster, expected.last_source_cluster);
    assert_eq!(receipt.flags_or, expected.flags_or);
    assert_eq!(receipt.final_pen_x_26_6, expected.final_pen_x_26_6);
    assert_eq!(receipt.final_pen_y_26_6, expected.final_pen_y_26_6);
    drop(oracle);
    drop(source);
    drop(document);
    drop(font);
    assert!(!original.unmap_succeeded()); // 尚活Spool正当地保留原Source/Font，不把join当业务释放。
    handoff.spool.close().unwrap();
    assert!(source_payload.payload_released() && font_payload.payload_released());
    assert!(handoff.job_resources.resources_released());
    let released = resources.snapshot();
    assert!(!released.file_open && released.close_succeeded && !released.cleanup_failed);
    assert!(released.source_payload_released && released.font_payload_released);
    assert!(original.unmap_succeeded() && original.mapped_bytes() == 0 && !original.failed());
    assert!(font_bytes.unmap_succeeded() && font_bytes.mapped_bytes() == 0 && !font_bytes.failed());
    println!(
        "native_paged_spool_paint case={label} glyphs={count} ink={ink} colored={colored} worker_joined=true job_dropped=true exact_job_pixels=true actual_unmap=true formal=false"
    );
}

fn spool_controls(base: &CTFont, mtm: MainThreadMarker) {
    let font = native_font(base, "Ab office", mtm);
    let (_file, document) = fixture("Ab office");
    let source = MarkdownLayoutTextSource::capture(&document);
    let make = || {
        worker_spool(
            source.clone(),
            font.core_font().clone(),
            MarkdownShapingDirection::LeftToRight,
            *b"Latn",
        )
    };
    let mut handoff = make();
    let output = handoff.spool.authority();
    let count = handoff.spool.glyph_count();
    let request = |range, origin| NativeMarkdownPagedGlyphPaintRequest {
        source: &handoff.source,
        font_authority: &handoff.font,
        native_font: &font,
        glyph_range: range,
        origin,
    };
    let mut canvas = Canvas::new();
    for (range, origin, allowed, cause) in [
        (0..257, origin(), true, 0),
        (0..count, origin(), false, 1),
        (
            0..count,
            NativeMarkdownPagedGlyphOrigin {
                pen_x_26_6: i64::MIN,
                ..origin()
            },
            true,
            2,
        ),
    ] {
        let error = paint_paged_spool_glyphs(
            &mut handoff.spool,
            &output,
            request(range, origin),
            &canvas.context,
            &|| allowed,
        )
        .unwrap_err();
        assert!(!error.native_draw_returned);
        match cause {
            0 => assert!(matches!(
                error.cause,
                NativeMarkdownPagedGlyphPaintCause::BatchLimit
            )),
            1 => assert!(matches!(
                error.cause,
                NativeMarkdownPagedGlyphPaintCause::Revoked
            )),
            _ => assert!(matches!(
                error.cause,
                NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate
            )),
        }
        assert_eq!(canvas.pixel_facts().0, 0);
    }
    let clip = CGRect {
        origin: CGPoint { x: 48.0, y: 0.0 },
        size: CGSize {
            width: 64.0,
            height: HEIGHT as f64,
        },
    };
    CGContext::clip_to_rect(Some(&canvas.context), clip);
    paint_paged_spool_glyphs(
        &mut handoff.spool,
        &output,
        request(0..count, origin()),
        &canvas.context,
        &|| true,
    )
    .unwrap();
    assert!(canvas.pixel_facts().0 > 0);
    for (index, pixel) in canvas.pixels.as_chunks::<4>().0.iter().enumerate() {
        if !(48..112).contains(&(index % WIDTH)) {
            assert_eq!(*pixel, [255; 4]);
        }
    }
    let mut discarded_canvas = Canvas::new();
    let address = discarded_canvas.pixels.as_ptr();
    let current = || {
        CGContext::flush(Some(&discarded_canvas.context));
        // SAFETY: 固定CPU缓冲不重分配，回调与Native绘制同线程且同步分隔，无并发借用。
        unsafe { std::slice::from_raw_parts(address, PIXEL_BYTES) }
            .iter()
            .all(|byte| *byte == 255)
    };
    let error = paint_paged_spool_glyphs(
        &mut handoff.spool,
        &output,
        request(0..count, origin()),
        &discarded_canvas.context,
        &current,
    )
    .unwrap_err();
    assert!(
        error.native_draw_returned
            && matches!(error.cause, NativeMarkdownPagedGlyphPaintCause::Revoked)
    );
    assert!(discarded_canvas.pixel_facts().0 > 0);
    handoff.spool.close().unwrap();

    // 相同Source/Font的另一个输出仍不能签原spool；错误在实际绘制前返回，原文件撤权归还。
    let other = make();
    let wrong_output = other.spool.authority();
    let mut denied = make();
    let resources = denied.spool.resource_observer();
    let mut blank = Canvas::new();
    let error = paint_paged_spool_glyphs(
        &mut denied.spool,
        &wrong_output,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &denied.source,
            font_authority: &denied.font,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &blank.context,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        error.cause,
        NativeMarkdownPagedGlyphPaintCause::Spool(MarkdownShapedSpoolError::OutputMismatch)
    ));
    assert!(!error.native_draw_returned && blank.pixel_facts().0 == 0);
    assert!(!resources.snapshot().file_open && resources.snapshot().close_succeeded);

    // 相同字节的另一个Source、相同face的另一次NativeFont capture都不授予原payload身份。
    let (_foreign_file, foreign_document) = fixture("Ab office");
    let foreign_source = MarkdownLayoutTextSource::capture(&foreign_document);
    let foreign_font = native_font(base, "Ab office", mtm);
    for wrong_source in [true, false] {
        let mut rejected = make();
        let output = rejected.spool.authority();
        let foreign_source_authority = foreign_source.authority();
        let error = paint_paged_spool_glyphs(
            &mut rejected.spool,
            &output,
            NativeMarkdownPagedGlyphPaintRequest {
                source: if wrong_source {
                    &foreign_source_authority
                } else {
                    &rejected.source
                },
                font_authority: &rejected.font,
                native_font: if wrong_source { &font } else { &foreign_font },
                glyph_range: 0..count,
                origin: origin(),
            },
            &blank.context,
            &|| true,
        )
        .unwrap_err();
        if wrong_source {
            assert!(matches!(
                error.cause,
                NativeMarkdownPagedGlyphPaintCause::Spool(MarkdownShapedSpoolError::SourceMismatch)
            ));
        } else {
            assert!(matches!(
                error.cause,
                NativeMarkdownPagedGlyphPaintCause::Spool(MarkdownShapedSpoolError::FontMismatch)
            ));
        }
        assert!(!error.native_draw_returned && blank.pixel_facts().0 == 0);
        let released = rejected.spool.resource_observer().snapshot();
        assert!(!released.file_open && released.close_succeeded && !released.cleanup_failed);
    }

    // seal并不脱离原任务取消位；收到实际取消后不读取后续glyph，也不能发布原可见像素。
    let mut cancelled = make();
    let output = cancelled.spool.authority();
    cancelled.cancellation.request();
    let error = paint_paged_spool_glyphs(
        &mut cancelled.spool,
        &output,
        NativeMarkdownPagedGlyphPaintRequest {
            source: &cancelled.source,
            font_authority: &cancelled.font,
            native_font: &font,
            glyph_range: 0..count,
            origin: origin(),
        },
        &blank.context,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        error.cause,
        NativeMarkdownPagedGlyphPaintCause::Spool(MarkdownShapedSpoolError::Cancelled)
    ));
    assert!(!error.native_draw_returned && blank.pixel_facts().0 == 0);
    other.spool.close().unwrap();
    println!(
        "native_paged_spool_negative oversized=true revoked=true invalid_coordinate=true exact_output=true exact_source=true exact_native_font=true cancelled=true clip=true actual_draw_then_discard=true formal=false"
    );
}

fn spool_two_batches(base: &CTFont, mtm: MainThreadMarker) {
    let font = native_font(base, "A", mtm);
    let (_file, document) = fixture(&"A".repeat(300));
    let source = MarkdownLayoutTextSource::capture(&document);
    let mut handoff = worker_spool(
        source,
        font.core_font().clone(),
        MarkdownShapingDirection::LeftToRight,
        *b"Latn",
    );
    let output = handoff.spool.authority();
    assert_eq!(handoff.spool.glyph_count(), 300);
    let mut canvas = Canvas::new();
    let mut point = origin();
    for range in [0..256, 256..300] {
        let receipt = paint_paged_spool_glyphs(
            &mut handoff.spool,
            &output,
            NativeMarkdownPagedGlyphPaintRequest {
                source: &handoff.source,
                font_authority: &handoff.font,
                native_font: &font,
                glyph_range: range.clone(),
                origin: point,
            },
            &canvas.context,
            &|| true,
        )
        .unwrap();
        assert_eq!(receipt.glyph_range, range);
        point.pen_x_26_6 = receipt.final_pen_x_26_6;
        point.pen_y_26_6 = receipt.final_pen_y_26_6;
    }
    let (mut expected_x, mut expected_y) = (0i64, 0i64);
    for index in 0..300 {
        let glyph = handoff
            .spool
            .glyph(&output, &handoff.source, &handoff.font, index)
            .unwrap()
            .unwrap();
        expected_x += i64::from(glyph.x_advance);
        expected_y += i64::from(glyph.y_advance);
    }
    assert_eq!(
        (point.pen_x_26_6, point.pen_y_26_6),
        (expected_x, expected_y)
    );
    assert!(canvas.pixel_facts().0 > 0);
    handoff.spool.close().unwrap();
    println!(
        "native_paged_spool_batches first=256 second=44 actual_total=300 exact_advance=true formal=false"
    );
}

fn main() {
    assert!(
        std::env::args_os().nth(1).is_none(),
        "字形控制不接受未知参数"
    );
    let mtm = MainThreadMarker::new().expect("真实CoreText控制须在主线程执行");
    autoreleasepool(|_| {
        #[cfg(feature = "native-markdown-runtime-probe")]
        system_fonts::run(mtm);
        #[cfg(feature = "native-paged-compose-probe")]
        composed_lines::run(mtm);
        #[cfg(feature = "native-paged-compose-probe")]
        control_tabs::run(mtm);
        let name = CFString::from_str("Helvetica");
        // SAFETY: 固定有效CFString和有限正32pt，None矩阵没有外部指针。
        let base = unsafe { CTFont::with_name(&name, 32.0, std::ptr::null()) };
        case(
            &base,
            mtm,
            "office AV a\u{301}",
            *b"Latn",
            false,
            false,
            "latin",
        );
        case(&base, mtm, "中文", *b"Hani", false, false, "cjk");
        case(&base, mtm, "السَّلَامُ", *b"Arab", true, false, "arabic");
        case(&base, mtm, "क्षि", *b"Deva", false, false, "indic");
        case(&base, mtm, "😀", *b"Zyyy", false, true, "emoji");
        case(&base, mtm, "👩🏽‍💻", *b"Zyyy", false, true, "emoji_zwj");
        case(&base, mtm, "👨‍👩‍👧‍👦", *b"Zyyy", false, true, "emoji_family");
        case(&base, mtm, "🇨🇳", *b"Zyyy", false, true, "emoji_flag");
        rejection_and_clip_controls(&base, mtm);
        two_exact_batches(&base, mtm);
        spool_case(
            &base,
            mtm,
            "office AV a\u{301}",
            *b"Latn",
            false,
            false,
            "latin",
        );
        spool_case(&base, mtm, "中文", *b"Hani", false, false, "cjk");
        spool_case(&base, mtm, "السَّلَامُ", *b"Arab", true, false, "arabic");
        spool_case(&base, mtm, "क्षि", *b"Deva", false, false, "indic");
        spool_case(&base, mtm, "😀", *b"Zyyy", false, true, "emoji");
        spool_case(&base, mtm, "👩🏽‍💻", *b"Zyyy", false, true, "emoji_zwj");
        spool_case(&base, mtm, "👨‍👩‍👧‍👦", *b"Zyyy", false, true, "emoji_family");
        spool_case(&base, mtm, "🇨🇳", *b"Zyyy", false, true, "emoji_flag");
        spool_controls(&base, mtm);
        spool_two_batches(&base, mtm);
        println!(
            "native_paged_glyph_paint_control actual_ready_shaping=true actual_bitmap_ink=true kernel_snapshot_unmapped=true native_font_cache_deallocated_verified=false product_geometry_verified=false formal=false"
        );
    });
}
