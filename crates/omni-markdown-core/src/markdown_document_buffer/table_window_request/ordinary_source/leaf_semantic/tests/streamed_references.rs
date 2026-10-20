//! 原生产Source请求驱动括号文件、唯一引用库存与Global；完整Parser只作为测试中的独立oracle。

use super::*;
use pulldown_cmark::TagEnd;

/// 原定义oracle有独立16KiB上限；这里只让真实匹配定义使用该既有边界，
/// 不把长段落整体交给Parser，也不把该whole-definition例外伪装成2048小块。
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

/// 所有真实Link文字/代码都与原Parser的规范化目标核对；Image本身不取得点击目标，
/// 但其label内真实Link仍由原嵌套决策授权。这里只消费公开结果，不遍历私有记录/槽。
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
    // 只有合成fixture在测试中物化；正文/label超原2KiB门，不能让短块Parser代替新流式端口。
    // 真实Image内嵌Link、Full/Collapsed/Shortcut与EOF Missing在同一原段落相互影响。
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
        // Map只共享首定义的真实规范化目标。输出URL大于2048仍是有限URI，不加载正文。
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
    // 两窗都属于同一真实长Root段落；定义在后方，第二窗新增查询必须reopen原库存并恢复Global。
    // 原已发布第一窗继续可读；真正编辑后即使Undo回原字节也不能复活旧Source权限。
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
    // 未定义引用仍交原Global确认真实EOF；本用例只证明真实暂停后的取消，
    // 不借测试注入标志把人工错误冒称内核Source EIO。
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
        // rlimit与signal是进程资源，必须由当前测试exe的独占--exact子进程承受。
        // 父cargo/其他并行测试不改限制，也不共享下面创建的Doc、文件或请求。
        // libtest的名称不含crate前缀；由当前真实模块名导出，不能传错exact名字空跑。
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
    // 原长前缀/正文仍在，首引用后新增真正的直接链接令同一个文件继续跨页；
    // 失败来自原StackFile的pwrite，而不是从测试私态替换FD或伪造返回errno。
    let source = format!(
        "{}[等待][MISSING] {} {} tail\n",
        "*a ".repeat(100),
        "[链接](../target) ".repeat(100),
        "中文😀 ".repeat(16_000)
    );
    let (directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let at = source.find("[等待]").unwrap();
    // 实际窗口确实覆盖这些后继链接；不能指望游标越过原requested_end主动写离屏记录。
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
        // SAFETY: 本--exact子进程只有此测试；getrlimit只向完整有效的栈结构写入。
        unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut original) },
        0
    );
    // SAFETY: SIGXFSZ只在该独占子进程临时忽略，让真实系统写返回EFBIG；不触父进程信号。
    let handler = unsafe { libc::signal(libc::SIGXFSZ, libc::SIG_IGN) };
    assert_ne!(handler, libc::SIG_ERR);
    let limited = libc::rlimit {
        rlim_cur: 0,
        rlim_max: original.rlim_max,
    };
    // SAFETY: 只降低本子进程软限额，硬限额保持原值，所有夹具写入已在此前完成。
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
    // SAFETY: 在任何业务结果断言前恢复原软/硬限制；后继检查、失败日志和资源归还不再受限。
    let restored = unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &original) };
    // SAFETY: handler来自本次signal的原真实返回值，此时唯一受限操作已经终止。
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
