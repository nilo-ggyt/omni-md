//! Temporarily enables body backing for the explicit updateLayer comparison and restores its exact relation.

use super::*;
use objc2_app_kit::NSViewLayerContentsRedrawPolicy;
use objc2_quartz_core::CALayer;

/// Holds no strong Native back-link. The view's unique slot returns this lease during payload release,
/// after teardown has sealed the original graph against new work.
pub(in crate::native_surface) struct NativeMarkdownRasterBodyLayerLease {
    view: Weak<PieceBackedMarkdownTextView>,
    previous_layer: Option<Weak<CALayer>>,
    installed_layer: Option<Weak<CALayer>>,
    previous_wants_layer: bool,
    previous_redraw: NSViewLayerContentsRedrawPolicy,
    counters: Arc<ResourceCounters>,
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
            None => None, // An originally absent layer is an exact relation, not a reason to create a placeholder.
        };
        let mut lease = Self {
            view: step(&current, || Weak::from(view))?,
            previous_layer: previous_weak,
            installed_layer: None,
            previous_wants_layer,
            previous_redraw,
            counters: Arc::clone(counters),
            finished: Cell::new(false),
        };
        // Prepare backing before window attachment. updateLayer must not create geometry or mutate
        // the view tree; any failure after this lease is built restores through the same Drop path.
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
            // Record only a layer actually created by this installation. Restoring an existing layer
            // does not exempt the new layer from the ledger's actual-deallocation requirement.
            step(&current, || {
                ledger.include("editor.raster.body-layer", &layer)
            })??;
        }
        drop(layer);
        drop(previous_layer);
        // Returning temporary Native claims may reenter; construction succeeds only if still original.
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
            return Ok(()); // Prevent duplicate reentrant restoration; the ledger/receipts still prove release.
        }
        let result = self.restore_once();
        if result.is_err() {
            self.counters.fail();
            // Retain the failure in the same shared receipt without reviving an obsolete notification link.
        }
        result
    }

    fn restore_once(&self) -> Result<(), String> {
        let view = self.view.load().ok_or("恢复正文layer时原view已归还")?;
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        // Construction can roll back before close. Once close is accepted, restoration is allowed
        // only after the original view has sealed its TextKit graph, preventing new body work.
        let current = || {
            !self.counters.closed.load(Ordering::Acquire)
                || view.ivars().textkit_graph_teardown.get()
        };
        let original = match self.previous_layer.as_ref() {
            Some(weak) => {
                Some(step(&current, || weak.load())?.ok_or("原正文layer已真实nil，不能伪造恢复")?)
            }
            None => None, // Restore original nil exactly; do not leave an empty newly created layer behind.
        };
        let installed = match self.installed_layer.as_ref() {
            Some(weak) => {
                Some(step(&current, || weak.load())?.ok_or("对照正文layer在恢复前已失去原身份")?)
            }
            None => None, // Failure before installation can restore only the previously captured relation.
        };
        let actual = step(&current, || view.layer())?;
        let expected = installed.as_deref().or(original.as_deref());
        // Never overwrite a layer installed by a later Native owner, even while returning this lease.
        if !same_layer(actual.as_deref(), expected) {
            return Err("正文layer已被替换，不覆盖后继Native关系".into());
        }
        if let Some(layer) = actual
            .as_deref()
            .filter(|layer| !same_layer(Some(layer), original.as_deref()))
        {
            // SAFETY: Clear only the actual layer's contents on the original view, without touching
            // sublayers or immutable document values. The layer's CGImage ownership is returned here.
            step(&current, || unsafe { layer.setContents(None) })?;
        }
        step(&current, || view.setWantsLayer(self.previous_wants_layer))?;
        step(&current, || view.setLayer(original.as_deref()))?;
        step(&current, || {
            view.setLayerContentsRedrawPolicy(self.previous_redraw)
        })?;
        // Verify all three public facts rather than treating successful setters as completed restoration.
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
        // Final temporary releases can reenter too; the original restoration phase must still hold.
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
        (None, None) => true, // Only two actual nil values match the original absence of a layer.
        _ => false,           // A missing layer cannot stand in for a different live identity.
    }
}

fn step<T>(current: &impl Fn() -> bool, work: impl FnOnce() -> T) -> Result<T, String> {
    // Restoration and construction use their own original phase predicate on both sides of Native.
    // An operation that revokes the phase cannot authorize the next setter or weak upgrade.
    if !current() {
        return Err("正文layer操作前阶段已撤销".into());
    }
    let value = work();
    if !current() {
        return Err("正文layer操作后阶段已撤销".into());
    }
    Ok(value)
}
