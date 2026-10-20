//! 同步拆图与像素真实归还分别记录；框架缓存的行号另按原标准控件空壳合同核实际载荷。

use super::*;
use crate::native_surface::deallocation::NativeMarkdownDeallocationCohort;
use std::cell::OnceCell;

/// 保留第一次清理失败的原typed原因；子视图收据只有原子/Weak元数据，不保原图或文档。
#[derive(Debug)]
pub enum NativeMarkdownPagedCloseFailure {
    View(NativeMarkdownPagedTextViewError),
    #[cfg(feature = "native-paged-production")]
    Body(crate::native_surface::paged_body::NativeMarkdownPagedBodyError),
    ViewClose(Rc<NativeMarkdownPagedCloseReceipt>),
}

/// 只代表本组件实际像素与行号载荷归还；完整文档及其余Native图仍由原控制器barrier核验。
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
                return; // 正文与行号可共享同一页账；一个真实归还义务不能重复登记。
            }
            observers.push(pixels.observer());
        }
        let claim = self.cohort.borrow_mut().resource_claim();
        pixels.observe_release(claim); // 登记已零和最后munmap共用原锁；这里只接纳同一原像素事实。
    }

    /// 行号只有一个真实装配槽；实际像素仍归原cohort，缓存Native空壳由具体typed Weak核载荷。
    #[cfg(feature = "native-paged-production")]
    pub(in crate::native_surface) fn track_gutter(
        &self,
        gutter: crate::native_surface::gutter::paged::PagedGutterRelease,
    ) {
        self.gutter
            .set(gutter)
            .unwrap_or_else(|_| panic!("同一正文关闭收据只装配一个源码行号"));
    }

    pub(in crate::native_surface) fn follow_view(&self, view: Rc<Self>) {
        let observers = view.pixels.borrow().clone();
        for pixels in observers {
            self.track_pixels(&pixels.0);
        } // Body已在接受时登记同一像素；只在调用方尚未登记时接纳子收据，避免重复义务。
        if view.failure.get().is_some() {
            self.record(NativeMarkdownPagedCloseFailure::ViewClose(view));
        } // 原视图清理已经返回；保留其第一次typed错误，后续Input清理失败不能覆盖它。
    }

    pub(in crate::native_surface) fn record(&self, failure: NativeMarkdownPagedCloseFailure) {
        let _ = self.failure.set(failure); // OnceCell只接第一次；后续错误按真实Drop归还，不覆盖首因。
    }

    pub(in crate::native_surface) fn seal(&self) {
        if !self.sealed.replace(true) {
            self.cohort.borrow_mut().seal();
        } // 同一次重复关闭不再归还builder，也不会二次完成。
    }

    /// 只登记一个原操作续体；完成已先发生时也由原cohort交付，不丢事件、不保业务强边。
    pub fn when_released(&self, work: impl FnOnce() + 'static) {
        self.cohort.borrow().when_released(work);
    }

    /// 原原因只读借出，重复核验不能取走错误后把失败收据洗成成功。
    pub fn cleanup_failure(&self) -> Option<&NativeMarkdownPagedCloseFailure> {
        self.failure.get()
    }

    /// 事件只授予核验时机；真实zero、原清理错误和cohort失败仍必须分别通过。
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

    /// 故障注入控制另核原Source首错；这里只证明物理资源归还，不将已保存的清理失败改成成功。
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
        } // 原像素已经实际归还后，仍须证明活着的框架空壳没有项目载荷、回调或图层关系。
        Ok(())
    }
}
