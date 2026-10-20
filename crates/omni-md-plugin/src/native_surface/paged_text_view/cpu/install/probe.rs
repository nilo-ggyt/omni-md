//! 实际未发布安装的撤权控制；只自建短合成文稿与无窗口DirectView，不给外部对象故障注入权。

use super::*;
use objc2::rc::{Weak as NativeWeak, autoreleasepool};
use objc2_foundation::NSSize;
use omni_markdown_core::MarkdownDocumentBuffer;
use std::io::Write;

type Observed = Vec<NativeWeak<AnyObject>>;
struct Owner {
    active: Cell<bool>,
    fired: Cell<bool>,
    inspecting: Cell<bool>,
    reparent: bool,
    successor: Retained<CALayer>,
    watched: RefCell<Option<Rc<NativeWeak<NativeMarkdownPagedTextView>>>>,
    observed: RefCell<Observed>,
}
impl NativeMarkdownPagedTextViewDelegate for Owner {
    fn current(&self, epoch: u64) -> bool {
        if epoch != 73 || !self.active.get() {
            return false;
        }
        let Ok(_inspecting) = PaintUse::enter(&self.inspecting) else {
            return self.active.get(); // Nativegetter重入只读原活动位，不递归抓层，也不伪造另一条前台票。
        };
        let weak = self.watched.borrow().clone();
        if !self.fired.get()
            && let Some(view) = weak.and_then(|weak| weak.load())
            && let Some(backing) = view.layer()
        {
            // SAFETY: 这是本控制新建的DirectView，唯一子层只能来自正在执行的真实CPU安装。
            let children = unsafe { backing.sublayers() };
            if let Some(children) = children
                && children.len() == 1
            {
                let mount = children.objectAtIndex(0);
                let receipts = [layer_weak(&backing), layer_weak(&mount)];
                self.observed.borrow_mut().extend(receipts);
                self.fired.set(true);
                if self.reparent {
                    self.successor.addSublayer(&mount); // 真实移到不同owner；回滚不得再移除或清它的关系。
                }
                self.active.set(false); // 唯一故障发生在addSublayer已经真实完成后的原current回调。
            }
        }
        self.active.get()
    }
    fn virtual_viewport(&self, _: u64, _: f64) -> Result<NativeMarkdownPagedVirtualViewport> {
        Err(NativeMarkdownPagedTextViewError::Pending)
    }
    fn ready_scene(
        &self,
        _: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>> {
        Err(NativeMarkdownPagedTextViewError::Pending)
    }
    fn scene_current(&self, _: &NativeMarkdownPagedSceneKey) -> bool {
        false
    }
    fn committed(&self, _: &NativeMarkdownPagedInputCommit) -> Result<()> {
        Err(NativeMarkdownPagedTextViewError::Pending)
    }
    fn reveal_committed_selection(&self, _: &NativeMarkdownPagedInputCommit) -> Result<()> {
        // 本有限组件没有正文Producer/reveal续体，不能把选择提交伪装成实际可见。
        Err(NativeMarkdownPagedTextViewError::Pending)
    }
    fn copy_selection(&self, _: &NativeMarkdownPagedSceneRequest) -> Result<()> {
        Err(NativeMarkdownPagedTextViewError::Pending)
    }
}

/// 只运行两个有限自有图；失败仍清理并先发布真实资源事实，不增菜单、按键或产品窗口。
pub(in crate::native_surface::paged_text_view) fn run(mtm: MainThreadMarker) -> Result<()> {
    for reparent in [false, true] {
        let (passed, observed, document_weak, input_weak, mapping) = autoreleasepool(
            |_| -> Result<_> {
                let mut file = tempfile::NamedTempFile::new()
                    .map_err(|cause| NativeMarkdownPagedTextViewError::Raster(cause.to_string()))?;
                file.write_all(b"finite installation control")
                    .map_err(|cause| NativeMarkdownPagedTextViewError::Raster(cause.to_string()))?;
                let document = Rc::new(RefCell::new(MarkdownDocumentBuffer::open(
                    file.path(),
                    4096,
                )?));
                let mapping = document
                    .borrow()
                    .original_backing_observer()
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
                let document_weak = Rc::downgrade(&document);
                let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document))?;
                let input_weak = Rc::downgrade(&input);
                let owner = Rc::new(Owner {
                    active: Cell::new(true),
                    fired: Cell::new(false),
                    inspecting: Cell::new(false),
                    reparent,
                    successor: CALayer::new(),
                    watched: RefCell::new(None),
                    observed: RefCell::new(Vec::with_capacity(2)),
                });
                let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = owner.clone();
                let view = NativeMarkdownPagedTextView::new(
                    input.clone(),
                    Rc::downgrade(&erased),
                    73,
                    NSRect::new(NSPoint::ZERO, NSSize::new(160.0, 80.0)),
                    mtm,
                )?;
                let context = view
                    .retain_input_context()
                    .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
                let mut observed: Observed = vec![
                    NativeWeak::from(&*view as &AnyObject),
                    NativeWeak::from(&*context as &AnyObject),
                    layer_weak(&owner.successor),
                ];
                *owner.watched.borrow_mut() = Some(Rc::new(NativeWeak::from(&*view)));
                let installed = CpuSurface::install(&view);
                let rejected = matches!(installed, Err(NativeMarkdownPagedTextViewError::Revoked));
                if let Ok(surface) = installed {
                    view.ivars().cpu.replace(Some(surface)); // 非预期成功也交给真实close清理，不能遗留无owner的mount。
                }
                let detached = !view.wantsLayer() && view.layer().is_none();
                let captured = owner.observed.borrow().len() == 2;
                observed.extend(owner.observed.borrow_mut().drain(..));
                let expected_mount = observed.last().and_then(NativeWeak::load);
                let untouched = if reparent {
                    // SAFETY: successor只由本控制持有；仅该回调向它迁入原mount，核数量和精确对象身份。
                    let children = unsafe { owner.successor.sublayers() };
                    children.as_ref().is_some_and(|children| {
                        children.len() == 1
                            && expected_mount.as_deref().is_some_and(|expected| {
                                let actual = children.objectAtIndex(0);
                                std::ptr::eq::<AnyObject>(&*actual as &AnyObject, expected)
                            })
                    })
                } else {
                    owner.successor.superlayer().is_none()
                };
                drop(expected_mount); // 临时Native强claim不能跨关闭后的Weak验收。
                let passed = rejected && detached && untouched && captured && owner.fired.get();
                println!(
                    "paged_cpu_installation reparent={reparent} revoked_after_actual_mount={} returned_revoked={rejected} original_backing_detached={detached} successor_parent_preserved={untouched} observed_mount_backing={captured} formal=false",
                    owner.fired.get()
                );
                // SAFETY: 对照owner自行归还迁入的关系；这不是被测安装guard的回滚动作。
                unsafe { owner.successor.setSublayers(None) };
                let watched = owner.watched.borrow_mut().take();
                drop(watched); // Weak原生注销也在RefCell借用外，不让清理回调重入活动槽。
                let close = view.begin_close()?;
                let arrived = Rc::new(Cell::new(false));
                let release_observed = Rc::clone(&arrived);
                let run_loop = objc2_core_foundation::CFRunLoop::current()
                    .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
                close.when_released(move || {
                    release_observed.set(true);
                    run_loop.stop(); // 原资源事件只结束本次交付，不保任何安装图或文档强边。
                });
                // SAFETY: 仅原main default mode有限3秒；未收到真实事件明确失败，不循环重试。
                unsafe {
                    objc2_core_foundation::CFRunLoop::run_in_mode(
                        objc2_core_foundation::kCFRunLoopDefaultMode,
                        3.0,
                        false,
                    )
                };
                if !arrived.get() {
                    return Err(NativeMarkdownPagedTextViewError::Raster(
                        "原安装资源未交付实际释放事件".into(),
                    ));
                }
                close
                    .verify_released()
                    .map_err(NativeMarkdownPagedTextViewError::Raster)?;
                drop((context, view, input, erased, owner, document, file));
                CATransaction::flush(); // 自有合成图全部归还后提交清空；不轮询Weak，不借后继业务清理旧图。
                Ok((passed, observed, document_weak, input_weak, mapping))
            },
        )?;
        let mut native = true;
        for (role, weak) in observed.into_iter().enumerate() {
            let nil = weak.load().is_none();
            println!(
                "paged_cpu_installation reparent={reparent} release_role={role} actual_weak_nil={nil} formal=false"
            );
            native &= nil;
        }
        let rust = document_weak.upgrade().is_none() && input_weak.upgrade().is_none();
        let unmapped =
            mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed();
        if !passed || !native || !rust || !unmapped {
            return Err(NativeMarkdownPagedTextViewError::Raster(format!(
                "未发布安装控制失败：reparent={reparent} passed={passed} native={native} rust={rust} actual_unmap={unmapped}"
            )));
        }
    }
    Ok(())
}

fn layer_weak(layer: &CALayer) -> NativeWeak<AnyObject> {
    NativeWeak::from(layer as &AnyObject)
}
