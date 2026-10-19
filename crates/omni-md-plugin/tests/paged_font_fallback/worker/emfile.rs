//! 仅由显式--trial-emfile子进程入口执行；实际FD上限故障不影响默认回退或内存工作负载。
use super::*;

struct FileLimit {
    original: libc::rlimit,
    restored: bool,
}
impl FileLimit {
    fn deny_new_descriptors() -> Self {
        let mut original = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: getrlimit公开ABI只写入一个正确对齐的完整rlimit；成功前不读取未初始化字段。
        let result = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, original.as_mut_ptr()) };
        assert_eq!(result, 0);
        // SAFETY: 上方实际getrlimit成功，两个rlim_t字段均由内核完整初始化。
        let original = unsafe { original.assume_init() };
        let limited = libc::rlimit {
            rlim_cur: 0,
            rlim_max: original.rlim_max,
        };
        // SAFETY: 独立子进程内保留真实hardlimit，仅把softlimit降零；原有效FD不会被关闭。
        let result = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limited) };
        assert_eq!(result, 0);
        Self {
            original,
            restored: false,
        }
    }
    fn restore(mut self) {
        // SAFETY: 使用本子进程实际取得的原值，hardlimit没有下降，原softlimit仍可恢复。
        let result = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.original) };
        self.restored = result == 0;
        assert_eq!(result, 0, "实际恢复FD上限失败，不能继续其他控制");
    }
}
impl Drop for FileLimit {
    fn drop(&mut self) {
        if !self.restored {
            // SAFETY: 仅异常退出的补偿；同一个原值仍有效，不修改任何已有FD或外部进程。
            let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.original) };
        } // 正常路径必须经过上方有断言的restore；Drop补偿本身不签恢复成功。
    }
}

// 原完整GCB必须足够触发真实分页spill；单个ASCII现在合法驻留64KiB工作块，不能再假设首字会开FD。
fn spill_fixture() -> Fixture {
    use std::io::Write;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("字体分页真实spill字素.md");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"a").unwrap();
    let mut block = [0; 4096];
    for scalar in block.as_chunks_mut::<2>().0 {
        scalar.copy_from_slice(&[0xcc, 0x81]); // U+0301连续附着原Latin起点，整份候选始终是同一个真实GCB。
    }
    for _ in 0..64 {
        file.write_all(&block).unwrap(); // 只保固定4KiB生成块，不构造整篇或整GCB的String。
    }
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let graphemes = MarkdownGraphemeJob::new(source.clone()).unwrap();
    let scripts = MarkdownScriptJob::new(
        source.clone(),
        &authority,
        MarkdownScriptCancellation::new(),
    )
    .unwrap();
    Fixture {
        _directory: directory,
        document,
        source,
        authority,
        graphemes,
        scripts,
    }
}

// 同一个真实worker先在内核FD拒绝下失败，再归还全部实际分页资源；不构造Result或资源快照。
fn failed_trial(packet: NativeMarkdownFontTrial) -> NativeMarkdownFontTrialResult {
    std::thread::spawn(move || {
        let mut worker = packet.into_worker();
        let observer = worker.resource_observer().unwrap();
        assert_eq!(observer.snapshot().files_created, 0);
        let cancellation = worker.cancellation();
        let limit = FileLimit::deny_new_descriptors();
        let input = MarkdownShapingBudget {
            bytes: 1,
            scalars: 1,
        }; // 保留原故障入口的单步预算，只让同一个真实Job续推到spill，不扩大一个量子。
        let mut failed_input = None;
        while worker.status() == NativeMarkdownFontTrialStatus::Ingesting {
            let step = worker.step(NativeMarkdownFontTrialBudget {
                input,
                ..zero()
            });
            assert_eq!(step.shape_calls, 0); // 故障必须来自原输入跨工作块的真实开文件，不调用完整shape制造旁路。
            match step.input {
                NativeMarkdownFontTrialInputProgress::Counted { bytes, scalars } => {
                    assert!(bytes > 0 && bytes <= input.bytes && scalars <= input.scalars);
                } // 每次真实有界输入都有进展；长度尚未完成时保同一个Job继续，不扩大单步budget。
                failed @ NativeMarkdownFontTrialInputProgress::Failed { .. } => {
                    failed_input = Some(failed);
                }
                NativeMarkdownFontTrialInputProgress::NotAttempted => {
                    panic!("原健康Ingesting试次没有消费本步输入")
                }
            }
        }
        limit.restore(); // 原Core文件创建调用结束后立即恢复，后继断言不在FD受限状态运行。
        assert_eq!(worker.status(), NativeMarkdownFontTrialStatus::Finished);
        assert_eq!(
            failed_input,
            Some(NativeMarkdownFontTrialInputProgress::Failed {
                byte_limit: input.bytes.min(input.scalars),
                scalar_limit: input.scalars.min(input.bytes),
            })
        );
        cancellation.request(); // 后来的真实取消不能覆盖已经观察到的EMFILE首因。
        let result = worker.into_result();
        let first = observer
            .snapshot()
            .first_error
            .expect("真实CreateFile失败必须留下原errno");
        assert_eq!(
            first.operation,
            omni_markdown_core::MarkdownShapingIoOperation::CreateFile
        );
        assert_eq!(first.errno, libc::EMFILE);
        assert!(observer.snapshot().peak_work_bytes >= 64 * 1024);
        assert_eq!(observer.snapshot().work_bytes, 0); // 两个原数组可能各有工作块；真实Drop必须一并归还。
        assert!(
            matches!(result.outcome(), Err(NativeMarkdownFontFallbackError::GlyphStoreIo(error)) if *error == first)
        );
        assert_eq!(result.resources().unwrap().first_error, Some(first));
        released(&observer);
        result
    })
    .join()
    .unwrap()
}

// 在同一个已完成试次上追加原Source的真实回收路径失败，核retire与外层wrapper都保留两项因果。
#[cfg(feature = "native-source-reclaim-probe")]
fn original_reclaim_preserves_finished_trial_error(mtm: MainThreadMarker) {
    use omni_markdown_core::MarkdownDocumentBufferError;

    let mut fixture = spill_fixture();
    let source = fixture.source.resource_observer();
    let graphemes = fixture.graphemes.resource_observer();
    let scripts = fixture.scripts.resource_observer();
    let mapping = fixture.document.original_backing_observer().unwrap();
    let font = base(mtm);
    let (mut fallback, packet) = issued(&mut fixture, &font, mtm);
    let result = failed_trial(packet);
    let glyph_error = result.resources().unwrap().first_error.unwrap();
    // 重新填满原64KiB驻留目录，再读取远端未驻留页；单标量写入sink强制实际原piece借用，
    // 不把节点摘要的端点换算误当成页访问。现有probe只在真实msync成功后注入EIO。
    for offset in (1..64 * 1024).step_by(4096) {
        fixture
            .source
            .write_utf16_range_to(offset..offset + 1, &mut std::io::sink())
            .unwrap();
    }
    assert_eq!(mapping.managed_resident_bytes(), 64 * 1024);
    let before = (mapping.reclaim_attempts(), mapping.reclaim_successes());
    mapping.fail_next_reclaim_for_test();
    assert_eq!(
        fixture
            .source
            .write_utf16_range_to(96 * 1024 + 1..96 * 1024 + 2, &mut std::io::sink()),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(mapping.reclaim_attempts() > before.0 && mapping.reclaim_successes() > before.1);
    assert_eq!(mapping.first_errno(), Some(libc::EIO));
    let entered = std::cell::Cell::new(false);
    let error = fallback
        .complete_trial(
            &font,
            &font.core_font().authority(),
            &mut fixture.graphemes,
            &mut fixture.scripts,
            result,
            &|| {
                entered.set(true);
                false
            },
        )
        .err()
        .expect("同一Result的两个实际I/O失败都必须保留");
    let NativeMarkdownFontFallbackError::TrialCleanup { cause, cleanup } = error else {
        panic!("不能把双错降级成单一Source或普通取消：{error:?}")
    };
    assert!(matches!(
        *cause,
        NativeMarkdownFontFallbackError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        )
    ));
    assert!(matches!(
        *cleanup,
        NativeMarkdownFontFallbackError::GlyphStoreIo(error) if error == glyph_error
    )); // 严格要求原typed资源次因，不能重复包装Source或只查错误字符串。
    assert!(!entered.get()); // 已发生的源I/O首错优先于current=false，不能靠回调制造该错误。
    fallback.close();
    drop(fixture);
    assert!(source.payload_released());
    let graphemes = graphemes.snapshot();
    assert!(graphemes.metadata_owner_released && graphemes.source_payload_released);
    assert!(!graphemes.metadata_file_open && !graphemes.cleanup_failed);
    assert_eq!(graphemes.metadata_map_bytes, 0);
    let scripts = scripts.snapshot();
    assert!(!scripts.run_file_open && !scripts.stack_file_open && !scripts.cleanup_failed);
    assert_eq!(scripts.mapped_bytes, 0);
    assert!(scripts.source_payload_released);
    assert!(mapping.unmap_succeeded() && mapping.failed());
    assert_eq!(mapping.first_errno(), Some(libc::EIO));
    assert_eq!(mapping.mapped_bytes(), 0);
    assert_eq!(mapping.managed_resident_bytes(), 0);
    let font_bytes = font.bytes_observer();
    drop(font);
    assert!(font_bytes.mapping_drop_completed() && font_bytes.unmap_succeeded());
    assert_eq!(font_bytes.mapped_bytes(), 0);
    assert!(font_bytes.source_descriptor_closed() && font_bytes.backing_descriptor_closed());
    assert!(!font_bytes.failed());
    println!(
        "paged_font_trial source_eio_preserves_same_trial_emfile=true source_primary=true typed_cleanup_exact=true original_reclaim_succeeded=true actual_source_unmapped=true actual_stores_released=true kernel_eio_claim=false formal_memory=false"
    );
}

pub(in super::super) fn verify(mtm: MainThreadMarker) {
    // 原正常结果消费和已撤权结果退休都执行同一真实EMFILE负控；每次完全释放后才开始下一次。
    // 后者必须先消费原结果的实际资源首错，不能在guard返回Revoked时把它直接丢掉。
    for current in [true, false] {
        let mut fixture = spill_fixture();
        let font = base(mtm);
        let (mut fallback, packet) = issued(&mut fixture, &font, mtm);
        let result = failed_trial(packet);
        assert!(matches!(
            fallback.complete_trial(&font, &font.core_font().authority(), &mut fixture.graphemes, &mut fixture.scripts, result, &|| current),
            Err(NativeMarkdownFontFallbackError::GlyphStoreIo(error)) if error.errno == libc::EMFILE
        ));
        let source = fixture.source.resource_observer();
        let mapping = fixture.document.original_backing_observer().unwrap();
        fallback.close();
        drop(fixture);
        assert!(source.payload_released() && mapping.unmap_succeeded() && !mapping.failed());
        assert_eq!(mapping.mapped_bytes(), 0);
        println!(
            "paged_font_trial isolated_emfile=true pid={} original_errno={} current={current} retired_result_preserves_io=true later_cancel_preserves_first_error=true failed_input_count_unknown=true actual_stores_released=true source_actual_unmapped=true formal_memory=false",
            std::process::id(),
            libc::EMFILE
        );
    }
    #[cfg(feature = "native-source-reclaim-probe")]
    original_reclaim_preserves_finished_trial_error(mtm);
}
