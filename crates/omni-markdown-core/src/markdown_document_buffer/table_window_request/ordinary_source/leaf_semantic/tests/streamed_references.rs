//! Production Source requests drive the bracket file, unique reference inventory, and original
//! Global; the full parser supplies only an independent test oracle.

use super::*;
use pulldown_cmark::TagEnd;

/// Reference-definition parsing has its existing independent 16-KiB limit. Only actual matching
/// definitions may use it; a long paragraph cannot enter the parser under that allowance, and a
/// whole-definition exception is not a 2048-byte finite-block parse.
fn finish_references(
    request: &mut MarkdownViewportSourceRequest,
    doc: &MarkdownDocumentBuffer,
    source: &str,
) -> Result<(), MarkdownTableRequestError> {
    let parser = Parser::new_ext(
        source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    );
    let definition_lengths: Vec<_> = parser
        .reference_definitions()
        .iter()
        .map(|(_, definition)| definition.span.len())
        .collect();
    for _ in 0..doc.byte_len() * 100 + 10_000 {
        let (MarkdownViewportSourceStep::Progress { work }
        | MarkdownViewportSourceStep::SourcesAvailable { work }) =
            request.advance(doc, authority(doc), 4096)?;
        assert!(work.source_bytes <= 4096);
        assert!(work.structural_transitions <= 4096);
        assert!(
            work.metadata_bytes <= 4096,
            "本步真实metadata超原量子: work={work:?}, total={:?}",
            request.total_work()
        );
        assert!(work.copied_source_bytes <= 2048);
        assert!(work.allocated_source_capacity <= 2048);
        assert!(work.parser_input_bytes <=
            crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_REFERENCE_BYTES);
        if work.parser_input_bytes > 2048 {
            assert!(
                definition_lengths.contains(&work.parser_input_bytes),
                "超小块输入必须是独立Parser确认的真实定义，不能借oracle上限复制长段正文"
            );
        }
        if request.sources_for(doc, authority(doc))?.is_some() {
            return Ok(());
        }
    }
    panic!("原有限引用定义与流式段落未在真实累计额度内完成");
}

/// Compare actual link text/code with the parser's normalized targets through public results.
/// Images have no click target, while real links within their labels retain the original nesting
/// decision; inspect no private record or slot directory.
fn assert_parser_links(
    ready: &VerifiedMarkdownRootLeafViewport,
    doc: &MarkdownDocumentBuffer,
    source: &str,
    requested: &Range<usize>,
) {
    let mut destination = None;
    for (event, range) in Parser::new_ext(
        source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => destination = Some(dest_url.into_string()),
            Event::End(TagEnd::Link) => destination = None,
            Event::Text(_) | Event::Code(_) => {
                let index = units(source, range).start;
                if requested.contains(&index) {
                    let actual = ready.link_at_for(doc, authority(doc), index).unwrap();
                    match destination.as_ref() {
                        Some(expected) => assert_eq!(
                            actual
                                .expect("原Parser的真实Link文字必须可点击")
                                .destination
                                .clone()
                                .into_string(),
                            *expected,
                        ),
                        None => assert!(actual.is_none(), "Image或普通文字不能被伪造为Link"),
                    }
                }
            }
            _ => {}
        }
    }
}

#[test]
fn streamed_reference_variants_nested_images_and_first_definitions_match_complete_parser() {
    // Materialize only the synthetic test fixture. Body/label exceed the original 2-KiB finite-block
    // limit, requiring streaming for interacting nested image links, reference variants, and true
    // EOF-missing outcomes in the same paragraph.
    let target = format!("https://example.invalid/{}", "path&amp;😀".repeat(300));
    let paragraph = format!(
        "{}*outer ![alt **粗体** [内链](../nested)](image.png) after* \
         [完整][ReF] [折叠][] [shortcut] [重复][REF] [失踪][MISSING] tail\n",
        "普通中文😀 ".repeat(700),
    );
    let source = format!(
        "{paragraph}\n[ref]: {target} \"首定义\"\n[REF]: https://example.invalid/wrong\n\
         [折叠]: <目录/目标>\n[shortcut]: ../shortcut\n",
    );
    let requested = units(&source, 0..paragraph.len());
    let inside = units(
        &source,
        source.find("完整").unwrap()..source.find("完整").unwrap(),
    )
    .start;
    for selection in [
        MarkdownTextSelection::caret(source.encode_utf16().count()),
        MarkdownTextSelection::caret(inside),
        MarkdownTextSelection {
            anchor_utf16: inside + 2,
            head_utf16: inside,
        },
    ] {
        let (directory, mut doc) = document(&source);
        let observer = doc.original_backing_observer().unwrap();
        doc.set_selection(selection).unwrap();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, selection)
            .unwrap();
        finish_references(&mut request, &doc, &source).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("真实引用与嵌套Image必须完成，不得以General/Pending丢弃功能");
        };
        assert_oracle(ready, &doc, &source, &requested);
        assert_parser_links(ready, &doc, &source, &requested);
        // Share the actual first definition's normalized target. A URL over 2048 bytes is still
        // bounded destination output, not a loaded paragraph.
        let actual = ready
            .link_at_for(&doc, authority(&doc), inside)
            .unwrap()
            .unwrap();
        assert!(actual.destination.byte_len() > 2048);
        assert_eq!(doc.selection(), selection);
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
    }
}

#[test]
fn streamed_references_resume_forward_windows_and_undo_rejects_all_old_publications() {
    // Both windows belong to the same long root paragraph. Later definitions require reopening the
    // original inventory/restoring Global for newly queried labels. Old publication remains readable
    // until an actual edit; undo restores bytes, never the old Source generation.
    let source = format!(
        "{}[第一处][A] {}[第二处][B] ![图像 [点击][C]](image) tail\n\n\
         [A]: ../first\n[B]: ../second\n[C]: ../nested\n",
        "普通😀 ".repeat(800),
        "后段中文 ".repeat(800),
    );
    let (_directory, mut doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let first_start = source.find("[第一处]").unwrap();
    let second_start = source.find("[第二处]").unwrap();
    let first_range = units(&source, first_start..first_start + "[第一处][A]".len());
    let second_range = units(&source, second_start..source.find("\n\n").unwrap());
    let mut request = doc
        .viewport_leaf_source_request(first_range.clone(), 57, doc.selection())
        .unwrap();
    super::inline::finish_inline(&mut request, &doc).unwrap();
    let first = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    request
        .continue_forward(&doc, &first, second_range.clone(), authority(&doc))
        .unwrap();
    super::inline::finish_inline(&mut request, &doc).unwrap();
    let second = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    for (publication, range) in [(&first, &first_range), (&second, &second_range)] {
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = publication
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("两个真实前移窗口都必须保完整引用/嵌套语义");
        };
        assert_oracle(ready, &doc, &source, range);
        assert_parser_links(ready, &doc, &source, range);
    }
    doc.insert_utf16(0, "改", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    for publication in [&first, &second] {
        assert_eq!(
            publication
                .ordinary_semantics_for(&doc, authority(&doc))
                .err(),
            Some(MarkdownTableRequestError::SourceSuperseded),
        );
    }
    drop((first, second, request, doc));
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn streamed_reference_pause_keeps_cancel_sticky_and_releases_source() {
    // Missing references still require the original Global's true EOF. This case proves cancellation
    // after an actual reference pause, without presenting an injected flag as a kernel Source EIO.
    let source = format!(
        "{}[等待][MISSING] {} tail\n",
        "*a ".repeat(100),
        "中文😀 ".repeat(16_000)
    );
    let (directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let at = source.find("[等待]").unwrap();
    let requested = units(&source, at..at + "[等待][MISSING]".len());
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    let mut reached_reference = false;
    for _ in 0..source.len() * 100 + 10_000 {
        let (MarkdownViewportSourceStep::Progress { work }
        | MarkdownViewportSourceStep::SourcesAvailable { work }) =
            request.advance(&doc, authority(&doc), 4096).unwrap();
        if work.normalizer_input_bytes != 0 && work.parser_input_bytes == 0 {
            reached_reference = true;
            break;
        }
        assert!(
            request
                .sources_for(&doc, authority(&doc))
                .unwrap()
                .is_none()
        );
    }
    assert!(
        reached_reference,
        "必须经过真实流式引用暂停，不能只取消尚未开始的请求"
    );
    request.cancel();
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert!(doc.original_backing_health().is_ok());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    assert!(!doc.can_undo() && !doc.can_redo());
    drop((request, doc));
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn streamed_reference_storage_write_failure_preserves_real_errno_and_releases_source() {
    const CHILD: &str = "OMNI_STREAMED_REFERENCE_STORAGE_CHILD";
    if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
        // Isolate process-wide rlimit/signal changes in this test executable's exact child. Parent
        // Cargo/parallel tests share neither limits nor this child's document/files/request. Derive
        // libtest's exact name without the crate prefix so the child actually executes this case.
        let exact = concat!(
            module_path!(),
            "::streamed_reference_storage_write_failure_preserves_real_errno_and_releases_source"
        );
        let exact = exact.strip_prefix("omni_markdown_core::").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", exact, "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "隔离真实存储失败用例失败: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // Keep the original long prefix/body and add real links after the first reference to continue
    // paging the same file. Failure must come from StackFile's actual pwrite, without replacing a
    // private descriptor or manufacturing errno.
    let source = format!(
        "{}[等待][MISSING] {} {} tail\n",
        "*a ".repeat(100),
        "[链接](../target) ".repeat(100),
        "中文😀 ".repeat(16_000)
    );
    let (directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let at = source.find("[等待]").unwrap();
    // Cover the later links in the actual viewport; the cursor need not write offscreen records
    // beyond its original requested_end.
    let requested = units(&source, at..source.find("中文😀").unwrap());
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    let mut reached_reference = false;
    for _ in 0..source.len() * 100 + 10_000 {
        let (MarkdownViewportSourceStep::Progress { work }
        | MarkdownViewportSourceStep::SourcesAvailable { work }) =
            request.advance(&doc, authority(&doc), 4096).unwrap();
        if work.normalizer_input_bytes != 0 && work.parser_input_bytes == 0 {
            reached_reference = true;
            break;
        }
        assert!(
            request
                .sources_for(&doc, authority(&doc))
                .unwrap()
                .is_none()
        );
    }
    assert!(
        reached_reference,
        "必须在原引用请求实际暂停后才改变独占子进程的文件资源"
    );
    let mut original = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        // SAFETY: this exact child runs only this test; getrlimit writes a complete valid stack struct.
        unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut original) },
        0
    );
    // SAFETY: only this exclusive child temporarily ignores SIGXFSZ so real writes return EFBIG.
    let handler = unsafe { libc::signal(libc::SIGXFSZ, libc::SIG_IGN) };
    assert_ne!(handler, libc::SIG_ERR);
    let limited = libc::rlimit {
        rlim_cur: 0,
        rlim_max: original.rlim_max,
    };
    // SAFETY: lower only this child's soft limit, retaining its hard limit after all fixture writes complete.
    let admitted = unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limited) };
    let mut failure = None;
    if admitted == 0 {
        for _ in 0..source.len() * 100 + 10_000 {
            match request.advance(&doc, authority(&doc), 4096) {
                Ok(MarkdownViewportSourceStep::Progress { .. }) => {}
                Ok(MarkdownViewportSourceStep::SourcesAvailable { .. }) => break,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
    }
    // SAFETY: restore original limits before result assertions, logging, checks, and resource cleanup.
    let restored = unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &original) };
    // SAFETY: handler is the actual previous signal handler; the sole limited operation has ended.
    let signal_restored = unsafe { libc::signal(libc::SIGXFSZ, handler) };
    assert_eq!(admitted, 0);
    assert_eq!(restored, 0);
    assert_ne!(signal_restored, libc::SIG_ERR);
    let error = MarkdownTableRequestError::InlineStorageUnavailable(libc::EFBIG);
    assert_eq!(
        failure,
        Some(error),
        "必须收到原匿名文件真实写入错误，不得发布部分语义"
    );
    request.cancel();
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(error)
    );
    assert!(
        doc.original_backing_health().is_ok(),
        "失败归属于元数据文件，不能伪报Source EIO"
    );
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    assert!(!doc.can_undo() && !doc.can_redo());
    drop((request, doc));
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}
