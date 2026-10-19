//! 只为显式updateLayer对照暂时启用原正文layer；关闭时恢复同一原关系。

use super::*;
use objc2_app_kit::NSViewLayerContentsRedrawPolicy;
use objc2_quartz_core::CALayer;

/// 没有原Native强拥有；唯一view槽持有该租约，断图后的release_payload同步取出归还。
pub(in crate::native_surface) struct NativeMarkdownRasterBodyLayerLease {
    view: Weak<PieceBackedMarkdownTextView>,
    previous_layer: Option<Weak<CALayer>>,
    installed_layer: Option<Weak<CALayer>>,
    previous_wants_layer: bool,
    previous_redraw: NSViewLayerContentsRedrawPolicy,
    counters: Arc<ResourceCounters>,
    owner: std::rc::Weak<NativeMarkdownRasterSurface>,
    finished: Cell<bool>,
}

impl NativeMarkdownRasterBodyLayerLease {
    pub(super) fn install(
        view: &PieceBackedMarkdownTextView,
        ledger: &NativeMarkdownOwnedLedger,
        owner: &Rc<NativeMarkdownRasterSurface>,
    ) -> Result<Self, String> {
        let counters = &owner.counters;
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        let current = || {
            !view.ivars().textkit_graph_teardown.get() && !counters.closed.load(Ordering::Acquire)
        };
        let previous_wants_layer = step(&current, || view.wantsLayer())?;
        let previous_redraw = step(&current, || view.layerContentsRedrawPolicy())?;
        let previous_layer = step(&current, || view.layer())?;
        let previous_weak = match previous_layer.as_deref() {
            Some(layer) => Some(step(&current, || Weak::from(layer))?),
            None => None, // 原来没有层是精确事实，不为恢复另造占位层。
        };
        let mut lease = Self {
            view: step(&current, || Weak::from(view))?,
            previous_layer: previous_weak,
            installed_layer: None,
            previous_wants_layer,
            previous_redraw,
            counters: Arc::clone(counters),
            owner: Rc::downgrade(owner),
            finished: Cell::new(false),
        };
        // 只在挂窗之前准备backing；updateLayer中不得创建几何或改变视图树。
        step(&current, || view.setWantsLayer(true))?;
        let layer = step(&current, || view.layer())?.ok_or("正文layer对照未取得真实CALayer")?;
        lease.installed_layer = Some(step(&current, || Weak::from_retained(&layer))?);
        step(&current, || {
            view.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay)
        })?;
        if !previous_layer
            .as_deref()
            .is_some_and(|original| std::ptr::eq::<CALayer>(original, &*layer))
        {
            // 只有本次实际新建的层进入自有角色；恢复原层不是按类型豁免新层的nil门禁。
            step(&current, || {
                ledger.include("editor.raster.body-layer", &layer)
            })??;
        }
        drop(layer);
        drop(previous_layer);
        if !current() {
            return Err("准备正文layer后原构造已撤销".into());
        }
        Ok(lease)
    }

    pub(super) fn installed_layer(&self) -> Option<Retained<CALayer>> {
        self.installed_layer.as_ref().and_then(Weak::load)
    }

    pub(in crate::native_surface) fn restore(&self) -> Result<(), String> {
        if self.finished.replace(true) {
            return Ok(()); // 只是避免重入重复恢复；是否真正释放仍由原ledger与资源收据判定。
        }
        let result = self.restore_once();
        if result.is_err() {
            self.counters.fail();
            // restore_once 的 Native 临时 claim 与状态借用已归还，再通知原主线程 owner。
            if let Some(owner) = self.owner.upgrade() {
                owner.notify_failed_first_paint();
            }
        }
        result
    }

    fn restore_once(&self) -> Result<(), String> {
        let view = self.view.load().ok_or("恢复正文layer时原view已归还")?;
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        // 构造失败可在尚未关闭时回滚；accepted关闭只能在原view已封闭后结束backing。
        let current = || {
            !self.counters.closed.load(Ordering::Acquire)
                || view.ivars().textkit_graph_teardown.get()
        };
        let original = match self.previous_layer.as_ref() {
            Some(weak) => {
                Some(step(&current, || weak.load())?.ok_or("原正文layer已真实nil，不能伪造恢复")?)
            }
            None => None, // 原nil关系在关闭后仍必须恢复为nil，不能留下新建layer空壳。
        };
        let installed = match self.installed_layer.as_ref() {
            Some(weak) => {
                Some(step(&current, || weak.load())?.ok_or("对照正文layer在恢复前已失去原身份")?)
            }
            None => None, // 还未成功切换的构造失败只允许恢复此前原关系。
        };
        let actual = step(&current, || view.layer())?;
        let expected = installed.as_deref().or(original.as_deref());
        if !same_layer(actual.as_deref(), expected) {
            return Err("正文layer已被替换，不覆盖后继Native关系".into());
        }
        if let Some(layer) = actual
            .as_deref()
            .filter(|layer| !same_layer(Some(layer), original.as_deref()))
        {
            // SAFETY: 只清同一原view当前实际CALayer.contents，不触碰任何子层或不可变正文值。
            step(&current, || unsafe { layer.setContents(None) })?;
        }
        step(&current, || view.setWantsLayer(self.previous_wants_layer))?;
        step(&current, || view.setLayer(original.as_deref()))?;
        step(&current, || {
            view.setLayerContentsRedrawPolicy(self.previous_redraw)
        })?;
        if step(&current, || view.wantsLayer())? != self.previous_wants_layer
            || !same_layer(
                step(&current, || view.layer())?.as_deref(),
                original.as_deref(),
            )
            || step(&current, || view.layerContentsRedrawPolicy())? != self.previous_redraw
        {
            return Err("正文layer原关系未精确恢复".into());
        }
        drop(actual);
        drop(installed);
        drop(original);
        if !current() {
            return Err("恢复正文layer临时claim归还后阶段失效".into());
        }
        Ok(())
    }
}

impl Drop for NativeMarkdownRasterBodyLayerLease {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!(
                "native_raster_surface event=body_layer_restore_failed error={error:?} formal=false"
            );
        }
    }
}

fn same_layer(actual: Option<&CALayer>, expected: Option<&CALayer>) -> bool {
    match (actual, expected) {
        (Some(actual), Some(expected)) => std::ptr::eq::<CALayer>(actual, expected),
        (None, None) => true, // 只有两边都确实无层才能匹配原nil关系。
        _ => false,           // 不能以一个无层状态替代仍存活的另一层身份。
    }
}

fn step<T>(current: &impl Fn() -> bool, work: impl FnOnce() -> T) -> Result<T, String> {
    if !current() {
        return Err("正文layer操作前阶段已撤销".into());
    }
    let value = work();
    if !current() {
        return Err("正文layer操作后阶段已撤销".into());
    }
    Ok(value)
}
