//! Isolates public AppKit TextKit 2 ownership from the plugin's custom view and storage.
//!
//! Each open/input/close/verification phase runs in a separate actual application event.
//! Main-queue handoffs unwind the preceding callback and its autorelease pool once; no
//! polling, sleeping, retry allowance, custom text subclass, or piece-backed storage is used.

use dispatch2::DispatchQueue;
use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{ClassType, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSEvent, NSEventModifierFlags, NSEventType, NSScrollView, NSTextContainer,
    NSTextContentStorage, NSTextInputClient, NSTextLayoutManager, NSTextStorage,
    NSTextStorageObserving, NSTextView, NSView, NSWindow, NSWindowAnimationBehavior,
    NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFRunLoop, CFRunLoopActivity, CFRunLoopObserver, kCFRunLoopCommonModes,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize,
    ns_string,
};
use std::cell::{Cell, RefCell};

/// Owns exactly one ordinary, mounted native text graph until the close phase consumes it.
struct ControlSession {
    window: Retained<NSWindow>,
    content_view: Retained<NSView>,
    scroll_view: Retained<NSScrollView>,
    text_view: Retained<NSTextView>,
    content_storage: Retained<NSTextContentStorage>,
    layout_manager: Retained<NSTextLayoutManager>,
    text_container: Retained<NSTextContainer>,
}

impl ControlSession {
    /// Creates the standard TextKit 2 network and makes its real text responder input-ready.
    fn open(mtm: MainThreadMarker) -> Result<Self, String> {
        // A standard initializer owns AppKit's ordinary storage; the control adds no custom adapter.
        let text_view = NSTextView::initUsingTextLayoutManager(NSTextView::alloc(mtm), true);
        let content_storage = text_view
            .textContentStorage()
            .ok_or("ordinary TextKit 2 content storage is missing")?;
        let layout_manager = text_view
            .textLayoutManager()
            .ok_or("ordinary TextKit 2 layout manager is missing")?;
        let text_container = layout_manager
            .textContainer()
            .ok_or("ordinary TextKit 2 text container is missing")?;
        // Finite Unicode content exercises the native storage without involving disk or user data.
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(640.0, 420.0));
        text_view.setFrame(frame);
        text_view.setString(ns_string!("Plain TextKit 2 control\nUnicode: 中文 🦀\n"));
        text_view.setEditable(true);
        text_view.setSelectable(true);
        text_view.setAllowsUndo(false);
        let scroll_view = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll_view.setHasVerticalScroller(true);
        scroll_view.setDocumentView(Some(&text_view));
        // SAFETY: this main-thread allocation receives NSWindow's designated initializer with
        // valid scalar geometry/style values; Rust retains the returned independent owner.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: close must not consume Rust's owning reference; this finite session releases it.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setAnimationBehavior(NSWindowAnimationBehavior::None);
        window.setTitle(ns_string!("Plain TextKit 2 ownership control"));
        let content_view = window
            .contentView()
            .ok_or("ordinary control window has no content view")?;
        content_view.addSubview(&scroll_view);
        window.setInitialFirstResponder(Some(&text_view));
        let application = NSApplication::sharedApplication(mtm);
        application.activate();
        window.orderFrontRegardless();
        window.makeKeyWindow();
        if !window.makeFirstResponder(Some(&text_view)) {
            // Focus refusal means the requested native input boundary was never established.
            return Err("ordinary text view refused first responder".into());
        }
        // Mounted geometry is resolved synchronously; the next actual event owns the input phase.
        content_view.layoutSubtreeIfNeeded();
        text_view.layoutSubtreeIfNeeded();
        window.displayIfNeeded();
        println!(
            "plain_textkit_display=open visible={} key={} insertion_point={}",
            window.isVisible(),
            window.isKeyWindow(),
            text_view.shouldDrawInsertionPoint()
        );
        Ok(Self {
            window,
            content_view,
            scroll_view,
            text_view,
            content_storage,
            layout_manager,
            text_container,
        })
    }

    /// Routes one finite insertion through the ordinary NSTextInputClient implementation.
    fn exercise(&self) -> Result<(), String> {
        // The text client is the actual view, rather than a replacement buffer or direct storage edit.
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*self.text_view);
        // SAFETY: the immutable NSString is live throughout this main-thread synchronous input call;
        // the zero-length replacement range is a valid scalar boundary in the existing document.
        unsafe {
            input.insertText_replacementRange(ns_string!("Inserted 🦀\n"), NSRange::new(0, 0))
        };
        if !self
            .text_view
            .string()
            .to_string()
            .starts_with("Inserted 🦀\n")
        {
            // A failed native insertion invalidates this control's claimed open/input lifecycle.
            return Err("ordinary text input did not update its native storage".into());
        }
        // Selection and display establish the same insertion-indicator/viewport responsibilities
        // that may temporarily retain an actual editor; this probe does not measure paint latency.
        self.text_view.setSelectedRange(NSRange::new(0, 0));
        self.content_view.layoutSubtreeIfNeeded();
        self.window.displayIfNeeded();
        println!(
            "plain_textkit_display=input visible={} key={} insertion_point={}",
            self.window.isVisible(),
            self.window.isKeyWindow(),
            self.text_view.shouldDrawInsertionPoint()
        );
        Ok(())
    }

    /// Disconnects public native ownership and returns evidence containing no retained graph nodes.
    fn close(self) -> Result<ReleaseEvidence, String> {
        // Capture weak identities before dismantling their relationships; the storage query's
        // temporary strong reference ends before any session owner is dropped below.
        let observer =
            ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*self.content_storage);
        let storage = observer
            .textStorage()
            .ok_or("ordinary content storage lost its text storage before close")?;
        let evidence = ReleaseEvidence {
            window: Weak::from_retained(&self.window),
            content_view: Weak::from_retained(&self.content_view),
            scroll_view: Weak::from_retained(&self.scroll_view),
            text_view: Weak::from_retained(&self.text_view),
            text_storage: Weak::from_retained(&storage),
            content_storage: Weak::from_retained(&self.content_storage),
            layout_manager: Weak::from_retained(&self.layout_manager),
            text_container: Weak::from_retained(&self.text_container),
        };
        drop(storage);
        if !self.window.makeFirstResponder(None) {
            // AppKit explicitly permits responder refusal; closing without observing it would
            // make a remaining insertion-indicator owner ambiguous in this diagnostic control.
            return Err("ordinary text responder refused to resign".into());
        }
        if self.window.firstResponder().is_some_and(|responder| {
            // Identity comparison retains only this temporary result within the close transaction.
            std::ptr::eq(
                std::ptr::from_ref(&*responder).cast::<()>(),
                std::ptr::from_ref(&*self.text_view).cast::<()>(),
            )
        }) {
            // Success must also remove the exact old text responder from the window's active route.
            return Err(
                "ordinary text view remained first responder after successful resignation".into(),
            );
        }
        self.window.setInitialFirstResponder(None);
        self.window.orderOut(None);
        self.window.close();
        self.window.setContentView(None);
        // Clear only documented ownership/delegate interfaces. AppKit performs input-context
        // activation and primitive view-container callbacks itself; no direct override call is made.
        self.text_view.unregisterDraggedTypes();
        self.text_view.setEditable(false);
        self.text_view.setSelectable(false);
        self.text_view.setDelegate(None);
        self.scroll_view.setDocumentView(None);
        self.scroll_view.removeFromSuperview();
        // SAFETY: these live main-thread graph nodes remain session-owned throughout detach;
        // clearing the nullable delegate/layout queue stops future work before their release.
        unsafe {
            self.content_storage.setDelegate(None);
            self.layout_manager.setLayoutQueue(None);
        }
        self.layout_manager.setDelegate(None);
        self.layout_manager
            .textViewportLayoutController()
            .setDelegate(None);
        self.text_container.setTextView(None);
        self.layout_manager.setTextContainer(None);
        observer.setTextStorage(None);
        self.content_storage.setAttributedString(None);
        self.content_storage.setPrimaryTextLayoutManager(None);
        self.content_storage
            .removeTextLayoutManager(&self.layout_manager);
        // The local pool also drains native temporaries created by the final Rust owner releases.
        autoreleasepool(|_| drop(self));
        Ok(evidence)
    }
}

/// Carries only weak object identities across the real event/autorelease/scheduler boundary.
struct ReleaseEvidence {
    window: Weak<NSWindow>,
    content_view: Weak<NSView>,
    scroll_view: Weak<NSScrollView>,
    text_view: Weak<NSTextView>,
    text_storage: Weak<NSTextStorage>,
    content_storage: Weak<NSTextContentStorage>,
    layout_manager: Weak<NSTextLayoutManager>,
    text_container: Weak<NSTextContainer>,
}

impl ReleaseEvidence {
    /// Observes every native node once and rejects any survivor without a delay or retry.
    fn verify(self) -> Result<(), String> {
        // Temporary upgraded owners are dropped at each expression before any aggregate decision.
        let nodes = [
            ("window", self.window.load().is_some()),
            ("content_view", self.content_view.load().is_some()),
            ("scroll_view", self.scroll_view.load().is_some()),
            ("text_view", self.text_view.load().is_some()),
            ("text_storage", self.text_storage.load().is_some()),
            ("content_storage", self.content_storage.load().is_some()),
            ("layout_manager", self.layout_manager.load().is_some()),
            ("text_container", self.text_container.load().is_some()),
        ];
        for (name, alive) in nodes {
            // Report all nodes even on failure so one early survivor cannot hide the rest of the graph.
            println!("plain_textkit_node={name} alive={alive}");
        }
        if nodes.iter().any(|(_, alive)| *alive) {
            // Strict release evidence is a failed control, never an allowed platform-retention mode.
            return Err("ordinary TextKit 2 graph survived the next native event boundary".into());
        }
        Ok(())
    }
}

/// Stores the unique current phase and transfers strong ownership out before native reentry.
struct ControlIvars {
    started: Cell<bool>,
    next_stage: Cell<i16>,
    session: RefCell<Option<ControlSession>>,
    evidence: RefCell<Option<ReleaseEvidence>>,
    result: RefCell<Option<Result<(), String>>>,
}

define_class!(
    // SAFETY: NSObject adds no subclass state requirements; initialized Rust ivars and all
    // native callbacks remain confined to the process's actual AppKit main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControlIvars]
    struct ControlDelegate;

    // SAFETY: this initialized NSObject subclass preserves native object identity and thread kind.
    unsafe impl NSObjectProtocol for ControlDelegate {}
    // SAFETY: main retains the weak application delegate through the complete real event loop.
    unsafe impl NSApplicationDelegate for ControlDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &NSNotification) {
            // The launch notification starts exactly one finite sequence; duplicate delivery is inert.
            if self.ivars().started.replace(true) {
                // A duplicate native notification cannot create another graph or event sequence.
                return;
            }
            post_stage(100, self.mtm());
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn keep_alive_after_close(&self, _application: &NSApplication) -> bool {
            // Native window closure must not terminate the process before weak verification executes.
            false
        }

        #[unsafe(method(applicationDidUpdate:))]
        /// Observe the public application update milestone without scheduling or changing fixture phases.
        fn application_did_update(&self, _notification: &NSNotification) {
            println!(
                "plain_textkit_milestone=application_did_update after_stage={}",
                OBSERVED_STAGE.with(Cell::get)
            );
        }
    }
);

impl ControlDelegate {
    /// Runs one actual event transaction and prevents a Rust unwind through the native callback ABI.
    fn run_stage(&self, stage: i16) {
        let mtm = self.mtm();
        if self.ivars().next_stage.get() != stage {
            // Duplicate or out-of-order fixture events fail instead of silently repeating ownership work.
            self.finish(Err(
                "control event phases were delivered out of order".into()
            ));
            return;
        }
        self.ivars().next_stage.set(stage + 1);
        OBSERVED_STAGE.with(|current| current.set(stage));
        println!("plain_textkit_stage={stage} boundary=actual_application_event");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Drain every phase's temporary Cocoa objects before publishing its scheduler successor.
            autoreleasepool(|_| match stage {
                100 => {
                    // Acquisition stays owned by the delegate until a later actual input event.
                    let session = ControlSession::open(mtm)?;
                    *self.ivars().session.borrow_mut() = Some(session);
                    Ok(())
                }
                101 => {
                    // End RefCell access before any native operation can reenter this delegate.
                    let session = self
                        .ivars()
                        .session
                        .borrow_mut()
                        .take()
                        .ok_or("missing open control")?;
                    let result = session.exercise();
                    *self.ivars().session.borrow_mut() = Some(session);
                    result
                }
                102 => {
                    // The close event consumes all native owners, leaving only weak evidence.
                    let session = self
                        .ivars()
                        .session
                        .borrow_mut()
                        .take()
                        .ok_or("missing exercised control")?;
                    let evidence = session.close()?;
                    *self.ivars().evidence.borrow_mut() = Some(evidence);
                    Ok(())
                }
                103 => {
                    // Verification occurs once after close returned and its native event pool drained.
                    let evidence = self
                        .ivars()
                        .evidence
                        .borrow_mut()
                        .take()
                        .ok_or("missing weak evidence")?;
                    evidence.verify()
                }
                _ => {
                    // The application router admits no other fixture phase identifiers.
                    Err("unknown control event stage".into())
                }
            })
        }))
        .unwrap_or_else(|_| Err("ordinary control panicked inside its native callback".into()));
        match result {
            Ok(()) if stage == 103 => {
                // A complete successful lifecycle stops only after every native weak node was checked.
                self.finish(Ok(()));
            }
            Ok(()) => {
                // A real main-queue handoff separates actual NSApplication events; it does not
                // spin a nested run loop or schedule an elapsed-time-based retry.
                DispatchQueue::main().exec_async(move || {
                    let mtm =
                        MainThreadMarker::new().expect("dispatch main queue is the AppKit thread");
                    post_stage(stage + 1, mtm);
                });
            }
            Err(error) => {
                // Preserve the failing stage and end the finite control rather than accepting a partial run.
                self.finish(Err(format!("stage {stage}: {error}")));
            }
        }
    }

    /// Records a terminal result and wakes the native loop after requesting its explicit stop.
    fn finish(&self, result: Result<(), String>) {
        *self.ivars().result.borrow_mut() = Some(result);
        NSApplication::sharedApplication(self.mtm()).stop(None);
        post_stage(0, self.mtm());
    }
}

// Routing retains no delegate or text graph between ordinary NSApplication events.
thread_local! {
    static DELEGATE: RefCell<Option<Weak<ControlDelegate>>> = const { RefCell::new(None) };
    static OBSERVED_STAGE: Cell<i16> = const { Cell::new(99) };
}

define_class!(
    // SAFETY: this subclass changes only routing of its own synthetic application events;
    // ordinary system events retain the inherited NSApplication behavior and main-thread ABI.
    #[unsafe(super = NSApplication)]
    #[thread_kind = MainThreadOnly]
    struct ControlApplication;

    impl ControlApplication {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            if event.r#type() == NSEventType::ApplicationDefined && (100..=103).contains(&event.subtype().0) {
                // Take a callback-local delegate owner after ending the routing RefCell borrow.
                let delegate = DELEGATE.with_borrow(|slot| slot.as_ref().and_then(Weak::load));
                if let Some(delegate) = delegate {
                    // Only the current retained delegate may advance the isolated fixture lifecycle.
                    delegate.run_stage(event.subtype().0);
                } else {
                    // Losing routing authority fails the finite process rather than waiting forever
                    // for a stage whose unique owner can no longer consume its event.
                    eprintln!("plain_textkit_control=failed: delegate routing authority disappeared");
                    self.stop(None);
                    post_stage(0, self.mtm());
                }
                return;
            }
            // SAFETY: the live event is forwarded exactly once with its matching Cocoa ABI;
            // this main-thread subclass does not alter ordinary native input or close events.
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
        }
    }
);

/// Posts a finite stage identifier without synthesizing keyboard input or capturing native owners.
fn post_stage(subtype: i16, mtm: MainThreadMarker) {
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0, 0, None, subtype, 0, 0,
    ).expect("AppKit constructs an application-defined control event");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
}

/// Uses the true process main thread and reports a nonzero exit for any failed native release.
fn main() {
    let mtm =
        MainThreadMarker::new().expect("plain TextKit control requires the macOS main thread");
    // Expose this control's scope and the exact scheduler boundary alongside every native result.
    println!("plain_textkit_control_kind=ordinary_nstextview_textkit2");
    println!(
        "plain_textkit_release_boundary=close_event_pool_then_main_queue_then_next_application_event"
    );
    // SAFETY: this is the first sharedApplication invocation in this standalone process;
    // the subclass inherits standard singleton initialization and has no additional state.
    let application: Retained<ControlApplication> =
        unsafe { msg_send![ControlApplication::class(), sharedApplication] };
    if !application.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
        // A rejected activation policy prevents this process from establishing its intended UI context.
        eprintln!("plain_textkit_control=failed: activation policy rejected");
        std::process::exit(1);
    }
    // The application delegate property is weak, so this Rust owner spans the actual run loop.
    let delegate = ControlDelegate::alloc(mtm).set_ivars(ControlIvars {
        started: Cell::new(false),
        next_stage: Cell::new(100),
        session: RefCell::new(None),
        evidence: RefCell::new(None),
        result: RefCell::new(None),
    });
    // SAFETY: all Rust ivars are initialized before NSObject's public initializer executes.
    let delegate: Retained<ControlDelegate> = unsafe { msg_send![super(delegate), init] };
    DELEGATE.with_borrow_mut(|slot| *slot = Some(Weak::from_retained(&delegate)));
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // Read-only run-loop observations establish whether immediately posted fixture events ever
    // allow the actual before-waiting/display cycle; the observer owns no native graph or stage work.
    let block = block2::RcBlock::new(
        |_observer: *mut CFRunLoopObserver, activity: CFRunLoopActivity| {
            println!(
                "plain_textkit_milestone=run_loop activity={} after_stage={}",
                activity.0,
                OBSERVED_STAGE.with(Cell::get)
            );
        },
    );
    // SAFETY: the default allocator accepts None and this live immutable block is copied by Core Foundation.
    let observer = unsafe {
        CFRunLoopObserver::with_handler(
            None,
            (CFRunLoopActivity::BeforeWaiting | CFRunLoopActivity::AfterWaiting).0,
            true,
            0,
            Some(&block),
        )
    }
    .expect("the main run loop accepts a diagnostic observer");
    let run_loop = CFRunLoop::main().expect("the main native run loop exists");
    // SAFETY: Core Foundation exports this immutable mode for the lifetime of the process.
    let common_modes = unsafe { kCFRunLoopCommonModes };
    run_loop.add_observer(Some(&observer), common_modes);
    application.run();
    // The read-only observer is scoped to this actual run and must not survive delegate cleanup.
    run_loop.remove_observer(Some(&observer), common_modes);
    observer.invalidate();
    // Stop routing before releasing the delegate, even when native verification produced an error.
    application.setDelegate(None);
    DELEGATE.with_borrow_mut(|slot| slot.take());
    let result = delegate
        .ivars()
        .result
        .borrow_mut()
        .take()
        .unwrap_or_else(|| Err("native loop ended without control evidence".into()));
    match result {
        Ok(()) => {
            // Success is reserved for input completion and release of every tracked graph node.
            println!("plain_textkit_control=ok");
        }
        Err(error) => {
            // A strict control failure remains visible to shell orchestration and full migration gates.
            eprintln!("plain_textkit_control=failed: {error}");
            std::process::exit(1);
        }
    }
}
