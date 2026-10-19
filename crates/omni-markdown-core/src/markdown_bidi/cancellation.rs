//! 一次分析及其全部元数据查询共享同一取消位；Core不创建线程、定时器或Native回调。

use super::BidiError;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
/// 只允许false→true的取消权限；Clone共享原位，不能使已取消的分析重新可用。
pub struct MarkdownBidiCancellation {
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    pause: Option<Arc<TestPause>>,
}

impl MarkdownBidiCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 发出永久取消；同步文件调用不能被打断，返回后会在原固定检查点观察该位。
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(super) fn check(&self) -> Result<(), BidiError> {
        if self.is_cancelled() {
            Err(BidiError::Cancelled)
        } else {
            Ok(())
        }
    }

    #[cfg(all(test, feature = "paged-layout-breaks"))]
    pub(super) fn pause_after_first_page() -> (Self, Arc<TestPause>) {
        let pause = Arc::new(TestPause {
            entered: std::sync::Barrier::new(2),
            resume: std::sync::Barrier::new(2),
            file: std::sync::Mutex::new(None),
        });
        (
            Self {
                cancelled: Arc::new(AtomicBool::new(false)),
                pause: Some(Arc::clone(&pause)),
            },
            pause,
        )
    }

    #[cfg(test)]
    pub(super) fn first_page_allocated(&self, observer: super::store::FileObserver) {
        if let Some(pause) = &self.pause {
            // 此点只在真实第一份16KiB页和匿名FD均已存在后进入，不按sleep猜测分析进度。
            *pause.file.lock().expect("synthetic operation gate") = Some(observer);
            pause.entered.wait();
            pause.resume.wait();
        }
    }
}

#[cfg(test)]
pub(super) struct TestPause {
    pub(super) entered: std::sync::Barrier,
    pub(super) resume: std::sync::Barrier,
    pub(super) file: std::sync::Mutex<Option<super::store::FileObserver>>,
}
