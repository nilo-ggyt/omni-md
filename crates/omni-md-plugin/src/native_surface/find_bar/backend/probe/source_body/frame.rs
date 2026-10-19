//! 单次真实完整CPU帧通知驱动原三秒deadline；Ready或队列执行不算图像完成。

use super::*;

pub(super) struct Frame {
    started: Instant,
    loop_: CFRetained<CFRunLoop>,
    waiting: Cell<bool>,
    published: RefCell<Option<(NativeMarkdownPagedSceneKey, usize, Duration)>>,
}

impl NativeMarkdownPagedBodyPaintObserver for Frame {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        assert!(self.published.borrow().is_none(), "原帧只能交付一次");
        *self.published.borrow_mut() = Some((key.clone(), paints, self.started.elapsed()));
        if self.waiting.get() {
            self.loop_.stop(); // 只停止这次测试runloop，不调用应用退出或增添产品快捷键。
        }
    }
}

/// 同一个真实帧的时钟与runloop装配；特殊动作错误观察仍只注册一次原帧通知。
pub(super) fn pending() -> Result<Rc<Frame>, String> {
    Ok(Rc::new(Frame {
        started: Instant::now(),
        loop_: CFRunLoop::current().ok_or("没有原主runloop")?,
        waiting: Cell::new(false),
        published: RefCell::new(None),
    }))
}

pub(super) fn observe(body: &NativeMarkdownPagedBody) -> Result<Rc<Frame>, String> {
    let frame = pending()?;
    let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = frame.clone();
    body.observe_next_cpu_frame(Rc::downgrade(&erased))
        .map_err(|cause| cause.to_string())?;
    Ok(frame)
}

pub(super) fn wait_frame(
    stage: &str,
    body: &NativeMarkdownPagedBody,
    frame: Rc<Frame>,
) -> Result<NativeMarkdownPagedBodyProgress, String> {
    wait(stage, body, frame, false)
}

/// 真EOF导航必须确认唯一Composer已complete；这不是放宽原Find前缀门或延长其deadline。
pub(super) fn wait_complete_frame(
    stage: &str,
    body: &NativeMarkdownPagedBody,
    frame: Rc<Frame>,
) -> Result<NativeMarkdownPagedBodyProgress, String> {
    wait(stage, body, frame, true)
}

fn wait(
    stage: &str,
    body: &NativeMarkdownPagedBody,
    frame: Rc<Frame>,
    expected_complete: bool,
) -> Result<NativeMarkdownPagedBodyProgress, String> {
    if frame.published.borrow().is_none() {
        frame.waiting.set(true);
        if let Some(remaining) = Duration::from_secs(3).checked_sub(frame.started.elapsed()) {
            // SAFETY: 同步动作也计入原三秒；这里只等待剩余时间，真实完整帧提前停止原loop。
            unsafe {
                CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), false)
            };
        }
        frame.waiting.set(false);
    }
    if let Some(cause) = body.take_failure() {
        return Err(format!("{stage}: 原Body失败：{cause}"));
    }
    let view = body.view().map_err(|cause| cause.to_string())?;
    let prior_pending = match view.take_failure() {
        Some(ViewError::Pending) => true, // 首个Ready之前的暂态通知不能覆盖随后必须实证的完整帧。
        Some(cause) => return Err(format!("{stage}: 原View失败：{cause}")),
        None => false,
    };
    let progress = body.progress().map_err(|cause| cause.to_string())?;
    println!(
        "native_body_find stage={stage} progress={progress:?} actual_elapsed_ms={:.3} original_frame_event={} consumed_prior_view_pending={prior_pending} deadline_seconds=3 formal=false whole_product=false",
        frame.started.elapsed().as_secs_f64() * 1000.0,
        frame.published.borrow().is_some()
    );
    let (key, paints, elapsed) = frame
        .published
        .borrow_mut()
        .take()
        .ok_or_else(|| format!("{stage}: 原三秒内没有真实完整帧通知"))?;
    let actual = view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if !actual.same(&key)
        || view.cpu_paints() != Some(paints)
        || view.cpu_paint_pending() != Some(false)
        || progress.maximum_step_bytes > 4096
        || progress.maximum_step_scalars > 256
        || progress.maximum_step_metadata_units > 256
        || progress.complete != expected_complete
    {
        return Err(format!("{stage}: 完整帧原票或有界工作量不符"));
    }
    println!(
        "native_body_find stage={stage} actual_action_to_frame_ms={:.3} actual_cpu_paints={paints} original_key_verified=true formal=false",
        elapsed.as_secs_f64() * 1000.0
    );
    metrics::sample(stage)?; // 原帧timestamp和完整票据门已固定；采样不增加原等待配额。
    if elapsed > Duration::from_secs(3) {
        return Err(format!(
            "{stage}: 真实动作到完整帧耗时{:.3}ms，超过原3000ms门",
            elapsed.as_secs_f64() * 1000.0
        ));
    }
    Ok(progress)
}
