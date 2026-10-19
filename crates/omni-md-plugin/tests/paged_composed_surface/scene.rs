//! 真正组合行窗口的薄Native消费端；回调只消费已有几何和glyph，不推进producer。

use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::{Rc, Weak},
};

use objc2_core_foundation::{CFRetained, CFRunLoop};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSPoint, NSRange};
use omni_markdown_core::MarkdownTextSelection;
use omni_md_plugin::native_surface::{
    paged_input::{NativeMarkdownPagedInput, NativeMarkdownPagedInputCommit},
    paged_line_viewport::{
        NativeMarkdownComposedViewport, NativeMarkdownComposedViewportError,
        NativeMarkdownComposedViewportPaintBudget, NativeMarkdownComposedViewportPaintCause,
        NativeMarkdownComposedViewportPaintError, NativeMarkdownComposedViewportPaintOutcome,
        NativeMarkdownComposedViewportPaintSession,
    },
    paged_text_view::{
        NativeMarkdownPagedCharacterRect, NativeMarkdownPagedEditCommand,
        NativeMarkdownPagedEditPlan, NativeMarkdownPagedNavigation, NativeMarkdownPagedSceneKey,
        NativeMarkdownPagedSceneNonce, NativeMarkdownPagedScenePaintBudget,
        NativeMarkdownPagedScenePaintCompletion, NativeMarkdownPagedScenePaintOutcome,
        NativeMarkdownPagedScenePaintSession, NativeMarkdownPagedScenePaintStep,
        NativeMarkdownPagedScenePaintWork, NativeMarkdownPagedSceneRequest,
        NativeMarkdownPagedTextScene, NativeMarkdownPagedTextViewDelegate,
        NativeMarkdownPagedTextViewError as Error, NativeMarkdownPagedVirtualViewport,
    },
    paged_viewport::{NativeMarkdownPagedViewport, NativeMarkdownPagedViewportGeometry},
};

pub const EPOCH: u64 = 19;
pub const PADDING: f64 = 8.0;

#[derive(Default)]
pub struct PaintFacts {
    pub calls: Cell<usize>,
    pub ink: Cell<usize>,
    pub glyphs: Cell<usize>,
    pub memory_failed: Cell<bool>,
    pub dense: Cell<bool>,
    pub theme: Cell<super::graphics::Theme>,
    pub selected_fill: Cell<usize>,
    pub selected_text: Cell<usize>,
    pub graphics_failed: Cell<bool>,
}

/// 单一scene的实际瓦片完成事实；仅测试大窗等待使用，不向产品增加timer或发布权限。
pub struct FrameProgress {
    expected_tiles: usize,
    completed_tiles: Cell<usize>,
    visible_glyphs: Cell<Option<usize>>,
    wake_armed: Cell<bool>,
    published: Cell<bool>,
}
impl FrameProgress {
    pub fn new(expected_tiles: usize) -> Self {
        assert!((1..=64).contains(&expected_tiles));
        Self {
            expected_tiles,
            completed_tiles: Cell::new(0),
            visible_glyphs: Cell::new(None),
            wake_armed: Cell::new(false),
            published: Cell::new(false),
        }
    }

    pub fn arm(&self) {
        assert_eq!(
            self.completed_tiles.get(),
            0,
            "新scene不应在等待前已完成部分帧"
        );
        self.wake_armed.set(true);
    }

    pub fn complete(&self) -> bool {
        self.completed_tiles.get() == self.expected_tiles && self.published.get()
    }

    pub fn visible_glyphs(&self) -> usize {
        self.visible_glyphs
            .get()
            .expect("只有真实Complete后才读取字形事实")
    }

    fn tile_completed(&self, glyphs: usize) -> Result<(), Error> {
        if self.visible_glyphs.get().is_some_and(|old| old != glyphs) {
            return Err(Error::InvalidGeometry);
        } // 每块context消费相同原视口字形；不能把跨tile的重复提交累加成可见字形数量。
        self.visible_glyphs.set(Some(glyphs));
        let completed = self
            .completed_tiles
            .get()
            .checked_add(1)
            .ok_or(Error::CounterExhausted)?;
        if completed > self.expected_tiles {
            return Err(Error::InvalidGeometry);
        } // 此scene只允许一份候选整帧；新nonce使用独立事实槽，不借历史完成数补签。
        self.completed_tiles.set(completed);
        // 最后字形只完成原 paint session；封存、校准与原子发布仍可能位于后继主队列轮次。
        // 这里只记真实 tile 数，不能提前停止等待并把半成品签成完整帧。
        Ok(())
    }
}

pub struct Scene {
    pub key: NativeMarkdownPagedSceneKey,
    pub _nonce: NativeMarkdownPagedSceneNonce,
    pub geometry: NativeMarkdownPagedViewportGeometry,
    pub window: Rc<NativeMarkdownComposedViewport>,
    pub owner: Weak<Owner>,
    pub paints: Rc<PaintFacts>,
    pub progress: FrameProgress,
    pub ranges: Box<[Range<usize>]>,
    pub selection: Option<Range<usize>>,
}
impl Scene {
    pub fn current(&self) -> bool {
        self.owner
            .upgrade()
            .is_some_and(|owner| owner.scene_current(&self.key))
    }
}

pub fn error(cause: NativeMarkdownComposedViewportError) -> Error {
    match cause {
        NativeMarkdownComposedViewportError::Pending => Error::Pending,
        NativeMarkdownComposedViewportError::Revoked => Error::Revoked,
        NativeMarkdownComposedViewportError::Composed(
            omni_markdown_core::MarkdownLineComposeError::Source(cause),
        ) => Error::Document(cause), // 原Source首错不能被几何或尚未Ready包装抹掉。
        // 有界组件以外的错误必须拒绝，输出具体原错方便定位，不能夹成一个成功坐标。
        other => {
            eprintln!("paged_composed_surface viewport_error={other:?}");
            Error::InvalidGeometry
        }
    }
}

fn paint_error(cause: NativeMarkdownComposedViewportPaintError) -> Error {
    match cause.cause {
        NativeMarkdownComposedViewportPaintCause::Window(cause) => error(cause),
        other => Error::Raster(other.to_string()),
    }
}

impl NativeMarkdownPagedTextScene for Scene {
    fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }
    fn cpu_frame_published(&self, paints: usize) {
        if !self.current()
            || paints == 0
            || self.progress.completed_tiles.get() != self.progress.expected_tiles
            || self.progress.published.replace(true)
        {
            self.paints.graphics_failed.set(true);
            return; // 异源、重复或不完整交付不能补签原完成事实。
        }
        if self.progress.wake_armed.replace(false)
            && let Some(run_loop) = CFRunLoop::current()
        {
            // 既有真实完成 hook 在原完整帧提交后才调用；不增加 timer、轮询或更长 deadline。
            run_loop.stop();
        }
    }
    fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<(), Error> {
        self.window
            .validate_ready(&|| current() && self.current())
            .map_err(error)
    }
    fn first_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect, Error> {
        self.window
            .first_rect(range, &|| self.current())
            .map_err(error)
    }
    fn hit(&self, local: NSPoint) -> Result<usize, Error> {
        Ok(self
            .window
            .hit(local, &|| self.current())
            .map_err(error)?
            .source_utf16)
    }
    fn mouse_selection(
        &self,
        _local: NSPoint,
        _clicks: usize,
        _anchor: Option<usize>,
        _extend: bool,
    ) -> Result<MarkdownTextSelection, Error> {
        Err(Error::Pending) // 此组件没有生产选择方向，不拿最近caret伪造真实选择。
    }
    fn navigate(
        &self,
        _action: NativeMarkdownPagedNavigation,
        _extend: bool,
        _context: &omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedNavigationContext,
    ) -> Result<
        omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedNavigationTarget,
        Error,
    > {
        Err(Error::Pending) // 尚未总装的导航能力与实际绘图成功分开报告。
    }
    fn edit_plan(
        &self,
        _action: NativeMarkdownPagedEditCommand,
    ) -> Result<NativeMarkdownPagedEditPlan, Error> {
        Err(Error::Pending) // 不新增编辑语义或快捷键，也不修改用户文稿。
    }
    fn begin_cpu_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> Result<Box<dyn NativeMarkdownPagedScenePaintSession>, Error> {
        let valid = || current() && self.current();
        self.validate_ready(&valid)?;
        let background = super::graphics::begin(&self, &context, &valid)?;
        let session = Rc::clone(&self.window)
            .begin_paint(context.clone(), &valid)
            .map_err(paint_error)?;
        Ok(Box::new(PaintSession {
            scene: self,
            context,
            session,
            background,
        }))
    }
    fn paint(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<(), Error> {
        let valid = || current() && self.current();
        if !valid() {
            return Err(Error::Revoked);
        }
        // 真实CPU候选先填原主题及真实选区，再绘当前窗口字形；失败候选不进入View发布收据。
        let background = super::graphics::begin(self, context, &valid)?;
        let painted = self.window.paint(context, &valid).map_err(|cause| {
            eprintln!("paged_composed_surface painter_error={cause:?}");
            Error::InvalidGeometry
        })?;
        if painted.glyphs == 0 || painted.glyphs > 4096 || painted.missing_glyphs != 0 {
            return Err(Error::InvalidGeometry);
        } // 原painter返回也须核非空和缺字，不能以空context冒充正文。
        let actual = background.observe(context, &valid)?;
        self.paints.ink.set(self.paints.ink.get() + actual.ink);
        self.paints
            .selected_fill
            .set(self.paints.selected_fill.get() + actual.selected_fill);
        self.paints
            .selected_text
            .set(self.paints.selected_text.get() + actual.selected_text);
        self.paints
            .glyphs
            .set(self.paints.glyphs.get() + painted.glyphs);
        self.paints.calls.set(self.paints.calls.get() + 1);
        let memory = crate::metrics::sample("composed_actual_draw", self.geometry.content_height);
        if !memory.process_within_bound || !(0..100_000_000).contains(&memory.graphics) {
            self.paints.memory_failed.set(true);
        } // 失败保持sticky，仍继续真实关闭后再统一拒收。
        if !valid() {
            return Err(Error::Revoked);
        }
        Ok(())
    }
}

/// 此适配器只接真实组合窗口的消费式游标，不能调用旧同步paint绕过每轮预算。
struct PaintSession {
    session: NativeMarkdownComposedViewportPaintSession,
    scene: Rc<Scene>,
    context: CFRetained<CGContext>,
    background: super::graphics::Sample,
}
impl NativeMarkdownPagedScenePaintSession for PaintSession {
    fn step(
        self: Box<Self>,
        budget: NativeMarkdownPagedScenePaintBudget,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedScenePaintStep, Error> {
        let Self {
            session,
            scene,
            context,
            background,
        } = *self;
        let valid = || current() && scene.current();
        let step = session.step(
            NativeMarkdownComposedViewportPaintBudget {
                glyphs: budget.glyphs,
                metadata_records: budget.metadata_records,
            },
            &valid,
        );
        scene.validate_ready(&valid)?; // 原Source健康必须优先于Native或权限拒绝，且借用不跨回调。
        let step = step.map_err(paint_error)?;
        let work = NativeMarkdownPagedScenePaintWork {
            glyphs: step.work.glyphs,
            batches: step.work.batches,
            metadata_records: step.work.metadata_records,
        };
        let outcome = match step.outcome {
            NativeMarkdownComposedViewportPaintOutcome::Pending(session) => {
                NativeMarkdownPagedScenePaintOutcome::Pending(Box::new(Self {
                    session,
                    scene,
                    context,
                    background,
                }))
            }
            NativeMarkdownComposedViewportPaintOutcome::Complete(completion) => {
                let receipt = completion.receipt();
                if receipt.glyphs == 0 || receipt.missing_glyphs != 0 {
                    return Err(Error::InvalidGeometry);
                }
                let actual = background.observe(&context, &valid)?;
                scene.paints.ink.set(scene.paints.ink.get() + actual.ink);
                scene
                    .paints
                    .selected_fill
                    .set(scene.paints.selected_fill.get() + actual.selected_fill);
                scene
                    .paints
                    .selected_text
                    .set(scene.paints.selected_text.get() + actual.selected_text);
                scene
                    .paints
                    .glyphs
                    .set(scene.paints.glyphs.get() + receipt.glyphs);
                scene.paints.calls.set(scene.paints.calls.get() + 1);
                let memory =
                    crate::metrics::sample("composed_actual_draw", scene.geometry.content_height);
                if !memory.process_within_bound || !(0..100_000_000).contains(&memory.graphics) {
                    scene.paints.memory_failed.set(true);
                } // 保留真实累计峰值，失败仍要实际close后统一拒收。
                scene.validate_ready(&valid)?;
                let completion = NativeMarkdownPagedScenePaintCompletion::from_composed(
                    completion, &scene.key, &context,
                )?;
                scene.progress.tile_completed(receipt.glyphs)?;
                NativeMarkdownPagedScenePaintOutcome::Complete(completion)
            }
        };
        Ok(NativeMarkdownPagedScenePaintStep { work, outcome })
    }
}

pub struct Owner {
    pub active: Cell<bool>,
    pub revoke_after_mount: Cell<bool>,
    pub revoke_fired: Cell<bool>,
    pub inspecting_mount: Cell<bool>,
    pub old_group: Cell<usize>,
    pub watched: RefCell<
        Option<
            objc2::rc::Weak<
                omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedTextView,
            >,
        >,
    >,
    pub viewport: RefCell<Weak<NativeMarkdownPagedViewport>>,
    pub input: Weak<NativeMarkdownPagedInput>,
    pub scene: RefCell<Option<Rc<Scene>>>,
}
impl NativeMarkdownPagedTextViewDelegate for Owner {
    fn current(&self, epoch: u64) -> bool {
        if epoch != EPOCH || !self.active.get() {
            return false;
        }
        if self.revoke_after_mount.get() && !self.inspecting_mount.replace(true) {
            let watched = self.watched.borrow().clone(); // Native Weak读取也必须在槽借用结束以后。
            if let Some(view) = watched.and_then(|watched| watched.load())
                && let Some(backing) = view.layer()
            {
                // SAFETY: 本harness CPU独占此原生backing；只读原有限group关系，不运行布局。
                let mounts = unsafe { backing.sublayers() };
                if let Some(mounts) = mounts
                    && mounts.len() == 1
                {
                    let mount = mounts.objectAtIndex(0);
                    // SAFETY: mount来自真实CPU安装，只可能持有本harness有限group。
                    let groups = unsafe { mount.sublayers() };
                    if let Some(groups) = groups
                        && groups.len() == 1
                    {
                        let group = groups.objectAtIndex(0);
                        if std::ptr::from_ref(&*group) as usize != self.old_group.get() {
                            // SAFETY: 新完整组已由唯一mount setter提交；候选填槽时还没有这条挂载关系。
                            let slots = unsafe { group.sublayers() };
                            if slots.as_ref().is_some_and(|slots| !slots.is_empty()) {
                                self.revoke_fired.set(true);
                                self.active.set(false); // 只在实际提交后撤原Owner权限，不伪造Source EIO。
                            }
                        }
                    }
                }
            }
            self.inspecting_mount.set(false);
        }
        self.active.get()
    }
    fn virtual_viewport(&self, epoch: u64) -> Result<NativeMarkdownPagedVirtualViewport, Error> {
        if !self.current(epoch) {
            return Err(Error::Revoked);
        }
        let viewport = self.viewport.borrow().clone();
        let geometry = viewport
            .upgrade()
            .ok_or(Error::Pending)?
            .geometry()
            .map_err(|_| Error::Pending)?;
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: geometry.document_visible.origin,
            scroll_epoch: geometry.scroll_epoch,
        }) // RefCell借用已在geometry/current等可重入调用之前结束。
    }
    fn ready_scene(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>, Error> {
        let scene = self.scene.borrow().clone().ok_or(Error::Pending)?;
        if !scene.key.request.same(request) || !self.scene_current(&scene.key) {
            return Err(Error::Pending);
        }
        Ok(scene)
    }
    fn scene_current(&self, key: &NativeMarkdownPagedSceneKey) -> bool {
        let scene = self.scene.borrow().clone();
        self.current(EPOCH)
            && scene.is_some_and(|scene| scene.key.same(key) && scene.geometry.is_current())
            && self
                .input
                .upgrade()
                .is_some_and(|input| input.state().is_ok_and(|state| state == key.request.input))
    }
    fn committed(&self, _commit: &NativeMarkdownPagedInputCommit) -> Result<(), Error> {
        Err(Error::Pending) // 本组件没有完整调度器，不能把未消费后果当成编辑parity。
    }
    fn reveal_committed_selection(&self, _: &NativeMarkdownPagedInputCommit) -> Result<(), Error> {
        // 本有限组件没有正文Producer/reveal续体，不能把选择提交伪装成实际可见。
        Err(Error::Pending)
    }
    fn copy_selection(&self, _request: &NativeMarkdownPagedSceneRequest) -> Result<(), Error> {
        Err(Error::Pending)
    }
}
