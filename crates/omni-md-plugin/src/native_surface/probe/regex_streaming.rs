//! Foundation正则对真实分段NSString的独立负控；不创建窗口或编辑器，不改变Find实现。

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

use objc2::rc::{Weak, autoreleasepool};
use objc2::runtime::Bool;
use objc2_foundation::{
    MainThreadMarker, NSMatchingFlags, NSMatchingOptions, NSRange, NSRegularExpression,
    NSRegularExpressionOptions, NSTextCheckingResult, ns_string,
};
use omni_markdown_core::MarkdownDocumentBuffer;

use super::super::text_storage::PieceBackedMarkdownString;
use super::super::{SharedMarkdownDocument, string_read_diagnostic};

const READ_STAGE: i16 = 650;

/// 只接受driver自建的有限ASCII夹具；Z$仍以原Foundation API扫描整个真实NSRange。
/// 返回Ok仅表示匹配、canonical和真实资源收据一致，读取/内存是否有界必须看诊断事实。
pub fn run_native_markdown_long_regex_control(
    path: &Path,
    expected_bytes: usize,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if !matches!(expected_bytes, 16_777_216 | 67_108_864) {
        return Err("正则负控只接受16MiB或64MiB合成夹具".to_owned());
    }
    if std::env::var_os("OMNI_NATIVE_FROZEN_VALUES").is_some() {
        return Err("正则负控须移除OMNI_NATIVE_FROZEN_VALUES，保持原live字符串路径".to_owned());
    }
    let metadata_before = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata_before.len() != expected_bytes as u64 {
        return Err("合成正则夹具实际长度不匹配".to_owned());
    }
    let document: SharedMarkdownDocument = Rc::new(RefCell::new(
        MarkdownDocumentBuffer::open(path, (expected_bytes + 65_536) as u64)
            .map_err(|error| error.to_string())?,
    ));
    let document_weak = Rc::downgrade(&document);
    let backing = document
        .borrow()
        .original_backing_observer()
        .ok_or_else(|| "非空夹具没有实际原始映射观察器".to_owned())?;
    let before = {
        let source = document.borrow();
        if source.byte_len() != expected_bytes || source.utf16_len() != expected_bytes {
            return Err("夹具必须是等长ASCII源，不能用虚报NSString长度替代".to_owned());
        }
        (
            source.generation(),
            source.byte_len(),
            source.utf16_len(),
            source.selection(),
            source.is_dirty(),
            source
                .current_revision()
                .map_err(|error| error.to_string())?,
        )
    }; // 所有Doc借用在第一个Native构造前结束。

    let native_started = Instant::now();
    let (string_weak, regex_weak, scan) = autoreleasepool(|_| {
        let string = PieceBackedMarkdownString::new(&document, mtm);
        let string_weak = Weak::from_retained(&string);
        let compiled = NSRegularExpression::regularExpressionWithPattern_options_error(
            ns_string!("Z$"),
            NSRegularExpressionOptions::empty(),
        );
        let (regex_weak, scan) = match compiled {
            Ok(regex) => {
                let weak = Weak::from_retained(&regex);
                string_read_diagnostic::begin(READ_STAGE);
                let start = Instant::now();
                // 完整原范围照常转交；Foundation若请求整段buffer，原primitive完整供给并计量。
                let matched = Cell::new(0usize);
                let nil_callbacks = Cell::new(0usize);
                let found = Cell::new(None);
                let callback_failed = Cell::new(false);
                let block = block2::StackBlock::new(
                    |result: *mut NSTextCheckingResult,
                     flags: NSMatchingFlags,
                     mut stop: NonNull<Bool>| {
                        if flags.contains(NSMatchingFlags::InternalError) {
                            callback_failed.set(true);
                            // SAFETY: 公开同步枚举在本次回调内提供有效BOOL输出，内部错误不能当无匹配。
                            unsafe { *stop.as_mut() = Bool::YES };
                            return;
                        }
                        // SAFETY: 公开匹配回调的result可以nil；非nil对象仅在本次调用中借用，不保留。
                        let Some(result) = (unsafe { result.as_ref() }) else {
                            if let Some(count) = nil_callbacks.get().checked_add(1) {
                                nil_callbacks.set(count);
                            } else {
                                callback_failed.set(true);
                                // SAFETY: 同一次有效stop输出；计数失真后不继续签完整扫描。
                                unsafe { *stop.as_mut() = Bool::YES };
                            }
                            return; // ReportProgress的nil不是空匹配，也不提早结束正常搜索。
                        };
                        let native = result.range();
                        let next = matched.get().checked_add(1);
                        if native != NSRange::new(expected_bytes - 1, 1) || next != Some(1) {
                            callback_failed.set(true);
                            // SAFETY: 原匹配回调的BOOL指针只在当前调用内写入，异常范围明确终止负控。
                            unsafe { *stop.as_mut() = Bool::YES };
                            return;
                        }
                        matched.set(1);
                        found.set(Some(native)); // 仅保留实际坐标，没有match对象、正文或Native反向强边。
                    },
                );
                regex.enumerateMatchesInString_options_range_usingBlock(
                    &string,
                    NSMatchingOptions::ReportProgress
                        | NSMatchingOptions::WithTransparentBounds
                        | NSMatchingOptions::WithoutAnchoringBounds,
                    NSRange::new(0, expected_bytes),
                    &block,
                );
                let seconds = start.elapsed().as_secs_f64();
                println!(
                    "native_long_regex phase=scan api=enumerate_matches bytes={expected_bytes} utf16_units={expected_bytes} elapsed_seconds={seconds:.6} matches={} nil_callbacks={} found_location={} found_length={} callback_failed={} synthetic_only=true window_created=false whole_product_pass=false",
                    matched.get(),
                    nil_callbacks.get(),
                    found.get().map_or(usize::MAX, |range| range.location),
                    found.get().map_or(0, |range| range.length),
                    callback_failed.get(),
                );
                let result = if !callback_failed.get()
                    && matched.get() == 1
                    && found.get() == Some(NSRange::new(expected_bytes - 1, 1))
                {
                    Ok(())
                } else {
                    Err("原Foundation正则未完整枚举唯一真实尾部Z的全局UTF16范围".to_owned())
                };
                (Some(weak), result)
            }
            Err(_) => (
                None,
                Err("固定合法Z$正则编译失败，不能用空结果冒充负控".to_owned()),
            ),
        };
        (string_weak, regex_weak, scan)
    }); // regex/string以及所有Foundation临时值先真正归还，再观察Weak和累计计数。
    string_read_diagnostic::report(READ_STAGE);
    let bounded_read_gate = string_read_diagnostic::verify_process_read_gate();
    println!(
        "native_long_regex phase=read_gate observed_gate_ok={} primitive_scope_only=true zero_copy_certified=false whole_product_pass=false",
        bounded_read_gate.is_ok()
    ); // 负控真实暴露失败；不能因此提前跳过canonical或资源释放检查。

    let canonical = {
        let source = document.borrow();
        let after = source.current_revision().map_err(|error| error.to_string());
        after.and_then(|revision| {
            if before
                == (
                    source.generation(),
                    source.byte_len(),
                    source.utf16_len(),
                    source.selection(),
                    source.is_dirty(),
                    revision,
                )
                && source.original_file_identity_is_unchanged()
            {
                Ok(())
            } else {
                Err("正则扫描改变了原canonical/选择/保存身份".to_owned())
            }
        })
    };
    let disk = std::fs::metadata(path)
        .map_err(|error| error.to_string())
        .and_then(|after| {
            use std::os::unix::fs::MetadataExt;
            if (
                metadata_before.dev(),
                metadata_before.ino(),
                metadata_before.len(),
                metadata_before.mtime(),
                metadata_before.mtime_nsec(),
            ) == (
                after.dev(),
                after.ino(),
                after.len(),
                after.mtime(),
                after.mtime_nsec(),
            ) {
                Ok(())
            } else {
                Err("合成文件的身份、长度或修改时间改变".to_owned())
            }
        });
    let native_nil = string_weak.load().is_none()
        && regex_weak
            .as_ref()
            .is_none_or(|value| value.load().is_none());
    drop(document);
    let released = document_weak.upgrade().is_none()
        && backing.unmap_succeeded()
        && backing.mapped_bytes() == 0
        && !backing.failed();
    println!(
        "native_long_regex phase=closed bytes={expected_bytes} elapsed_native_seconds={:.6} string_regex_weak_nil={native_nil} document_weak_nil={} kernel_unmap_succeeded={} mapped_bytes={} source_failed={} peak_managed_resident_bytes={} canonical_unchanged={} file_unchanged={} whole_product_pass=false",
        native_started.elapsed().as_secs_f64(),
        document_weak.upgrade().is_none(),
        backing.unmap_succeeded(),
        backing.mapped_bytes(),
        backing.failed(),
        backing.peak_managed_resident_bytes(),
        canonical.is_ok(),
        disk.is_ok(),
    );
    // 先完成全部真实资源观察，再按原失败顺序返回；匹配失败不能留下尚未释放的图。
    scan?;
    canonical?;
    disk?;
    if !native_nil || !released {
        return Err("正则负控的实际Native/文档/原映射没有完整归还".to_owned());
    }
    Ok(())
}
