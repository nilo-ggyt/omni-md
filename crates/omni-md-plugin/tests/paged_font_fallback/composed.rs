//! 原组合器在真实worker中保有唯一依赖；Native字体回退不接收第二份字素或脚本Job。

use std::{sync::mpsc, thread::JoinHandle};

use objc2_app_kit::NSFont;
use omni_markdown_core::{
    MarkdownLayoutFont, MarkdownLineComposeCancellation, MarkdownLineComposeError,
    MarkdownLineComposeStyle, MarkdownLineFontProbe, MarkdownLineItemAnswer,
    MarkdownLineItemCoordinator, MarkdownLineStyleRequest,
};
use omni_md_plugin::native_surface::paged_font_fallback::NativeMarkdownProbedFontFallbackRequest;

use super::*;

enum Command {
    Consume(MarkdownLayoutFont),
    Close,
}

struct Owner {
    sender: mpsc::SyncSender<Command>,
    acknowledged: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}
impl Owner {
    fn consume(&self, font: &NativeMarkdownPagedFont) {
        self.sender
            .send(Command::Consume(font.core_font().clone()))
            .unwrap();
        self.acknowledged.recv().unwrap(); // 真实provide_style返回以后才测试旧试次，不能只改一个bool。
    }
    fn close(mut self) {
        self.sender.send(Command::Close).unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}

fn pending(
    source: MarkdownLayoutTextSource,
) -> (Owner, MarkdownLineStyleRequest, MarkdownLineFontProbe) {
    let (commands, received) = mpsc::sync_channel(1);
    let (acknowledge, acknowledged) = mpsc::sync_channel(1);
    let (ready, packet) = mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        let mut coordinator = MarkdownLineItemCoordinator::new(
            source.clone(),
            &source.authority(),
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let mut pending = None;
        for _ in 0..10_000 {
            match coordinator
                .step(omni_markdown_core::MarkdownLineComposeBudget::default())
                .unwrap()
                .answer
            {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    pending = Some(request);
                    break;
                }
                _ => panic!("短合成字体输入必须先交付真实样式请求"),
            }
        }
        let request = pending.expect("原worker必须实际完成同一组有限元数据");
        let probe = coordinator.font_probe(&request).unwrap();
        ready.send((request.clone(), probe)).unwrap();
        while let Ok(command) = received.recv() {
            match command {
                Command::Consume(font) => {
                    coordinator
                        .provide_style(
                            request
                                .clone()
                                .respond(
                                    request.source_range(),
                                    font,
                                    MarkdownLineComposeStyle {
                                        points: 14.0,
                                        language: None,
                                        features: Vec::new(),
                                    },
                                )
                                .unwrap(),
                        )
                        .unwrap();
                    acknowledge.send(()).unwrap();
                }
                Command::Close => break,
            }
        }
        drop(coordinator); // 原worker局部的非Send Job只在其构造线程归还，不强行送回主线程。
    });
    let (request, probe) = packet.recv().unwrap();
    (
        Owner {
            sender: commands,
            acknowledged,
            thread: Some(thread),
        },
        request,
        probe,
    )
}

fn owner(
    probe: MarkdownLineFontProbe,
    font: &NativeMarkdownPagedFont,
    mtm: MainThreadMarker,
) -> NativeMarkdownFontFallback {
    NativeMarkdownFontFallback::new_with_probe(
        probe,
        font,
        &font.core_font().authority(),
        NativeMarkdownProbedFontFallbackRequest {
            style: MarkdownLineComposeStyle {
                points: font.point_size() as f32,
                language: None,
                features: Vec::new(),
            },
            languages: LANGUAGES,
            cascade_budget: NativeMarkdownFontCascadeBudget::default(),
            maximum_font_bytes: MAX_FONT_BYTES,
        },
        mtm,
    )
    .unwrap()
}

fn source(
    text: &str,
) -> (
    tempfile::NamedTempFile,
    MarkdownDocumentBuffer,
    MarkdownLayoutTextSource,
) {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(text.as_bytes()).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    (file, document, source)
}

fn selected(
    text: &str,
    base: &NativeMarkdownPagedFont,
    mtm: MainThreadMarker,
    transfer_allowed: bool,
) {
    let (file, document, source) = source(text);
    let observer = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let (worker, request, probe) = pending(source.clone());
    assert!(probe.accepts_request(&request).unwrap());
    let range = probe.source_range();
    let mut fallback = owner(probe, base, mtm);
    let mut selected = false;
    // 原系统列表最多256候选，另有唯一Base；不能用任意32次限制截掉真实emoji字体。
    for _ in 0..=omni_md_plugin::native_surface::paged_font_cascade::NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES {
        let trial = match fallback
            .next_trial_probed(base, &base.core_font().authority(), &|| true)
            .unwrap()
        {
            NativeMarkdownFontFallbackStep::Trial(trial) => trial,
            NativeMarkdownFontFallbackStep::UnavailableCandidate { .. } => continue,
            NativeMarkdownFontFallbackStep::Exhausted { .. } => {
                panic!("实际系统级联必须支持该有限中文/组合/emoji字素")
            }
            NativeMarkdownFontFallbackStep::PendingDependencies => {
                panic!("原协调器已签票，不允许第二份依赖保持Pending")
            }
        };
        let result = std::thread::spawn(move || trial.run()).join().unwrap();
        stores_released(&result);
        match fallback
            .complete_trial_probed(base, &base.core_font().authority(), result, &|| true)
            .unwrap()
        {
            NativeMarkdownFontFallbackCompletion::Rejected { .. } => {}
            NativeMarkdownFontFallbackCompletion::Selected(choice) => {
                assert_eq!(choice.source_range(), range);
                assert_eq!(choice.facts().missing_glyphs, 0);
                let chosen = choice.font(base, &source).unwrap();
                let selected_authority = chosen.core_font().authority();
                let selected_bytes = chosen.bytes_observer();
                let captured = choice.origin() != NativeMarkdownFontOrigin::Base;
                println!(
                    "paged_composed_font selected={:?} source_path={:?} metadata={:?} point_size={} script={:?} source_range={:?} duplicate_indexes=false whole_run_proven=false",
                    choice.origin(),
                    chosen.source_path(),
                    chosen.core_font().metadata(),
                    chosen.point_size(),
                    request.script(),
                    range
                );
                let transferred = choice.into_captured_font(base, &source, &|| transfer_allowed);
                if transfer_allowed {
                    let transferred = transferred.unwrap();
                    assert_eq!(transferred.is_some(), captured);
                    if let Some(font) = transferred {
                        assert!(font.core_font().permits(&selected_authority));
                        assert!(!selected_bytes.mapping_drop_completed());
                        drop(font); // 仅转移同一capture；最后实际owner结束才必须看到FD与mmap归还。
                    }
                } else {
                    assert!(matches!(transferred, Err(NativeMarkdownFontFallbackError::Revoked)));
                }
                if captured {
                    assert!(selected_bytes.mapping_drop_completed() && selected_bytes.unmap_succeeded());
                    assert!(selected_bytes.source_descriptor_closed() && selected_bytes.backing_descriptor_closed());
                    assert_eq!(selected_bytes.mapped_bytes(), 0);
                    assert!(!selected_bytes.failed());
                } // Base仍由调用方持活，不能因本选择结束就把它记为已释放。
                selected = true;
                break;
            }
        }
    }
    assert!(selected);
    fallback.close();
    worker.close();
    drop(source);
    drop(document);
    drop(file);
    assert!(observer.payload_released() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
}

fn stale_trial(base: &NativeMarkdownPagedFont, mtm: MainThreadMarker, before_shape: bool) {
    let (file, document, source) = source("a");
    let observer = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let (worker, _request, probe) = pending(source.clone());
    let mut fallback = owner(probe, base, mtm);
    let NativeMarkdownFontFallbackStep::Trial(trial) = fallback
        .next_trial_probed(base, &base.core_font().authority(), &|| true)
        .unwrap()
    else {
        panic!("真实完整Latin GCB应得到唯一Base试次");
    };
    if before_shape {
        worker.consume(base); // 在packet进入worker前真实消费原request，构造不能产生新的字形FD。
    }
    let result = std::thread::spawn(move || trial.run()).join().unwrap();
    if before_shape {
        assert!(matches!(
            result.outcome(),
            Err(NativeMarkdownFontFallbackError::Compose(
                MarkdownLineComposeError::Poisoned
            ))
        ));
        assert!(result.resources().is_none());
    } else {
        stores_released(&result);
        assert!(result.outcome().is_ok());
        worker.consume(base); // 真实shape成功后、主线程消费结果前撤原scope，不能接受旧成功。
    }
    assert!(matches!(
        fallback.complete_trial_probed(base, &base.core_font().authority(), result, &|| true),
        Err(NativeMarkdownFontFallbackError::Compose(
            MarkdownLineComposeError::Poisoned
        ))
    ));
    fallback.close();
    worker.close();
    drop(source);
    drop(document);
    drop(file);
    assert!(observer.payload_released() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
}

/// 原current回调真的消费请求后，回调前的健康和bool都不能补签已撤票。
fn reentrant_request_consumption(base: &NativeMarkdownPagedFont, mtm: MainThreadMarker) {
    let (file, document, source) = source("a");
    let observer = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let (worker, _request, probe) = pending(source.clone());
    let mut fallback = owner(probe, base, mtm);
    let consumed = std::cell::Cell::new(false);
    let current = || {
        if !consumed.replace(true) {
            worker.consume(base); // 同一个真实Core请求完成，不以测试bool伪造Style已失效。
        }
        false
    };
    assert!(matches!(
        fallback.next_trial_probed(base, &base.core_font().authority(), &current),
        Err(NativeMarkdownFontFallbackError::Compose(
            MarkdownLineComposeError::Poisoned
        ))
    ));
    assert!(consumed.get());
    fallback.close();
    worker.close();
    drop(source);
    drop(document);
    drop(file);
    assert!(observer.payload_released() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
}

pub(super) fn verify(mtm: MainThreadMarker) {
    let native = NSFont::systemFontOfSize(14.0);
    let base =
        NativeMarkdownPagedFont::capture(native.as_ref(), MAX_FONT_BYTES, mtm, &|| true).unwrap();
    for text in ["中后继汉字", "a\u{301}next", "👩🏽‍💻后继"] {
        selected(text, &base, mtm, true);
    }
    selected("中后继", &base, mtm, false); // 失权时拒绝转移真实fallback，并验证其原捕获资源实际释放。
    stale_trial(&base, mtm, true);
    stale_trial(&base, mtm, false);
    reentrant_request_consumption(&base, mtm);
    println!(
        "paged_composed_font single_original_dependency=true selected=3 stale=3 actual_capture_transfer=3 transfer_revoked=1 actual_source_unmapped=true full_product_pass=false"
    );
}
