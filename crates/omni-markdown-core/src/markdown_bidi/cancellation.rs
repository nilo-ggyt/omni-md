//! One analysis and all its metadata queries share one cancellation bit; Core creates no thread, timer, or Native callback here.

use super::BidiError;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
/// Monotonic false-to-true revocation; clones share the original bit and cannot reactivate a cancelled analysis.
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

    /// Request permanent cancellation. A synchronous file call is not interrupted; the original fixed checkpoint observes the request afterward.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(super) fn check(&self) -> Result<(), BidiError> {
        // This scalar gate only reports revocation; the current Store/worker owner remains responsible for actual resource release.
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
            // The existing test gate is reached only after the actual first 16-KiB page and anonymous FD exist, without sleep-based progress guesses.
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
