//! Paged source line numbers consume bounded geometry from the original Scene; Body still owns mounting, pixels, and closure.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak as RcWeak},
    sync::Arc,
};

use objc2::{
    ClassType, DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::{Retained, Weak, autoreleasepool},
    runtime::AnyObject,
};
use objc2_app_kit::{
    NSAccessibility, NSClipView, NSEvent, NSRulerOrientation, NSRulerView, NSScrollView, NSView,
    NSViewLayerContentsRedrawPolicy,
};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::{CGColor, CGContext};
use objc2_foundation::{
    MainThreadMarker, NSEdgeInsets, NSObjectProtocol, NSPoint, NSRect, NSSize, ns_string,
};
use objc2_quartz_core::{CALayer, CATransaction, kCAContentsFormatRGBA8Uint, kCAGravityResize};

use super::labels::LineNumberFont;
use crate::native_surface::{
    paged_body::{NativeMarkdownPagedBody, scene::Scene},
    paged_text_view::{
        NativeMarkdownPagedCloseReceipt, NativeMarkdownPagedTextScene, NativeMarkdownPagedTextView,
        NativeMarkdownPagedTextViewError as Error,
    },
    viewport_pixels::{Bitmap, NativeMarkdownViewportPixels},
};

type Result<T> = std::result::Result<T, Error>;
mod folding;

/// Intersects canonical logical lines with the same measured soft lines; continuations keep active highlighting without invented line numbers.
pub(in crate::native_surface) struct GutterRow {
    pub logical_line: usize,
    pub number: Option<usize>,
    pub may_fold: bool,
    pub folded: bool,
    pub frame: NSRect,
    pub active: bool,
}

pub(in crate::native_surface) struct PagedGutterIvars {
    owner: RefCell<RcWeak<NativeMarkdownPagedBody>>,
    view: RefCell<Option<Rc<Weak<NativeMarkdownPagedTextView>>>>,
    scroll: RefCell<Option<Rc<Weak<NSScrollView>>>>,
    clip: RefCell<Option<Rc<Weak<NSClipView>>>>,
    scene: RefCell<RcWeak<Scene>>,
    font: RefCell<Option<Rc<LineNumberFont>>>,
    layer: RefCell<Option<Rc<Retained<CALayer>>>>,
    // Shares the document pixel Arc; its live-byte budget, actual munmap, and close events also cover this narrow image.
    pixels: RefCell<Option<Arc<NativeMarkdownViewportPixels>>>,
    original_ruler_flags: (bool, bool),
    original_clip_automatic_insets: bool,
    original_clip_content_insets: NSEdgeInsets,
    painting: Cell<bool>,
    closed: Cell<bool>,
}

define_class!(
    // SAFETY: All main-thread NSRulerView ivars precede super init; owner back-references and Scene are weak.
    #[unsafe(super(NSRulerView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PagedGutterIvars]
    pub(in crate::native_surface) struct NativeMarkdownPagedGutter;

    // SAFETY: Inherits public main-thread NSObject identity without changing superclass or protocol layout.
    unsafe impl NSObjectProtocol for NativeMarkdownPagedGutter {}

    impl NativeMarkdownPagedGutter {
        #[unsafe(method(isFlipped))]
        /// Matches the paged document and CPU context's y-up coordinates; actual NSView relationships own conversion.
        fn is_flipped(&self) -> bool { false }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool { true }

        #[unsafe(method(acceptsFirstResponder))]
        /// Line numbers are presentation only and retain the original nonfocusable number-area interaction, without input commands.
        fn accepts_first_responder(&self) -> bool { false }

        #[unsafe(method(canBecomeKeyView))]
        /// Excludes Tab eligibility during construction so decoration cannot enter the document input chain.
        fn can_become_key_view(&self) -> bool { false }

        #[unsafe(method(updateLayer))]
        /// Actual AppKit drawing consumes the current weak Scene only; failure neither recaptures authority nor starts another scheduler.
        fn update_layer(&self) {
            if self.ivars().closed.get() || self.ivars().painting.replace(true) {
                return; // Closed or synchronously reentrant drawing cannot create duplicate candidate pixels.
            }
            // Even synchronous displayIfNeeded may run inside a long-lived caller pool; getter/CA temporaries belong only to this frame.
            let result = autoreleasepool(|_| self.paint());
            self.ivars().painting.set(false);
            let owner = self.ivars().owner.borrow().upgrade();
            if let Err(error) = result
                && !matches!(error, Error::Pending | Error::Revoked)
                && let Some(owner) = owner
            {
                owner.gutter_failed(error); // Actual resource/font/Source errors enter the original Body's failure path rather than silently dropping frames.
            }
        }

        #[unsafe(method(drawHashMarksAndLabelsInRect:))]
        /// Labels enter the CPU image only; the NSRuler superclass must not create a window glyph display list.
        fn draw_hash_marks_and_labels(&self, _dirty: NSRect) {}

        #[unsafe(method(drawRect:))]
        /// The layer-backed entry exclusively owns display; legacy ruler drawing cannot bypass CPU pixel ownership.
        fn draw_rect(&self, _dirty: NSRect) {}

        #[unsafe(method(mouseDown:))]
        /// Source-gutter clicks retain the same fold action; a late Scene cannot recapture successor layout authority.
        fn mouse_down(&self, event: &NSEvent) {
            let result = autoreleasepool(|_| self.toggle_fold_at_event(event));
            if let Err(cause) = result && !matches!(cause, Error::Pending | Error::Revoked) {
                crate::native_surface::present_native_markdown_error(&cause.to_string());
            }
        }
    }
);

impl NativeMarkdownPagedGutter {
    /// Body already constructed CPU content; this ruler shares its actual pixel ledger without hidden text views.
    pub(in crate::native_surface) fn new(
        owner: RcWeak<NativeMarkdownPagedBody>,
        scroll: &NSScrollView,
        view: &NativeMarkdownPagedTextView,
        lines: usize,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>> {
        autoreleasepool(|_| {
            // Construction may run in the caller's session pool; only the returned Retained owner and explicit ivars survive this phase.
            if construction_step(view, || scroll.verticalRulerView())?.is_some() {
                return Err(Error::InvalidGeometry); // Does not take over another ruler already mounted by the caller.
            }
            let font = Rc::new(construction_step(view, LineNumberFont::new)??);
            let width = font.width(lines);
            let pixels = construction_step(view, || view.pixel_observer())?
                .expect("Source Body已装配CPU正文")
                .0;
            let original_ruler_flags = (
                construction_step(view, || scroll.hasVerticalRuler())?,
                construction_step(view, || scroll.rulersVisible())?,
            );
            let clip = construction_step(view, || scroll.contentView())?;
            let original_clip_automatic_insets =
                construction_step(view, || clip.automaticallyAdjustsContentInsets())?;
            let original_clip_content_insets = construction_step(view, || clip.contentInsets())?;
            let allocated = Self::alloc(mtm).set_ivars(PagedGutterIvars {
                owner: RefCell::new(owner),
                view: RefCell::new(Some(Rc::new(Weak::from(view)))),
                scroll: RefCell::new(Some(Rc::new(Weak::from(scroll)))),
                clip: RefCell::new(Some(Rc::new(Weak::from(&*clip)))),
                scene: RefCell::new(RcWeak::new()),
                font: RefCell::new(Some(font)),
                layer: RefCell::new(None),
                pixels: RefCell::new(Some(pixels)),
                original_ruler_flags,
                original_clip_automatic_insets,
                original_clip_content_insets,
                painting: Cell::new(false),
                closed: Cell::new(false),
            });
            // SAFETY: The main-thread designated initializer receives the same scroll after every Rust payload is initialized.
            let ruler: Retained<Self> = construction_step(view, || unsafe {
                msg_send![super(allocated), initWithScrollView: Some(scroll), orientation: NSRulerOrientation::VerticalRuler]
            })?;
            construction_step(view, || ruler.setClipsToBounds(true))?;
            construction_step(view, || ruler.setReservedThicknessForMarkers(0.0))?;
            construction_step(view, || ruler.setReservedThicknessForAccessoryView(0.0))?;
            construction_step(view, || ruler.setRuleThickness(width))?;
            construction_step(view, || ruler.setClientView(Some(view)))?;
            construction_step(view, || {
                ruler.setAccessibilityLabel(Some(ns_string!("源码行号")))
            })?;
            // AppKit creates the sole backing; font drawing stays in bounded CPU contexts without superclass ruler painting.
            construction_step(view, || ruler.setWantsLayer(true))?;
            construction_step(view, || {
                ruler.setLayerContentsRedrawPolicy(
                    NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay,
                )
            })?;
            let layer = construction_step(view, || ruler.layer())?
                .ok_or_else(|| Error::Raster("无法建立源码行号图层".into()))?;
            let background = CGColor::new_srgb(19.0 / 255.0, 22.0 / 255.0, 28.0 / 255.0, 1.0);
            construction_step(view, || layer.setBackgroundColor(Some(&background)))?;
            *ruler.ivars().layer.borrow_mut() = Some(Rc::new(layer));
            Ok(ruler)
        })
    }

    /// The caller installs the owner slot before mounting, so synchronous native notifications see the sole assembled ruler.
    pub(in crate::native_surface) fn mount(&self) -> Result<()> {
        autoreleasepool(|_| {
            // Mount-triggered layout/getter temporaries return before this call ends instead of surviving into post-close waits.
            let view = self.body_view().ok_or(Error::Revoked)?;
            let scroll = self.scroll_view().ok_or(Error::Revoked)?;
            let clip = self.clip_view().ok_or(Error::Revoked)?;
            if self.ivars().closed.get()
                || construction_step(&view, || scroll.verticalRulerView())?.is_some()
                || !std::ptr::eq::<NSClipView>(
                    &*construction_step(&view, || scroll.contentView())?,
                    &*clip,
                )
            {
                return Err(Error::Revoked);
            }
            construction_step(&view, || scroll.setVerticalRulerView(Some(self)))?;
            // AppKit may overlay the ruler on the full clip; public clip.contentInsets records the actual reserved area.
            // The original clip calculates actual ruler/header insets; viewport uses them for the sole document instead of guessing ruler width.
            // The scroll's automatic title-bar policy belongs to the caller and is not changed here.
            construction_step(&view, || clip.setAutomaticallyAdjustsContentInsets(true))?;
            construction_step(&view, || scroll.setHasVerticalRuler(true))?;
            construction_step(&view, || scroll.setRulersVisible(true))?;
            construction_step(&view, || scroll.tile())?;
            Ok(())
        })
    }

    pub(in crate::native_surface) fn is_busy(&self) -> bool {
        self.ivars().painting.get()
    }

    /// Only a line-count digit-width change updates native thickness; the caller captures actual document geometry in its subsequent turn.
    pub(in crate::native_surface) fn update_width(&self, lines: usize) -> Result<bool> {
        let font = self.ivars().font.borrow().clone().ok_or(Error::Revoked)?;
        let width = font.width(lines);
        let view = self.body_view().ok_or(Error::Revoked)?;
        if self.ivars().closed.get() {
            return Err(Error::Revoked);
        }
        if construction_step(&view, || self.ruleThickness())? == width {
            return Ok(false); // An unchanged digit width neither repeats layout nor revokes the document Scene being painted.
        }
        self.clear_snapshot();
        construction_step(&view, || self.setRuleThickness(width))?;
        let scroll = self.scroll_view().ok_or(Error::Revoked)?;
        // Completes actual scroll layout after a digit-width change; delayed tile would let the next document capture stale clip width.
        construction_step(&view, || scroll.tile())?;
        Ok(true)
    }

    /// Stores only the same publication's weak Scene, without extending Reader, Source, or document lifetime.
    pub(in crate::native_surface) fn publish(&self, scene: &Rc<Scene>) -> Result<()> {
        if scene.key().presentation_mode != omni_markdown_core::MarkdownPresentationMode::Source {
            // A hidden source accessory consumes no preview layout and retains no old Source through queued gutter paint.
            self.clear_snapshot();
            return Ok(());
        }
        scene.gutter_rows()?;
        if self.ivars().closed.get() {
            return Err(Error::Revoked);
        }
        *self.ivars().scene.borrow_mut() = Rc::downgrade(scene);
        self.native(scene, || self.setNeedsDisplay(true))
    }

    /// Revoking Scene removes label pixels; an empty retained ruler cannot authorize the next epoch or display old numbers.
    pub(in crate::native_surface) fn clear_snapshot(&self) {
        *self.ivars().scene.borrow_mut() = RcWeak::new();
        let layer = self.ivars().layer.borrow().clone();
        if let Some(layer) = layer {
            // SAFETY: Clears only this owned layer's contents; provider release follows the original pixel callback to actual munmap.
            unsafe { layer.setContents(None) };
        }
    }

    /// One editing session owns one standard ruler; mode changes visibility and returns its pixel claim, while final close tears down the graph.
    pub(in crate::native_surface) fn set_mode_visibility(
        &self,
        visible: bool,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        if self.ivars().closed.get() || !current() {
            return Err(Error::Revoked);
        }
        let scroll = self.scroll_view().ok_or(Error::Revoked)?;
        if !current() {
            return Err(Error::Revoked);
        }
        let actual = scroll.verticalRulerView();
        if !current()
            || !actual
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<NSRulerView>(actual, self.as_super()))
        {
            return Err(Error::Revoked);
        }
        drop(actual); // Native claim destruction precedes subsequent state mutation and RefCell borrowing.
        self.clear_snapshot();
        if !current() {
            return Err(Error::Revoked);
        }
        self.setNeedsDisplay(false); // Old damage notifications cannot recreate label pixels while hidden.
        if !current() {
            return Err(Error::Revoked);
        }
        scroll.setRulersVisible(visible);
        if !current() {
            return Err(Error::Revoked);
        }
        scroll.tile(); // Completes the actual clip-width change before the next mode target lays out its new lane.
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Weak loading may enter native reference counting; releases the slot borrow first so close can take that same slot.
    fn body_view(&self) -> Option<Retained<NativeMarkdownPagedTextView>> {
        let view = self.ivars().view.borrow().clone();
        view.and_then(|view| view.load())
    }

    /// Borrows the original scroll for mounting/teardown only; closure leaves no Weak control block in a framework-cached shell.
    fn scroll_view(&self) -> Option<Retained<NSScrollView>> {
        let scroll = self.ivars().scroll.borrow().clone();
        scroll.and_then(|scroll| scroll.load())
    }

    /// Restores only the originally mounted clip; weak identity prevents modifying a replacement contentView during closure.
    fn clip_view(&self) -> Option<Retained<NSClipView>> {
        let clip = self.ivars().clip.borrow().clone();
        clip.and_then(|clip| clip.load())
    }

    fn require(&self, scene: &Rc<Scene>) -> Result<()> {
        scene.gutter_rows()?; // Original Source/Font/Reader failure precedes ordinary native revocation, without recapturing a successor Scene.
        if self.ivars().closed.get()
            || !RcWeak::ptr_eq(&self.ivars().scene.borrow(), &Rc::downgrade(scene))
        {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Checks the same Scene around native reentry; revoked return values cannot become input to a new layout.
    fn native<T>(&self, scene: &Rc<Scene>, work: impl FnOnce() -> T) -> Result<T> {
        self.require(scene)?;
        let value = work();
        self.require(scene)?;
        Ok(value)
    }

    fn paint(&self) -> Result<()> {
        let scene = self
            .ivars()
            .scene
            .borrow()
            .upgrade()
            .ok_or(Error::Pending)?;
        self.require(&scene)?;
        let body = self
            .native(&scene, || self.body_view())?
            .ok_or(Error::Revoked)?;
        let font = self.ivars().font.borrow().clone().ok_or(Error::Revoked)?;
        let layer = self.ivars().layer.borrow().clone().ok_or(Error::Revoked)?;
        let pixels = self.ivars().pixels.borrow().clone().ok_or(Error::Revoked)?;
        let bounds = self.native(&scene, || self.bounds())?;
        let window = self
            .native(&scene, || self.window())?
            .ok_or(Error::Pending)?;
        let scale = self.native(&scene, || window.backingScaleFactor())?;
        let width = bounds.size.width * scale;
        let height = bounds.size.height * scale;
        if !scale.is_finite()
            || scale <= 0.0
            || !width.is_finite()
            || !height.is_finite()
            || width <= 0.0
            || height <= 0.0
            || width > usize::MAX as f64
            || height > usize::MAX as f64
            || !bounds.origin.x.is_finite()
            || !bounds.origin.y.is_finite()
        {
            return Err(Error::InvalidGeometry);
        }
        // Allocates only the actual ruler viewport; Bitmap shares the real page budget rather than document logical height.
        let bitmap = Bitmap::new(width.ceil() as usize, height.ceil() as usize, pixels)
            .map_err(Error::Raster)?;
        let context = bitmap.context();
        CGContext::concat_ctm(
            Some(context),
            CGAffineTransform {
                a: scale,
                b: 0.0,
                c: 0.0,
                d: scale,
                tx: -bounds.origin.x * scale,
                ty: -bounds.origin.y * scale,
            },
        );
        CGContext::clip_to_rect(Some(context), bounds);
        CGContext::set_rgb_fill_color(Some(context), 19.0 / 255.0, 22.0 / 255.0, 28.0 / 255.0, 1.0);
        CGContext::fill_rect(Some(context), bounds);
        for row in scene.gutter_rows()? {
            let frame =
                self.native(&scene, || self.convertRect_fromView(row.frame, Some(&body)))?;
            let bottom = bounds.origin.y.max(frame.origin.y);
            let top =
                (bounds.origin.y + bounds.size.height).min(frame.origin.y + frame.size.height);
            if top <= bottom {
                continue; // An offscreen first soft line does not move its number to the viewport top; continuations clip their own actual geometry.
            }
            if row.active {
                // Adjacent soft lines may meet at fractional device-pixel edges; separate antialiasing would blend
                // inactive color into both sides and create a false seam. Only backgrounds disable antialiasing; glyph state is restored afterward.
                CGContext::save_g_state(Some(context));
                CGContext::set_should_antialias(Some(context), false);
                CGContext::set_rgb_fill_color(
                    Some(context),
                    27.0 / 255.0,
                    32.0 / 255.0,
                    42.0 / 255.0,
                    1.0,
                );
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(bounds.origin.x, bottom),
                        NSSize::new(bounds.size.width, top - bottom),
                    ),
                );
                CGContext::restore_g_state(Some(context));
            }
            if let Some(number) = row.number {
                CGContext::set_rgb_fill_color(
                    Some(context),
                    81.0 / 255.0,
                    91.0 / 255.0,
                    109.0 / 255.0,
                    1.0,
                );
                self.native(&scene, || {
                    font.draw(
                        context,
                        number,
                        bounds.origin.x + bounds.size.width - 1.0 - 3.0 - 10.0,
                        frame,
                    )
                })?;
                if row.folded || row.may_fold {
                    folding::draw_fold_marker(
                        context,
                        frame,
                        bounds.origin.x + bounds.size.width - 3.0,
                        row.folded,
                    );
                }
            } // Only the original logical-line start supplies a number; wrapped continuations create no duplicate number or fold marker.
        }
        CGContext::set_rgb_fill_color(Some(context), 37.0 / 255.0, 42.0 / 255.0, 51.0 / 255.0, 1.0);
        CGContext::fill_rect(
            Some(context),
            NSRect::new(
                NSPoint::new(bounds.origin.x + bounds.size.width - 1.0, bounds.origin.y),
                NSSize::new(1.0, bounds.size.height),
            ),
        );
        self.require(&scene)?;
        let image = bitmap.finish().map_err(Error::Raster)?; // The writable context has actually ended before its read-only provider transfers to the layer.
        if self.native(&scene, || self.bounds())? != bounds
            || self.native(&scene, || window.backingScaleFactor())? != scale
            || !self
                .native(&scene, || self.layer())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &**layer))
            || !self
                .native(&scene, || self.clientView())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<NSView>(actual, body.as_super()))
            || !self
                .native(&scene, || self.window())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &*window))
        {
            return Err(Error::Revoked);
        }
        let transaction = self.native(&scene, || {
            CATransaction::begin();
            GutterTransaction // Temporary Drop balances begin even if the post-call intent check revokes authority.
        })?;
        self.native(&scene, || CATransaction::setDisableActions(true))?;
        // SAFETY: QuartzCore exports these process-lifetime constants; only the same original owned backing is configured.
        self.native(&scene, || unsafe {
            layer.setContentsFormat(kCAContentsFormatRGBA8Uint)
        })?;
        // SAFETY: The same constant lifetime applies; the complete image uses actual scale, without partial or downsampled substitution.
        self.native(&scene, || unsafe {
            layer.setContentsGravity(kCAGravityResize)
        })?;
        self.native(&scene, || layer.setContentsScale(scale))?;
        let mut publication = GutterPublication {
            layer: &layer,
            image: std::ptr::from_ref(&*image).cast::<AnyObject>(),
            accepted: false,
        };
        // SAFETY: CALayer.contents receives the actual CF CGImage; bridges identity only, without sending NSObject messages to CF.
        self.native(&scene, || unsafe {
            layer.setContents(Some(&*std::ptr::from_ref(&*image).cast::<AnyObject>()));
        })?;
        drop(transaction);
        if self.native(&scene, || self.bounds())? != bounds
            || self.native(&scene, || window.backingScaleFactor())? != scale
            || !self
                .native(&scene, || self.layer())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &**layer))
            || !self
                .native(&scene, || self.clientView())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<NSView>(actual, body.as_super()))
            || !self
                .native(&scene, || self.window())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &*window))
        {
            return Err(Error::Revoked); // An executed setter still requires original geometry checks; revocation removes only this actual image, never successor contents.
        }
        // SAFETY: Reads this layer's public contents relationship only, without NSObject messages to its CF image.
        let actual = self.native(&scene, || unsafe { layer.contents() })?;
        if !actual
            .as_deref()
            .is_some_and(|actual| std::ptr::eq(actual, publication.image))
        {
            return Err(Error::Revoked);
        }
        publication.accepted = true;
        Ok(())
    }

    /// Actual pixels must return under the ruler contract; if AppKit caches an empty control, the receipt separately checks every owned payload was removed.
    pub(in crate::native_surface) fn close(
        &self,
        receipt: &NativeMarkdownPagedCloseReceipt,
    ) -> Result<()> {
        if self.ivars().closed.replace(true) {
            return Ok(());
        }
        let pixels = self
            .ivars()
            .pixels
            .borrow_mut()
            .take()
            .expect("已装配的Source行号持有原正文像素页账");
        receipt.track_pixels(&pixels);
        drop(pixels); // The receipt keeps actual page-return observation; a cached ruler cannot retain the project's shared Arc.
        let layer = self.ivars().layer.borrow().clone();
        receipt.track_gutter(PagedGutterRelease {
            ruler: Weak::from(self),
            layer: layer.as_ref().map(|layer| Weak::from(&***layer)),
        });
        let owner = self.ivars().owner.replace(RcWeak::new());
        drop(owner); // Even after Body's last strong claim ends, its Weak control block must leave any framework-cached object.
        autoreleasepool(|_| {
            // Close-stage public getters, CA temporaries, and contents results must not survive in a caller pool across waits.
            // The original receipt fixes pixel and layer identities; teardown below still performs actual cleanup and retains cleanup failure.
            let original_window = self.window();
            let this_view: &NSView = self;
            // SAFETY: The original main-thread close stack keeps the ruler live; public key-view getters return its actual current neighbors.
            let (previous_key, next_key) = unsafe { (self.previousKeyView(), self.nextKeyView()) };
            if let Some(previous) = previous_key.as_deref()
                && !std::ptr::eq(previous, this_view)
            {
                // SAFETY: The getter keeps previous alive; reads its actual incoming edge without traversing the focus graph.
                let incoming = unsafe { previous.nextKeyView() };
                if incoming
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq(actual, this_view))
                {
                    let successor = next_key
                        .as_deref()
                        .filter(|actual| !std::ptr::eq(*actual, this_view));
                    // SAFETY: Reconnects the original predecessor only while it still points here, preserving an externally replaced chain.
                    unsafe { previous.setNextKeyView(successor) };
                }
            }
            // SAFETY: The sealed ruler clears its own outgoing edge without destroying or reading neighboring view payloads.
            unsafe { self.setNextKeyView(None) };
            self.clear_snapshot();
            let font = self.ivars().font.borrow_mut().take();
            drop(font); // Releases ten glyph scalars and this project's NSFont claim; the system font has no document/business back-reference.
            self.setClientView(None);
            let mut result = Ok(());
            if let Some(scroll) = self.scroll_view() {
                if scroll
                    .verticalRulerView()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<NSRulerView>(actual, self.as_super()))
                {
                    scroll.setVerticalRulerView(None);
                    scroll.setHasVerticalRuler(self.ivars().original_ruler_flags.0);
                    scroll.setRulersVisible(self.ivars().original_ruler_flags.1);
                    if let Some(clip) = self.clip_view()
                        && std::ptr::eq::<NSClipView>(&*scroll.contentView(), &*clip)
                    {
                        clip.setAutomaticallyAdjustsContentInsets(
                            self.ivars().original_clip_automatic_insets,
                        );
                        if !self.ivars().original_clip_automatic_insets {
                            // Mount-time automatic policy may replace caller-specified insets; disables automatic adjustment before restoring those values.
                            // An originally automatic policy lets AppKit recompute after ruler removal instead of restoring stale automatic results.
                            clip.setContentInsets(self.ivars().original_clip_content_insets);
                        }
                        if !std::ptr::eq::<NSClipView>(&*scroll.contentView(), &*clip) {
                            result = Err(Error::Revoked); // Setter reentry replacing the original clip cannot be reported as successful restoration.
                        }
                    } else {
                        result = Err(Error::Revoked); // A successor contentView lies outside this original clip-policy restoration.
                    }
                } else {
                    result = Err(Error::Revoked); // An externally replaced mount cannot authorize deleting a successor ruler for cleanup.
                }
            }
            // The scroll ruler property and NSView hierarchy are separate strong edges; removes the actual owned subview
            // and the ruler's weak scroll relationship, because a nil property alone does not prove detachment from the native parent graph.
            self.removeFromSuperview();
            self.setScrollView(None);
            if let Some(window) = &original_window {
                window.recalculateKeyViewLoop(); // Marks recalculation only; the original window updates its focus loop after actual removal.
                if let Some(view) = self.body_view()
                    && view
                        .window()
                        .as_deref()
                        .is_some_and(|actual| std::ptr::eq(actual, &**window))
                {
                    // AppKit recalculates on the next in-window key-view query; queries the original document once without moving focus.
                    // SAFETY: Both live objects belong to the verified original window; AppKit rebuilds valid key-view edges from its current tree.
                    drop(unsafe { view.nextKeyView() });
                }
            }
            self.setNeedsDisplay(false); // Cancels this owned view's pending damage so a closed empty shell creates no more pixels.
            if let Some(layer) = &layer {
                let actual = self.layer();
                if actual
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &***layer))
                {
                    self.setWantsLayer(false);
                    // Disabling layers may synchronously remove backing; clears only the original or now-empty relationship, never an external replacement.
                    let after_disable = self.layer();
                    if after_disable
                        .as_deref()
                        .is_none_or(|actual| std::ptr::eq::<CALayer>(actual, &***layer))
                    {
                        self.setLayer(None);
                    } else {
                        result = Err(Error::Revoked);
                    }
                } else {
                    result = Err(Error::Revoked); // Preserves replacement backing; the original owned layer still sheds its own payload below.
                }
                layer.removeFromSuperlayer(); // The original layer cannot retain this drawing as another layer's child.
                layer.setDelegate(None); // Also removes NSView's actual backing-layer drawing callback relationship.
                layer.setBackgroundColor(None);
                // SAFETY: Clears only this component's original image/mask/sublayers; None introduces no external object or CA ownership.
                unsafe {
                    layer.setContents(None);
                    layer.setMask(None);
                    layer.setSublayers(None);
                }
            }
            drop(self.ivars().layer.borrow_mut().take());
            // After native getters finish, takes the Native Weak allocations too; empty controls retain no old graph identity slots.
            let view = self.ivars().view.borrow_mut().take();
            let scroll = self.ivars().scroll.borrow_mut().take();
            let clip = self.ivars().clip.borrow_mut().take();
            drop((view, scroll, clip));
            drop(layer);
            CATransaction::flush(); // Flushes CA after actual owned backing/mount detachment, before this local native pool ends.
            result
        })
    }

    /// Checks every payload slot so framework-cached controls are empty shells, with no Weak blocks, pixel Arc, or fonts left in ivars.
    fn payload_is_empty(&self) -> bool {
        let payload_empty = self.ivars().closed.get()
            && !self.ivars().painting.get()
            && RcWeak::ptr_eq(&self.ivars().owner.borrow(), &RcWeak::new())
            && self.ivars().view.borrow().is_none()
            && self.ivars().scroll.borrow().is_none()
            && self.ivars().clip.borrow().is_none()
            && RcWeak::ptr_eq(&self.ivars().scene.borrow(), &RcWeak::new())
            && self.ivars().font.borrow().is_none()
            && self.ivars().layer.borrow().is_none()
            && self.ivars().pixels.borrow().is_none();
        if !payload_empty {
            return false; // Dead Weak references or zero pixel counts cannot replace removal of the actual ownership slots.
        }
        // All Rust borrows have ended; public relationships must be actually detached rather than certified by a closed flag alone.
        // SAFETY: The receipt's same-thread Weak upgrade keeps this object alive; getters do not access removed project payloads.
        let detached = unsafe { self.superview().is_none() && self.nextKeyView().is_none() };
        detached
            && self.window().is_none()
            && self.clientView().is_none()
            && self.scrollView().is_none()
            && self.layer().is_none()
            && !self.wantsLayer()
    }
}

/// Observes only this actual ruler/backing without document, font, pixel Arc, or Native strong owners and without another resource ledger.
pub(in crate::native_surface) struct PagedGutterRelease {
    ruler: Weak<NativeMarkdownPagedGutter>,
    layer: Option<Weak<CALayer>>,
}

impl PagedGutterRelease {
    /// AppKit may cache empty standard controls; project payloads and original layer relationships still require checks on the actual objects.
    pub(in crate::native_surface) fn verify_released(&self) -> std::result::Result<(), String> {
        autoreleasepool(|_| {
            if self
                .ruler
                .load()
                .is_some_and(|ruler| !ruler.payload_is_empty())
            {
                return Err("源码行号仍持有业务载荷或原生挂载关系".into());
            }
            if let Some(layer) = self.layer.as_ref().and_then(Weak::load) {
                // SAFETY: The receipt reads only its original layer, testing CF contents for absence without sending NSObject messages.
                let contents_empty = unsafe { layer.contents().is_none() };
                // SAFETY: The original layer remains live on the same main thread; reads public sublayer count without mutating unknown objects.
                let children_empty =
                    unsafe { layer.sublayers() }.is_none_or(|children| children.is_empty());
                if !contents_empty
                    || !children_empty
                    || layer.mask().is_some()
                    || layer.backgroundColor().is_some()
                    || layer.delegate().is_some()
                    || layer.superlayer().is_some()
                {
                    return Err("源码行号原图层仍持有像素、回调或父子关系".into());
                }
            } // Only actual layer destruction or empty payload/relationships satisfies the standard-control empty-shell contract.
            Ok(())
        })
    }
}

/// Balances the original CATransaction on every failure path; commit alone cannot certify a revoked candidate.
struct GutterTransaction;
impl Drop for GutterTransaction {
    fn drop(&mut self) {
        CATransaction::commit();
    }
}

/// Revocation after a setter still has pixel consequences; removes only contents matching this image, preserving successor publication.
struct GutterPublication<'a> {
    layer: &'a CALayer,
    image: *const AnyObject,
    accepted: bool,
}
impl Drop for GutterPublication<'_> {
    fn drop(&mut self) {
        if self.accepted {
            return; // After original intent and actual contents checks pass, the layer retains the sole published image ownership.
        }
        // SAFETY: Compares public native contents identity only; CF objects receive no object messages.
        let actual = unsafe { self.layer.contents() };
        if actual
            .as_deref()
            .is_some_and(|actual| std::ptr::eq(actual, self.image))
        {
            // SAFETY: The relationship still belongs to this failed setter, so clears only its own pixels, not external or successor contents.
            unsafe { self.layer.setContents(None) };
        }
    }
}

/// Construction/width work has no Scene yet; borrows the original CPU document's session authority without remounting old graphs after native closure.
fn construction_step<T>(view: &NativeMarkdownPagedTextView, work: impl FnOnce() -> T) -> Result<T> {
    if !view.session_is_current() {
        return Err(Error::Revoked);
    }
    let value = work();
    if !view.session_is_current() {
        return Err(Error::Revoked);
    }
    Ok(value)
}
