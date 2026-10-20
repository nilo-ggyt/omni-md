//! 同一真实 UIFont 的完整短串对照；语言只作观察，默认 None 仍须独立匹配 Native 默认。

use std::{io::Write, ptr::NonNull};

use objc2::{AnyThread, rc::autoreleasepool, runtime::AnyObject};
use objc2_app_kit::{NSFont, NSFontAttributeName};
use objc2_core_foundation::{CFRange, CFString, CFType, CGSize};
use objc2_core_text::{CTFont, CTLine, CTRun};
use objc2_foundation::{MainThreadMarker, NSAttributedString, NSDictionary, NSString};
use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownLayoutFont, MarkdownLayoutTextSource, MarkdownShapingBudget,
    MarkdownShapingDirection, MarkdownShapingJob, MarkdownShapingOptions, MarkdownShapingStatus,
};

use super::{MAXIMUM_FONT_BYTES, NativeMarkdownPagedFont, assert_released};

// 只允许这个固定五字符例子，完整输入远小于2KiB；这里没有产品文稿或长段落入口。
const SOURCE: &str = "中文，中文";
const GLYPHS: usize = 64;

/// 两侧只交换真实 Copy 字形事实；不把 CTFont、Job、Source 或映射藏进诊断结果。
#[derive(Debug, Eq, PartialEq)]
struct Glyph {
    source_utf16: usize,
    id: u32,
    advance_26_6: i32,
}

struct Observation {
    language: Option<&'static str>,
    glyphs: Vec<Glyph>,
}

/// 原 Core 完整短 Job 始终走实际输入、shape 和逐 glyph 读取，不能用 nominal 查询代替。
fn core_glyphs(
    source: &MarkdownLayoutTextSource,
    font: &MarkdownLayoutFont,
    language: Option<&'static str>,
) -> Result<Vec<Glyph>, String> {
    let source_authority = source.authority();
    let font_authority = font.authority();
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &source_authority,
        font.clone(),
        &font_authority,
        0..SOURCE.encode_utf16().count(),
        MarkdownShapingOptions {
            direction: MarkdownShapingDirection::LeftToRight,
            script: *b"Hani",
            language: language.map(str::to_owned),
            points: 14.0,
            beginning_of_text: true,
            end_of_text: true,
            features: Vec::new(),
        },
    )
    .map_err(|cause| format!("完整短例 Core Job 构造失败：{cause:?}"))?;
    let resources = job.resource_observer();
    let result = (|| {
        // 正配额足以消费固定短源；保留真实分步合同，不从字节数伪造已完成状态。
        while job.status() == MarkdownShapingStatus::Ingesting {
            job.step(
                &source_authority,
                &font_authority,
                MarkdownShapingBudget::default(),
            )
            .map_err(|cause| format!("完整短例 Core 输入失败：{cause:?}"))?;
        }
        job.shape(&source_authority, &font_authority)
            .map_err(|cause| format!("完整短例 Core 整形失败：{cause:?}"))?;
        let profile = job
            .profile()
            .map_err(|cause| format!("完整短例 Core 字形事实失败：{cause:?}"))?;
        if !(1..=GLYPHS).contains(&profile.glyph_count) || !profile.shaper_access_certified {
            return Err("完整短例的真实字形域为空、超出固定栈界或分页访问未认证".into());
        }
        let mut glyphs = Vec::with_capacity(profile.glyph_count);
        for ordinal in 0..profile.glyph_count {
            let glyph = job
                .glyph(&source_authority, &font_authority, ordinal)
                .map_err(|cause| format!("完整短例 Core 读取字形失败：{cause:?}"))?
                .ok_or("完整短例 Core 字形提前 EOF")?;
            if glyph.y_advance != 0 {
                return Err("完整短例 Core 产生非水平推进，不能忽略后再声称相同".into());
            }
            glyphs.push(Glyph {
                source_utf16: glyph.source_utf16_cluster,
                id: glyph.glyph_id,
                advance_26_6: glyph.x_advance,
            });
        }
        if job
            .glyph(&source_authority, &font_authority, profile.glyph_count)
            .map_err(|cause| format!("完整短例 Core EOF 认证失败：{cause:?}"))?
            .is_some()
        {
            return Err("完整短例 Core 的实际 glyph 数超过原 profile".into());
        }
        Ok(glyphs)
    })();
    // 即使读取或比较将失败，也先真正销毁本 Job，再检查它的文件、映射和工作块。
    drop(job);
    let released = resources.snapshot();
    if released.files_open != 0
        || released.mappings_open != 0
        || released.mapped_bytes != 0
        || released.work_bytes != 0
        || released.cleanup_failed
        || released.first_error.is_some()
        || !released.shaper_access_certified
    {
        return Err(format!("完整短例 Core 字形存储没有实际释放：{released:?}"));
    }
    result
}

/// 三种语言共用同一原文件、mmap 与字体实例；语言观察失败不会绕过真实 Source 释放。
fn core_observations(font: MarkdownLayoutFont) -> Result<Vec<Observation>, String> {
    let mut file = tempfile::NamedTempFile::new()
        .map_err(|cause| format!("固定短例临时文件创建失败：{cause}"))?;
    file.write_all(SOURCE.as_bytes())
        .map_err(|cause| format!("固定短例写入失败：{cause}"))?;
    let document = MarkdownDocumentBuffer::open(file.path(), 2048)
        .map_err(|cause| format!("固定短例 mmap 文档打开失败：{cause:?}"))?;
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_resources = source.resource_observer();
    let backing = document
        .original_backing_observer()
        .ok_or("固定文件例没有取得真实原 backing")?;
    let result = (|| {
        let mut observations = Vec::with_capacity(3);
        for language in [None, Some("zh-Hans"), Some("en")] {
            let glyphs = core_glyphs(&source, &font, language)?;
            println!(
                "native_paged_punctuation side=core language={} glyphs={glyphs:?}",
                language.unwrap_or("none")
            );
            observations.push(Observation { language, glyphs });
        }
        Ok(observations)
    })();
    drop((source, document, file, font));
    if !source_resources.payload_released()
        || !backing.unmap_succeeded()
        || backing.mapped_bytes() != 0
        || backing.failed()
        || backing.first_errno().is_some()
    {
        return Err("固定短例的真实 Source 或原 mmap 没有归还".into());
    }
    result
}

/// 原生参考只用完整固定短串与捕获时同一个真实 CTFont，不从候选 glyph 或局部子串反造期望。
fn native_glyphs(font: &CTFont) -> Result<Vec<Glyph>, String> {
    autoreleasepool(|_| {
        let text = NSString::from_str(SOURCE);
        let font_type: &CFType = font.as_ref();
        let font_object: &AnyObject = font_type.as_ref();
        // SAFETY: AppKit 的 Font 属性键与当前仍活的原 CTFont/NSFont 公共桥接类型匹配。
        let attributes =
            unsafe { NSDictionary::from_slices(&[NSFontAttributeName], &[font_object]) };
        // SAFETY: 固定完整短串及其单个真实字体均由本栈持有；没有语言或特性覆盖。
        let attributed = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &text,
                Some(&attributes),
            )
        };
        // SAFETY: 原属性串完整保留上下文；不会把待比标点截成另一个孤立 CTLine。
        let line = unsafe { CTLine::with_attributed_string(attributed.as_ref()) };
        // SAFETY: 不可变 line 的真实字形域与来源只读；所有后续 run 借用均在本池内结束。
        let (range, count, runs) =
            unsafe { (line.string_range(), line.glyph_count(), line.glyph_runs()) };
        let count = usize::try_from(count).map_err(|_| "原生短例字形数无效")?;
        let length = SOURCE.encode_utf16().count();
        if range != CFRange::new(0, length as _) || !(1..=GLYPHS).contains(&count) {
            return Err("原生参考未覆盖完整固定短串或超过64字形，禁止截断".into());
        }
        let mut result = Vec::with_capacity(count);
        let mut glyphs = [0_u16; GLYPHS];
        let mut advances = [CGSize::ZERO; GLYPHS];
        let mut indices = [0_isize; GLYPHS];
        for ordinal in 0..runs.len() {
            let ordinal = isize::try_from(ordinal).map_err(|_| "原生短例 run 下标无效")?;
            // SAFETY: ordinal 来自活 line 的公开数组；CTRun 的借用不离开本地自动释放池。
            let raw = unsafe { runs.value_at_index(ordinal) };
            let run = NonNull::new(raw.cast_mut().cast::<CTRun>()).ok_or("原生短例 run 为空")?;
            // SAFETY: 原 run 指针由上方真实数组保活，返回的数量和矩阵均为 Copy 值。
            let (used, matrix) =
                unsafe { (run.as_ref().glyph_count(), run.as_ref().text_matrix()) };
            let used = usize::try_from(used).map_err(|_| "原生短例 run 字形数无效")?;
            if used > GLYPHS
                || result.len() + used > count
                || (matrix.a, matrix.b, matrix.c, matrix.d, matrix.tx, matrix.ty)
                    != (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
            {
                return Err("原生短例真实字形域或矩阵不属于完整水平参考".into());
            }
            if used == 0 {
                continue; // 数量为零的真实 run 不读未初始化数组，也不增添合成 glyph。
            }
            // SAFETY: 三个固定数组均可完整容纳 used≤64 个真实 glyph，输出全量而非采样。
            unsafe {
                run.as_ref().glyphs(
                    CFRange::new(0, used as _),
                    NonNull::new(glyphs.as_mut_ptr()).expect("固定 glyph 数组非空"),
                );
                run.as_ref().advances(
                    CFRange::new(0, used as _),
                    NonNull::new(advances.as_mut_ptr()).expect("固定 advance 数组非空"),
                );
                run.as_ref().string_indices(
                    CFRange::new(0, used as _),
                    NonNull::new(indices.as_mut_ptr()).expect("固定 cluster 数组非空"),
                );
            }
            for index in 0..used {
                let source_utf16 =
                    usize::try_from(indices[index]).map_err(|_| "原生短例 glyph 来源索引无效")?;
                let advance = (advances[index].width * 64.0).round();
                if source_utf16 >= length
                    || !advance.is_finite()
                    || advance < f64::from(i32::MIN)
                    || advance > f64::from(i32::MAX)
                    || advances[index].height != 0.0
                {
                    return Err("原生短例 glyph 来源或推进不能表示为真实水平26.6".into());
                }
                result.push(Glyph {
                    source_utf16,
                    id: u32::from(glyphs[index]),
                    advance_26_6: advance as i32,
                });
            }
        }
        if result.len() != count {
            return Err("原生短例没有交付 line 声明的全部真实 glyph".into());
        }
        println!("native_paged_punctuation side=native language=unset glyphs={result:?}");
        Ok(result)
    })
}

pub(super) fn run(mtm: MainThreadMarker) {
    let base = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
    let scalar = CFString::from_str("，");
    let base_font: &CTFont = base.as_ref();
    // SAFETY: 同一个原 mono14 为完整 FF0C 选择真实回退字体；不按别名重建或改字号。
    let native = unsafe { base_font.for_string(&scalar, CFRange::new(0, 1)) };
    let captured = NativeMarkdownPagedFont::capture(&native, MAXIMUM_FONT_BYTES, mtm, &|| true)
        .expect("固定标点例的真实 UIFont 必须经过原生产捕获边界");
    let bytes = captured.bytes_observer();
    let payload = captured.core_font().resource_observer();
    println!(
        "native_paged_punctuation source_bytes={} font_metadata={:?} point_size={} actual_capture=true",
        SOURCE.len(),
        captured.core_font().metadata(),
        captured.point_size()
    );
    let core_font = captured.core_font().clone();
    // 原非 Send Job 在真实工作线程内构造和销毁；主线程只交原不可变 Font，并接纯值。
    let worker = std::thread::spawn(move || core_observations(core_font));
    let actual_native = native_glyphs(captured.native_font());
    let actual_core = worker.join();
    drop((captured, native, scalar, base));
    assert!(payload.payload_released(), "固定标点例仍保活 Core Font");
    assert_released(&bytes);
    // 默认比对放在真实字形、Source、Font 和 mmap 释放之后；不能因差异 panic 漏掉清理核验。
    let actual_core = actual_core
        .expect("固定短例 Core 工作线程必须实际结束")
        .expect("固定短例 Core 计算和真实资源释放必须成功");
    let actual_native = actual_native.expect("固定短例原生完整上下文必须成功");
    let default = actual_core
        .iter()
        .find(|observation| observation.language.is_none())
        .expect("真实 None 语言观察不可缺失");
    assert_eq!(
        default.glyphs, actual_native,
        "同一 UIFont 的 Core 默认 None 必须与 Native 默认完整短串一致；不能择另一语言过关"
    );
    println!(
        "native_paged_punctuation default_language_exact=true explicit_languages_observed=true glyph_stores_released=true source_released=true font_unmapped=true formal=false"
    );
}
