//! Synthetic attribution only: retain the complete physical paragraph while asking a standard
//! fragment to use an actual finite range inside that original element. Its finite range alone
//! proves neither bounded framework text access nor the default editor's lazy rendering.

use super::*;
use objc2_app_kit::{NSTextLayoutFragment, NSTextLayoutManagerDelegate};
use objc2_foundation::NSComparisonResult;

/// The provider exclusively owns delegate lifetime. Neither manager nor delegate holds a strong
/// backlink to the provider or canonical document.
pub(super) struct FragmentProbeOwner {
    delegate: Retained<FragmentRangeFactory>,
    manager: Weak<NSTextLayoutManager>,
}

impl Drop for FragmentProbeOwner {
    fn drop(&mut self) {
        // Upgrade only the original weak manager and clear only this installed delegate. A
        // successor delegate belongs to its own operation and must survive this owner's Drop.
        if let Some(manager) = self.manager.load() {
            let expected =
                ProtocolObject::<dyn NSTextLayoutManagerDelegate>::from_ref(&*self.delegate);
            if manager
                .delegate()
                .is_some_and(|actual| std::ptr::eq(actual.as_ref(), expected))
            {
                manager.setDelegate(None); // Revoke this exact installation, without clearing a later manager owner.
            }
        }
    }
}

pub(super) struct FragmentRangeIvars {
    provider: Weak<NativeMarkdownContentStorageProbe>,
    manager: Weak<NSTextLayoutManager>,
    storage: Weak<PieceBackedMarkdownTextStorage>,
    document: WeakMarkdownDocument,
    installed_key: ContentKey,
    entry_logs: Cell<u8>,
    calls: Cell<u64>,
}

define_class!(
    // SAFETY: Implements the public factory only. Original provider supplies locations and the
    // complete paragraph; this delegate takes over neither input nor shaping/layout algorithms.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = FragmentRangeIvars]
    struct FragmentRangeFactory;
    // SAFETY: No extra NSObject obligations; all Native access and release remain main-thread-only.
    unsafe impl NSObjectProtocol for FragmentRangeFactory {}
    // SAFETY: The required nonnull return follows the protocol ABI. Synthetic failure throws a
    // native exception, never a default whole-paragraph fragment or fabricated empty fragment.
    unsafe impl NSTextLayoutManagerDelegate for FragmentRangeFactory {
        #[unsafe(method_id(textLayoutManager:textLayoutFragmentForLocation:inTextElement:))]
        fn fragment(
            &self,
            manager: &NSTextLayoutManager,
            location: &ProtocolObject<dyn NSTextLocation>,
            element: &NSTextElement,
        ) -> Retained<NSTextLayoutFragment> {
            self.make_fragment(manager, location, element)
                .unwrap_or_else(|failure| throw_fragment_failure(failure))
        }
    }
);

impl FragmentRangeFactory {
    fn make_fragment(
        &self,
        manager: &NSTextLayoutManager,
        location: &ProtocolObject<dyn NSTextLocation>,
        element: &NSTextElement,
    ) -> Result<Retained<NSTextLayoutFragment>, ProviderFailure> {
        // Installation binds the original weak document/storage allocation. Reusing a shell for
        // another note cannot authorize this callback, even with equal coordinates or length.
        let provider = self
            .ivars()
            .provider
            .load()
            .ok_or(ProviderFailure::Source)?;
        let storage = self.ivars().storage.load().ok_or(ProviderFailure::Source)?;
        if !std::rc::Weak::ptr_eq(&self.ivars().document, &provider.ivars().document)
            || !std::rc::Weak::ptr_eq(&self.ivars().document, &storage.ivars().document.borrow())
        {
            return Err(ProviderFailure::Source); // Original document/Source identity forbids admission through a rebound shell.
        }
        let (generation, length, selection) = {
            let document = self
                .ivars()
                .document
                .upgrade()
                .ok_or(ProviderFailure::Source)?;
            let document = document.borrow();
            (
                document.generation(),
                document.utf16_len(),
                document.selection(),
            )
        };
        let installed = self.ivars().installed_key;
        if installed.generation != 0 || generation != 0 || length != installed.length {
            return Err(ProviderFailure::Stale); // A new callback cannot admit an edited generation or another-length Source.
        }
        // A factory callback is a new request. Capture its derived state from Rust scalars at
        // entry, then preserve this same ticket across Native calls. Legal selection/width changes
        // during view initialization do not reuse stale installation state or renew mid-request authority.
        let key = ContentKey {
            generation,
            length,
            selection,
            mode: storage.presentation_mode(),
            semantic_epoch: storage.ivars().presentation_semantic_epoch.get(),
            table_epoch: storage.ivars().table_geometry_epoch.get(),
            input_epoch: storage.ivars().input_transaction_epoch.get(),
            input_depth: storage.ivars().native_input_depth.get(),
            composition: storage.ivars().composition_active.get(),
        };
        let logs = self.ivars().entry_logs.get();
        if logs < 4 {
            self.ivars().entry_logs.set(logs + 1);
            eprintln!(
                "content_storage_probe stage=fragment_range_callback_key call={} generation={} length={} selection_equal={} mode_equal={} installed_semantic={:?} semantic={:?} installed_table={} table={} installed_input={} input={} input_depth={} composition={} synthetic_only=true diagnostic_formal=false",
                logs + 1,
                generation,
                length,
                key.selection == installed.selection,
                key.mode == installed.mode,
                installed.semantic_epoch,
                key.semantic_epoch,
                installed.table_epoch,
                key.table_epoch,
                installed.input_epoch,
                key.input_epoch,
                key.input_depth,
                key.composition
            );
        }
        let ticket = ProviderTicket {
            storage,
            document: self.ivars().document.clone(),
            key,
            trace: Rc::clone(&provider.ivars().trace),
        };
        let _payload = PayloadUseGuard::enter(&ticket.storage.ivars().payload_use_depth);
        if key.input_depth != 0 || key.composition || !ticket.current() {
            return Err(ProviderFailure::Stale); // Reject transient input in this synthetic read-only request before its first Native call.
        }
        let observed_storage = ticket
            .native("fragment.provider_storage", || {
                provider.ivars().storage.load()
            })?
            .ok_or(ProviderFailure::Source)?;
        if !std::ptr::eq::<PieceBackedMarkdownTextStorage>(&*observed_storage, &*ticket.storage) {
            return Err(ProviderFailure::Source); // Provider must still observe original storage, not a same-coordinate successor object.
        }
        let actual_manager = ticket
            .native("fragment.weak_manager", || self.ivars().manager.load())?
            .ok_or(ProviderFailure::Graph)?;
        if !std::ptr::eq(actual_manager.as_ref(), manager) {
            return Err(ProviderFailure::Graph);
        }
        let delegate = ticket.native("fragment.delegate", || manager.delegate())?;
        // Exact original manager/delegate identity is independent of document coordinates.
        let expected = ProtocolObject::<dyn NSTextLayoutManagerDelegate>::from_ref(self);
        if !delegate.is_some_and(|actual| std::ptr::eq(actual.as_ref(), expected)) {
            return Err(ProviderFailure::Graph); // Do not publish into a graph already taken over by another delegate.
        }
        let (captured, origin) = ticket.native("fragment.capture", || provider.capture())??;
        if captured.key != ticket.key {
            return Err(ProviderFailure::Stale);
        }
        let original_manager = ticket
            .native("fragment.element_manager", || element.textContentManager())?
            .ok_or(ProviderFailure::Graph)?;
        if !std::ptr::eq::<objc2_app_kit::NSTextContentManager>(
            original_manager.as_ref(),
            provider.as_super().as_super(),
        ) {
            return Err(ProviderFailure::Graph);
        }
        let element_range = ticket
            .native("fragment.element_range", || element.elementRange())?
            .ok_or(ProviderFailure::Range)?;
        let element_start = ticket.native("fragment.element_start", || element_range.location())?;
        let element_end = ticket.native("fragment.element_end", || element_range.endLocation())?;
        let start = provider.offset(&ticket, &origin, &element_start)?;
        let end = provider.offset(&ticket, &origin, &element_end)?;
        let requested = provider.offset(&ticket, &origin, location)?;
        let requested_back = provider.location(&ticket, &origin, requested)?;
        if ticket.native("fragment.request_roundtrip", || {
            location.compare(&requested_back)
        })? != NSComparisonResult::Same
        {
            return Err(ProviderFailure::Location); // Equal numeric offsets still need exact original-position roundtrip; foreign graph coordinates cannot substitute.
        }
        if start != 0
            || end != ticket.key.length
            || requested >= end
            || element.downcast_ref::<NSTextParagraph>().is_none()
        {
            return Err(ProviderFailure::Range); // This experiment admits only its complete single physical paragraph, not guessed EOF-empty source coverage.
        }
        let source = {
            let document = ticket.document.upgrade().ok_or(ProviderFailure::Source)?;
            let document = document.borrow();
            finite_source_grid(end, requested, |offset| {
                document.byte_offset_for_utf16(offset).is_ok()
            })
            .ok_or(ProviderFailure::Range)?
        }; // Query finite scalar endpoints only; retain no Core borrow across later Native calls.
        let source_start = provider.location(&ticket, &origin, source.start)?;
        let source_end = provider.location(&ticket, &origin, source.end)?;
        let range = ticket
            .native("fragment.range", || {
                NSTextRange::initWithLocation_endLocation(
                    NSTextRange::alloc(),
                    &source_start,
                    Some(&source_end),
                )
            })?
            .ok_or(ProviderFailure::Range)?;
        let fragment = ticket.native("fragment.init", || {
            // The standard fragment still receives the complete original text element. Asking
            // for a subrange does not prove it will never retain or bulk-read that whole element.
            NSTextLayoutFragment::initWithTextElement_range(
                NSTextLayoutFragment::alloc(),
                element,
                Some(&range),
            )
        })?;
        provider.include_owned(&ticket, "editor.fragment_range_probe.fragment", &fragment)?;
        let actual = ticket.native("fragment.actual_range", || fragment.rangeInElement())?;
        if !ticket.native("fragment.verify_range", || {
            actual.isEqualToTextRange(&range)
        })? {
            return Err(ProviderFailure::Range); // Actual rangeInElement must preserve the requested subrange, not expand it and still claim finite range admission.
        }
        let calls = self.ivars().calls.get().saturating_add(1);
        self.ivars().calls.set(calls);
        if calls <= 8 || (calls <= 1024 && calls.is_power_of_two()) {
            eprintln!(
                "content_storage_probe stage=fragment_range_factory calls={} requested={} source_start={} source_end={} element_start={} element_end={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                calls, requested, source.start, source.end, start, end
            );
        }
        Ok(fragment)
    }
}

/// The original composition root calls this after storage/manager connection and before
/// NSTextView creation. Outside the three explicit experiment gates, change no object.
pub(in crate::native_surface) fn install_fragment_range_probe(
    content: &NSTextContentStorage,
    manager: &NSTextLayoutManager,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if !receiver_observation_requested()
        || std::env::var_os("OMNI_NATIVE_FRAGMENT_RANGE_PROBE").as_deref()
            != Some(std::ffi::OsStr::new("1"))
    {
        // Ordinary TextKit reference graphs retain their existing manager/delegate unchanged.
        return Ok(());
    }
    let provider = content
        .downcast_ref::<NativeMarkdownContentStorageProbe>()
        .ok_or("fragment范围实验要求原virtual provider")?;
    if !provider.ivars().virtual_paragraph || provider.ivars().fragment_probe.borrow().is_some() {
        return Err("fragment范围实验provider或安装状态无效".into());
    }
    let (ticket, _) = provider
        .capture()
        .map_err(|_| "fragment范围实验无法捕获原票")?;
    let _payload = PayloadUseGuard::enter(&ticket.storage.ivars().payload_use_depth);
    if ticket.key.generation != 0 || ticket.key.input_depth != 0 || ticket.key.composition {
        return Err("fragment范围实验只允许原g0静止合成文档".into());
    }
    if ticket
        .native("fragment.install_delegate", || manager.delegate())
        .map_err(|_| "fragment安装原票失效")?
        .is_some()
    {
        // Do not overwrite an actual pre-existing delegate or another rendering experiment.
        return Err("fragment范围实验不能覆盖已有delegate或Raster".into());
    }
    let primary = ticket
        .native("fragment.install_primary", || {
            provider.primaryTextLayoutManager()
        })
        .map_err(|_| "fragment安装原票失效")?
        .ok_or("fragment安装无原manager")?;
    if !std::ptr::eq(primary.as_ref(), manager) {
        return Err("fragment安装manager不是原provider主图".into());
    }
    let allocated = ticket
        .native("fragment.alloc", || {
            FragmentRangeFactory::alloc(mtm).set_ivars(FragmentRangeIvars {
                provider: Weak::from(provider),
                manager: Weak::from(manager),
                storage: Weak::from_retained(&ticket.storage),
                document: ticket.document.clone(),
                installed_key: ticket.key,
                entry_logs: Cell::new(0),
                calls: Cell::new(0),
            })
        })
        .map_err(|_| "fragment分配期间原票失效")?;
    // SAFETY: Original ticket and weak graph are installed in all ivars. NSObject init reads no
    // text and initializes no second storage graph.
    let delegate: Retained<FragmentRangeFactory> = ticket
        .native("fragment.delegate_init", || unsafe {
            msg_send![super(allocated), init]
        })
        .map_err(|_| "fragment初始化期间原票失效")?;
    provider
        .include_owned(&ticket, "editor.fragment_range_probe.delegate", &delegate)
        .map_err(|_| "fragment delegate无法纳入原ledger")?;
    let owner = FragmentProbeOwner {
        delegate,
        manager: Weak::from(manager),
    };
    // Publish the actual weak delegate only while its strong local owner already exists. Every
    // subsequent failure drops that owner and removes only this exact installation.
    ticket
        .native("fragment.install_set", || {
            manager.setDelegate(Some(
                ProtocolObject::<dyn NSTextLayoutManagerDelegate>::from_ref(&*owner.delegate),
            ))
        })
        .map_err(|_| "fragment安装期间原票失效")?;
    // Recheck original authority and actual delegate after installation returns. Local-owner
    // failure cleanup uses exact identity, leaving no dangling weak delegate or successor mutation.
    let installed = ticket
        .native("fragment.installed_delegate", || manager.delegate())
        .map_err(|_| "fragment安装返回后原票失效")?;
    let expected = ProtocolObject::<dyn NSTextLayoutManagerDelegate>::from_ref(&*owner.delegate);
    if !installed.is_some_and(|actual| std::ptr::eq(actual.as_ref(), expected)) {
        return Err("fragment安装返回后delegate身份改变".into());
    }
    let previous = provider.ivars().fragment_probe.borrow_mut().replace(owner);
    drop(previous); // The RefCell borrow ended before any possible Native cleanup/reentry.
    eprintln!(
        "content_storage_probe stage=fragment_range_installed limit=4096 synthetic_only=true read_only=true parity=false diagnostic_formal=false"
    );
    Ok(())
}

fn throw_fragment_failure(failure: ProviderFailure) -> ! {
    // Preserve failure as an explicit experiment outcome, without fallback, another fragment
    // attempt, or successful default-product attribution.
    eprintln!(
        "content_storage_probe stage=fragment_range_failed failure={} synthetic_only=true diagnostic_formal=false",
        failure.code()
    );
    let class = objc2::runtime::AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: Only explicit synthetic mode reaches this public NSException factory, whose
    // Exception type follows objc2's exception ABI. Fixed reason contains no text; do not recover
    // into whole-paragraph fallback, continue layout, or cast an unknown exception object.
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: ns_string!("NSInternalInconsistencyException"), reason: ns_string!("Synthetic physical paragraph fragment-range contract failed"), userInfo: None::<&AnyObject>]
    };
    objc2::exception::throw(exception)
}
