//! 真实循环握手，不用sleep或调用前请求冒充深层协作取消。
use super::*;
use crate::{Buffer, ContentType, Direction, Font, Script, ShapeOptions, ShaperFont};
use std::sync::{atomic::AtomicUsize, mpsc, Barrier};

const ROBOTO: &[u8] = include_bytes!("../../benches/fonts/Roboto-Regular.ttf");

#[test]
fn nested_scope_restores_original_control_and_never_resets_parent_request() {
    let buffer = Buffer::new();
    let observer = buffer.paging_resource_observer();
    let parent = CancellationToken::new();
    let child = CancellationToken::new();
    child.request();
    let result = with_cooperative_control(
        CooperativeControl::new(parent.clone(), observer.clone(), None),
        || {
            assert_eq!(
                with_cooperative_control(
                    CooperativeControl::new(child, observer.clone(), None),
                    || Ok(())
                ),
                Err(ShapeError::Cancelled)
            );
            poll_boundary()?; // 内层撤权不能污染原外层；恢复的是同一个控制而非无条件None。
            parent.request();
            poll_boundary()
        },
    );
    assert_eq!(result, Err(ShapeError::Cancelled));
    assert!(poll_boundary().is_ok()); // 最外层归还后，合法无scope调用不继承前一任务取消。
}

#[test]
fn source_error_has_priority_over_the_same_checkpoint_cancellation() {
    let buffer = Buffer::new();
    let token = CancellationToken::new();
    token.request();
    let health: CooperativeSourceHealth = Arc::new(|_| Some(libc::EIO));
    assert_eq!(
        with_cooperative_control(
            CooperativeControl::new(token, buffer.paging_resource_observer(), Some(health)),
            || Ok(())
        ),
        Err(ShapeError::SourceUnavailable(libc::EIO))
    );
}

#[test]
fn actual_unicode_normalize_gsub_and_position_loops_cancel_without_glyph_publication() {
    for target in [
        CooperativePhase::Unicode,
        CooperativePhase::Normalize,
        CooperativePhase::Substitute,
        CooperativePhase::Position,
    ] {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker = std::thread::spawn(move || {
            // 不能把含Rc/NonNull的Buffer跨线程搬运；真正owner在worker内构造、使用并Drop。
            let font = Font::new(ROBOTO, 0).unwrap();
            let shaper = ShaperFont::new(&font);
            let mut buffer = Buffer::new();
            buffer.set_direction(Direction::LeftToRight);
            buffer.set_script(Some(Script::LATIN));
            for index in 0..16_384 {
                buffer.push(u32::from(if index % 3 == 0 { 'f' } else { 'i' }), index);
            }
            let observer = buffer.paging_resource_observer();
            let token = CancellationToken::new();
            ready_tx.send((token.clone(), observer.clone())).unwrap();
            let entered = AtomicBool::new(false);
            let checks = Arc::new(AtomicUsize::new(0));
            let read_checks = checks.clone();
            let health: CooperativeSourceHealth = Arc::new(move |point| {
                read_checks.fetch_add(1, Ordering::Relaxed);
                if point.phase == target
                    && point.kind == CooperativeCheckpointKind::Loop
                    && !entered.swap(true, Ordering::AcqRel)
                {
                    entered_tx.send(point).unwrap();
                    worker_barrier.wait(); // 原算法已经进入深层循环，父线程此时才请求取消。
                }
                None
            });
            let result = with_cooperative_control(
                CooperativeControl::new(token, observer.clone(), Some(health)),
                || crate::shape(&shaper, &mut buffer, ShapeOptions::new()),
            );
            assert_eq!(result, Err(ShapeError::Cancelled));
            assert_ne!(buffer.content_type(), Some(ContentType::Glyphs));
            assert!(checks.load(Ordering::Acquire) > 1);
            drop(buffer);
            drop(shaper);
            drop(font);
        });
        let (token, observer) = ready_rx.recv().unwrap();
        assert_eq!(entered_rx.recv().unwrap().phase, target);
        token.request();
        barrier.wait();
        worker.join().unwrap(); // join以后核真实FD/munmap收据，不能把request=true当作释放。
        let resources = observer.snapshot();
        assert_eq!(
            (
                resources.files_open,
                resources.mappings_open,
                resources.mapped_bytes
            ),
            (0, 0, 0)
        );
        assert!(resources.files_created > 0 && resources.mappings_created > 0);
        assert_eq!(resources.files_created, resources.files_closed);
        assert_eq!(resources.mappings_created, resources.mappings_released);
        assert!(!resources.cleanup_failed && resources.first_error.is_none());
    }
}

#[test]
fn real_close_then_injected_failure_remains_io_instead_of_cancelled() {
    let mut buffer = Buffer::new();
    // 真正超过固定工作块，才会取得可被归还的原任务FD；小样本不能冒充文件清理。
    let count = 64 * 1024 / core::mem::size_of::<crate::GlyphInfo>() + 1;
    for index in 0..count {
        buffer.push(u32::from('a'), index as u32);
    }
    assert!(buffer.allocation_successful());
    let observer = buffer.paging_resource_observer();
    let active = observer.snapshot();
    assert!(active.files_open > 0 && active.mappings_open > 0);
    assert!(active.mapped_bytes > 64 * 1024);
    assert!(!active.cleanup_failed && active.first_error.is_none());
    // 原FD真实close成功后才注入负面收据，不宣称内核close实际失败；取消不得覆盖首因。
    buffer.info.paging_scope().test_fail_next_close_after_real();
    drop(buffer);
    let ended = observer.snapshot();
    assert_eq!(
        (
            ended.work_bytes,
            ended.files_open,
            ended.mappings_open,
            ended.mapped_bytes
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(ended.files_created, ended.files_closed);
    assert_eq!(ended.mappings_created, ended.mappings_released);
    assert!(ended.cleanup_failed);
    let error = observer.first_error().unwrap();
    assert_eq!(error.operation, crate::PagingOperation::Close);
    assert_eq!(error.errno, libc::EIO);
    let token = CancellationToken::new();
    token.request();
    assert_eq!(
        with_cooperative_control(CooperativeControl::new(token, observer, None), || Ok(())),
        Err(ShapeError::Paging {
            operation: error.operation as u32,
            errno: error.errno
        })
    );
}
