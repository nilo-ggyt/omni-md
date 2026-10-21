//! Track synchronous graph detachment separately from actual pixel return; cached gutter shells still
//! undergo the original standard-control payload checks.

use super::*;
use crate::native_surface::deallocation::NativeMarkdownDeallocationCohort;
use std::cell::OnceCell;

/// Retain the first typed cleanup failure; child-view receipts hold atomic/weak metadata without
/// retaining the graph or document.
#[derive(Debug)]
pub enum NativeMarkdownPagedCloseFailure {
    View(NativeMarkdownPagedTextViewError),
    #[cfg(feature = "native-paged-production")]
    Body(crate::native_surface::paged_body::NativeMarkdownPagedBodyError),
    ViewClose(Rc<NativeMarkdownPagedCloseReceipt>),
}

/// Certifies only this component's actual pixel and gutter-payload return; the controller barrier still
/// verifies the document and remaining Native graph.
#[must_use = "接受拆图不是释放完成；须订阅原收据事件并验证真实资源归还"]
pub struct NativeMarkdownPagedCloseReceipt {
    cohort: RefCell<NativeMarkdownDeallocationCohort>,
    pixels: RefCell<Vec<NativeMarkdownViewportPixelObserver>>,
    #[cfg(feature = "native-paged-production")]
    gutter: OnceCell<crate::native_surface::gutter::paged::PagedGutterRelease>,
    failure: OnceCell<NativeMarkdownPagedCloseFailure>,
    sealed: Cell<bool>,
}

impl std::fmt::Debug for NativeMarkdownPagedCloseReceipt {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("NativeMarkdownPagedCloseReceipt")
            .field("sealed", &self.sealed.get())
            .field("released", &self.cohort.borrow().has_completed())
            .field("failure", &self.failure.get())
            .finish()
    }
}

impl NativeMarkdownPagedCloseReceipt {
    /// Create a receipt with one builder claim; later sealing permits real completion.
    pub(in crate::native_surface) fn new(mtm: MainThreadMarker) -> Rc<Self> {
        Rc::new(Self {
            cohort: RefCell::new(NativeMarkdownDeallocationCohort::new(mtm)),
            pixels: RefCell::new(Vec::new()),
            #[cfg(feature = "native-paged-production")]
            gutter: OnceCell::new(),
            failure: OnceCell::new(),
            sealed: Cell::new(false),
        })
    }

    /// Subscribe to the actual pixel owner once, preserving the original release identity.
    pub(in crate::native_surface) fn track_pixels(
        &self,
        pixels: &std::sync::Arc<
            crate::native_surface::viewport_pixels::NativeMarkdownViewportPixels,
        >,
    ) {
        {
            let mut observers = self.pixels.borrow_mut();
            if observers
                .iter()
                .any(|observer| std::sync::Arc::ptr_eq(&observer.0, pixels))
            {
                return; // Body and gutter may share one page ledger; register each actual release obligation once.
            }
            observers.push(pixels.observer());
        }
        let claim = self.cohort.borrow_mut().resource_claim();
        pixels.observe_release(claim); // Zero-at-registration and final munmap share the original lock, preserving the same actual pixel-release fact.
    }

    /// The gutter has one actual assembly slot; pixels remain in the original cohort, while a typed
    /// weak identity checks cached-shell payload.
    #[cfg(feature = "native-paged-production")]
    pub(in crate::native_surface) fn track_gutter(
        &self,
        gutter: crate::native_surface::gutter::paged::PagedGutterRelease,
    ) {
        self.gutter
            .set(gutter)
            .unwrap_or_else(|_| panic!("同一正文关闭收据只装配一个源码行号"));
    }

    /// Inherit child pixel obligations and its first cleanup failure without retaining its graph.
    pub(in crate::native_surface) fn follow_view(&self, view: Rc<Self>) {
        let observers = view.pixels.borrow().clone();
        for pixels in observers {
            self.track_pixels(&pixels.0);
        } // Body already tracks accepted pixels; inherit only previously unregistered child obligations to avoid counting one release twice.
        if view.failure.get().is_some() {
            self.record(NativeMarkdownPagedCloseFailure::ViewClose(view));
        } // View cleanup already returned; preserve its first typed failure rather than replacing it with a later Input cleanup failure.
    }

    /// Keep the first typed cleanup error for all subsequent verifications.
    pub(in crate::native_surface) fn record(&self, failure: NativeMarkdownPagedCloseFailure) {
        let _ = self.failure.set(failure); // OnceCell retains the first failure; later values follow actual Drop cleanup without replacing that cause.
    }

    /// Close registration once; outstanding real resources alone determine completion.
    pub(in crate::native_surface) fn seal(&self) {
        if !self.sealed.replace(true) {
            self.cohort.borrow_mut().seal();
        } // Repeated close neither returns the builder again nor completes the cohort twice.
    }

    /// Register an original-operation continuation through the cohort, including already-completed
    /// release, without losing the event or retaining business owners.
    pub fn when_released(&self, work: impl FnOnce() + 'static) {
        self.cohort.borrow().when_released(work);
    }

    /// Borrow the original failure read-only so repeated verification cannot consume an error and turn
    /// a failed receipt into success.
    pub fn cleanup_failure(&self) -> Option<&NativeMarkdownPagedCloseFailure> {
        self.failure.get()
    }

    /// The event only permits verification; actual zero, the original cleanup error, and cohort
    /// completion remain separate requirements.
    pub fn verify_released(&self) -> std::result::Result<(), String> {
        self.verify_resources_released()?;
        match self.failure.get() {
            Some(NativeMarkdownPagedCloseFailure::View(error)) => Err(error.to_string()),
            #[cfg(feature = "native-paged-production")]
            Some(NativeMarkdownPagedCloseFailure::Body(error)) => Err(error.to_string()),
            Some(NativeMarkdownPagedCloseFailure::ViewClose(view)) => view.verify_released(),
            None => Ok(()),
        }
    }

    /// The fault-control path separately verifies the original Source failure; this method proves
    /// physical return without clearing recorded cleanup failures.
    pub fn verify_resources_released(&self) -> std::result::Result<(), String> {
        if !self.cohort.borrow().has_completed() {
            return Err("原正文组件的资源释放尚未完成".into());
        }
        for pixels in &*self.pixels.borrow() {
            if !pixels.is_released() {
                return Err(format!("原正文像素没有真实归还：{:?}", pixels.snapshot()));
            }
        }
        #[cfg(feature = "native-paged-production")]
        if let Some(gutter) = self.gutter.get() {
            gutter.verify_released()?;
        } // After actual pixel return, surviving framework shells must still contain no project payload, callback, or layer relationship.
        Ok(())
    }
}
