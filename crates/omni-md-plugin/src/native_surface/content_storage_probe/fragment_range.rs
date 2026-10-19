//! 仅合成归因：保留完整物理paragraph，标准fragment只领取原element内的真实有限范围。

use super::*;
use objc2_app_kit::{NSTextLayoutFragment, NSTextLayoutManagerDelegate};
use objc2_foundation::NSComparisonResult;

/// provider独占delegate的实际寿命；manager及delegate均不反向强持provider或正文。
pub(super) struct FragmentProbeOwner {
    delegate: Retained<FragmentRangeFactory>,
    manager: Weak<NSTextLayoutManager>,
}

impl Drop for FragmentProbeOwner {
    fn drop(&mut self) {
        if let Some(manager) = self.manager.load() {
            let expected =
                ProtocolObject::<dyn NSTextLayoutManagerDelegate>::from_ref(&*self.delegate);
            if manager
                .delegate()
                .is_some_and(|actual| std::ptr::eq(actual.as_ref(), expected))
            {
                manager.setDelegate(None); // 仅撤销原安装，不能清掉后来接管manager的delegate。
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
    // SAFETY: 只实现公开factory；位置和完整paragraph仍来自原provider，不接管输入或排版算法。
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = FragmentRangeIvars]
    struct FragmentRangeFactory;
    // SAFETY: NSObject协议没有额外义务，所有原生访问和释放均限主线程。
    unsafe impl NSObjectProtocol for FragmentRangeFactory {}
    // SAFETY: 必需非空返回与协议ABI一致；合成失败明确抛原生异常，不提供默认整段或伪空片。
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
        let provider = self
            .ivars()
            .provider
            .load()
            .ok_or(ProviderFailure::Source)?;
        let storage = self.ivars().storage.load().ok_or(ProviderFailure::Source)?;
        if !std::rc::Weak::ptr_eq(&self.ivars().document, &provider.ivars().document)
            || !std::rc::Weak::ptr_eq(&self.ivars().document, &storage.ivars().document.borrow())
        {
            return Err(ProviderFailure::Source); // 安装授权固定原doc/source身份，不能接受重新绑定的同一个shell。
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
            return Err(ProviderFailure::Stale); // 新回调不授权用户编辑后的g>0或另一长度的源。
        }
        // factory回调是新请求；入口只用Rust标量签该次派生状态，随后Native全程坚持这一张票。
        // view初始化的合法宽度/选择变化不再误用安装票核验，也不能在同一请求中途补签。
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
            return Err(ProviderFailure::Stale); // 合成只读请求不接输入暂态，首个Native调用前已核原票。
        }
        let observed_storage = ticket
            .native("fragment.provider_storage", || {
                provider.ivars().storage.load()
            })?
            .ok_or(ProviderFailure::Source)?;
        if !std::ptr::eq::<PieceBackedMarkdownTextStorage>(
            &*observed_storage,
            &*ticket.storage,
        ) {
            return Err(ProviderFailure::Source); // provider也必须仍观察原安装storage，而非同坐标后继对象。
        }
        let actual_manager = ticket
            .native("fragment.weak_manager", || self.ivars().manager.load())?
            .ok_or(ProviderFailure::Graph)?;
        if !std::ptr::eq(actual_manager.as_ref(), manager) {
            return Err(ProviderFailure::Graph);
        }
        let delegate = ticket.native("fragment.delegate", || manager.delegate())?;
        let expected = ProtocolObject::<dyn NSTextLayoutManagerDelegate>::from_ref(self);
        if !delegate.is_some_and(|actual| std::ptr::eq(actual.as_ref(), expected)) {
            return Err(ProviderFailure::Graph); // 不为另一个delegate已接管的图发布片段。
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
            return Err(ProviderFailure::Location); // 同数值offset仍须反查原公开位置，不能接受异图坐标。
        }
        if start != 0
            || end != ticket.key.length
            || requested >= end
            || element.downcast_ref::<NSTextParagraph>().is_none()
        {
            return Err(ProviderFailure::Range); // 本实验只承诺已验证完整单物理段落，不把EOF空片猜成源覆盖。
        }
        let source = {
            let document = ticket.document.upgrade().ok_or(ProviderFailure::Source)?;
            let document = document.borrow();
            finite_source_grid(end, requested, |offset| {
                document.byte_offset_for_utf16(offset).is_ok()
            })
            .ok_or(ProviderFailure::Range)?
        }; // 只查有限scalar端点，不读取正文或保持Core借用跨Native调用。
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
            return Err(ProviderFailure::Range); // 必须真实保留所给子范围，不能由系统扩回整段仍说有限。
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

/// Root在storage/manager连接后、创建NSTextView前调用；三个显式开关之外不改变任何对象。
pub(in crate::native_surface) fn install_fragment_range_probe(
    content: &NSTextContentStorage,
    manager: &NSTextLayoutManager,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if !receiver_observation_requested()
        || std::env::var_os("OMNI_NATIVE_FRAGMENT_RANGE_PROBE").as_deref()
            != Some(std::ffi::OsStr::new("1"))
    {
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
    // SAFETY: 原票和weak图已写入全部ivars；NSObject init不会读取正文或初始化另一套storage。
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
    ticket
        .native("fragment.install_set", || {
            manager.setDelegate(Some(
                ProtocolObject::<dyn NSTextLayoutManagerDelegate>::from_ref(&*owner.delegate),
            ))
        })
        .map_err(|_| "fragment安装期间原票失效")?;
    // 安装返回后再次核原票；局部owner失败析构会按精确delegate身份清理，不留下悬空weak委托。
    let installed = ticket
        .native("fragment.installed_delegate", || manager.delegate())
        .map_err(|_| "fragment安装返回后原票失效")?;
    let expected = ProtocolObject::<dyn NSTextLayoutManagerDelegate>::from_ref(&*owner.delegate);
    if !installed.is_some_and(|actual| std::ptr::eq(actual.as_ref(), expected)) {
        return Err("fragment安装返回后delegate身份改变".into());
    }
    let previous = provider.ivars().fragment_probe.borrow_mut().replace(owner);
    drop(previous); // RefCell借用已结束，潜在Native清理不在借用栈内。
    eprintln!(
        "content_storage_probe stage=fragment_range_installed limit=4096 synthetic_only=true read_only=true parity=false diagnostic_formal=false"
    );
    Ok(())
}

fn throw_fragment_failure(failure: ProviderFailure) -> ! {
    eprintln!(
        "content_storage_probe stage=fragment_range_failed failure={} synthetic_only=true diagnostic_formal=false",
        failure.code()
    );
    let class = objc2::runtime::AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: 仅显式合成模式可到此；公开NSException工厂的Exception类型遵守objc2异常包装ABI，
    // 固定原因无正文，不恢复whole段fallback或继续布局，也不进行未知对象转型。
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: ns_string!("NSInternalInconsistencyException"), reason: ns_string!("Synthetic physical paragraph fragment-range contract failed"), userInfo: None::<&AnyObject>]
    };
    objc2::exception::throw(exception)
}
