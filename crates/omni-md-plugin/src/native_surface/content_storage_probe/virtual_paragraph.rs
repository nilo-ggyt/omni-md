//! 仅合成只读单段实验：完整物理段落保留原坐标，属性值通过公开primitive按需读取原适配器。

use super::*;
use objc2::Message;
use objc2::runtime::NSZone;

mod slice;
mod string_slice;
use slice::Utf16Slice;
use string_slice::SyntheticVirtualStringSlice;

const SYNTHETIC_GIANT_BYTES: usize = 345_112;
const SYNTHETIC_GIANT_UTF16: usize = 165_042;

/// 原票只保存Weak和标量；Native对象存活不延长canonical document或piece root寿命。
struct VirtualBinding {
    storage: Weak<PieceBackedMarkdownTextStorage>,
    document: WeakMarkdownDocument,
    key: ContentKey,
    trace: Rc<ProviderPhaseTrace>,
    ledger: std::rc::Weak<NativeMarkdownOwnedLedger>,
    counts: VirtualCounts,
    attributed_substring_logs: Cell<u8>,
    string_substring_logs: Cell<u8>,
}

impl VirtualBinding {
    fn ticket(&self) -> ProviderTicket {
        let Some(storage) = self.storage.load() else {
            throw_virtual_contract(); // 已关闭的原适配器不能换成后继正文或一个伪空值。
        };
        let ticket = ProviderTicket {
            storage,
            document: self.document.clone(),
            key: self.key,
            trace: Rc::clone(&self.trace),
        };
        if !ticket.current() {
            throw_virtual_contract(); // 本实验只承诺原只读票；任何编辑/模式变化均明确失败，不改变旧值。
        }
        ticket
    }

    fn include_owned(
        &self,
        ticket: &ProviderTicket,
        role: &'static str,
        object: &AnyObject,
    ) -> Result<(), ProviderFailure> {
        let ledger = self.ledger.upgrade().ok_or(ProviderFailure::Ledger)?;
        ticket
            .native("virtual.owned_include", || ledger.include(role, object))?
            .map_err(|_| ProviderFailure::Ledger) // 每个新slice也必须进入原总预算，不用小对象名义跳过释放验证。
    }

    fn log_substring(&self, attributed: bool, requested: NSRange, actual: Utf16Slice) {
        let logs = if attributed {
            &self.attributed_substring_logs
        } else {
            &self.string_substring_logs
        };
        let count = logs.get();
        if count < 4 {
            logs.set(count + 1);
            // 同一原票的所有slice共享预算，每种substring只前四条，避免每个新对象重新刷日志。
            eprintln!(
                "content_storage_probe stage=virtual_substring kind={} location={} length={} global_location={} global_length={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                if attributed { "attributed" } else { "string" },
                requested.location,
                requested.length,
                actual.start,
                actual.length,
            );
        }
    }

    fn checked<T>(&self, phase: &'static str, operation: impl FnOnce() -> T) -> T {
        let ticket = self.ticket();
        match ticket.native(phase, operation) {
            Ok(value) => value,
            Err(_) => throw_virtual_contract(), // getter重入撤权不能发布后继内容或缩短原长度。
        }
    }
}

#[derive(Default)]
struct VirtualCounts {
    copy_calls: Cell<u64>,
    full_substrings: Cell<u64>,
    partial_substrings: Cell<u64>,
    attribute_reads: Cell<u64>,
    substring_entries: Cell<u64>,
    string_copy_entries: Cell<u8>,
    longest_calls: Cell<u64>,
    longest_null: Cell<u64>,
    longest_nonnull: Cell<u64>,
}

impl Drop for VirtualBinding {
    fn drop(&mut self) {
        // 同一原票所有派生值只汇总一次；正文bulk仍由原string_read_diagnostic计量，不从共享次数推断零复制。
        eprintln!(
            "content_storage_probe stage=virtual_attributed_drop copy_calls={} full_substring_shares={} partial_substrings={} attribute_reads={} substring_entries={} longest_calls={} longest_null={} longest_nonnull={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
            self.counts.copy_calls.get(),
            self.counts.full_substrings.get(),
            self.counts.partial_substrings.get(),
            self.counts.attribute_reads.get(),
            self.counts.substring_entries.get(),
            self.counts.longest_calls.get(),
            self.counts.longest_null.get(),
            self.counts.longest_nonnull.get(),
        );
    }
}

pub(super) struct VirtualAttributedIvars {
    binding: Rc<VirtualBinding>,
    source_string: Retained<NSString>,
    view_string: Retained<NSString>,
    slice: Utf16Slice,
}

define_class!(
    // SAFETY: 实现NSAttributedString公开两个primitive；原只读票内返回真实slice，不持有正文镜像。
    #[unsafe(super = NSAttributedString)]
    #[thread_kind = MainThreadOnly]
    #[ivars = VirtualAttributedIvars]
    struct SyntheticVirtualAttributedString;

    // SAFETY: NSObject协议无额外义务；本对象和所有Native访问均限原主线程图。
    unsafe impl NSObjectProtocol for SyntheticVirtualAttributedString {}

    impl SyntheticVirtualAttributedString {
        #[unsafe(method_id(string))]
        #[unsafe(method_family = none)]
        fn virtual_string(&self) -> Retained<NSString> {
            self.ivars().binding.checked("virtual.attr_string", || {
                self.ivars().view_string.clone() // full和partial都用真实范围的immutable视图，底层仍是唯一原source。
            })
        }

        #[unsafe(method(length))]
        fn virtual_length(&self) -> NSUInteger {
            let _ticket = self.ivars().binding.ticket();
            self.ivars().slice.length // 原value/subrange的精确UTF16长度，不用viewport或虚假fullrange替换。
        }

        #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
        #[unsafe(method_family = none)]
        /// # Safety
        /// effective_range须为null或本同步调用期间可写的NSRange指针。
        unsafe fn virtual_attributes(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
        ) -> Retained<MarkdownTextAttributes> {
            let ticket = self.ivars().binding.ticket();
            let global_index = self.ivars().slice.index(index).unwrap_or_else(|| throw_virtual_range());
            let reads = self.ivars().binding.counts.attribute_reads.get().saturating_add(1);
            self.ivars().binding.counts.attribute_reads.set(reads);
            if reads <= 262_144 && reads.is_power_of_two() {
                // 所有真实slice共用同一累计计数；1到2^18最多19条，不添加属性查询。
                eprintln!(
                    "content_storage_probe stage=virtual_attribute_reads attribute_reads={} original_key_length={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                    reads,
                    ticket.key.length,
                );
            }
            let mut global_effective = NSRange::new(0, 0);
            let output: *mut NSRange = if effective_range.is_null() {
                std::ptr::null_mut() // caller不需要范围时原getter也接收null，不额外索取属性metadata。
            } else {
                &mut global_effective
            };
            let attributes = match ticket.native("virtual.attr_attributes", || {
                // SAFETY: global_index真实属于原storage；输出为null或活到同步返回的栈上range，不跨状态借用。
                unsafe { ticket.storage.as_super().attributesAtIndex_effectiveRange(global_index, output) }
            }) {
                Ok(attributes) => attributes,
                Err(_) => throw_virtual_contract(), // 撤权不能发布半旧半新字体/颜色。
            };
            if !effective_range.is_null() {
                let (location, length) = self.ivars().slice.local_effective_range(
                    index, global_effective.location, global_effective.length, ticket.key.length,
                ).unwrap_or_else(|| throw_virtual_contract());
                // SAFETY: caller提供可写NSRange；真实global run与本slice相交后转local，且仍覆盖本次index。
                unsafe { effective_range.write(NSRange::new(location, length)) };
            }
            attributes // 唯一尾表达式交给method_id按非copy selector移交，不再额外retain字典。
        }

        #[unsafe(method_id(attributesAtIndex:longestEffectiveRange:inRange:))]
        #[unsafe(method_family = none)]
        /// # Safety
        /// effective_range须为null或本同步调用期间可写的NSRange指针。
        unsafe fn virtual_longest_attributes(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
            range_limit: NSRange,
        ) -> Retained<MarkdownTextAttributes> {
            let _ticket = self.ivars().binding.ticket();
            if self.ivars().slice.index(index).is_none()
                || self.ivars().slice.subrange(range_limit.location, range_limit.length).is_none()
            {
                throw_virtual_range(); // NULL仅免除求最长范围，不能免除原index/rangeLimit的公开越界异常。
            }
            let counts = &self.ivars().binding.counts;
            let calls = counts.longest_calls.get().saturating_add(1);
            counts.longest_calls.set(calls);
            if effective_range.is_null() {
                counts.longest_null.set(counts.longest_null.get().saturating_add(1));
            } else {
                counts.longest_nonnull.set(counts.longest_nonnull.get().saturating_add(1));
            }
            if calls <= 4 || (calls <= 262_144 && calls.is_power_of_two()) {
                // 同一原票全部slice共享固定首4次/2幂预算，观察NULL占比而不新增属性请求。
                eprintln!(
                    "content_storage_probe stage=virtual_longest_attributes calls={} null={} nonnull={} index={} limit_location={} limit_length={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                    calls, counts.longest_null.get(), counts.longest_nonnull.get(),
                    index, range_limit.location, range_limit.length,
                );
            }
            if effective_range.is_null() {
                // caller不接收最长范围时，属性值只由真实index决定；无需Foundation向相邻run扫描。
                // SAFETY: index已验证，NULL是公开primitive允许的输出值；该primitive仍完整执行同票检查。
                unsafe { self.as_super().attributesAtIndex_effectiveRange(index, std::ptr::null_mut()) }
            } else {
                // SAFETY: 原index/rangeLimit与caller可写输出原样单次交给super；仍求真实最大同值区间，
                // 不把有限canonical窗口或不同字典身份伪装成Foundation最长范围。
                self.ivars().binding.checked("virtual.attr_longest", || unsafe {
                    msg_send![super(self), attributesAtIndex: index, longestEffectiveRange: effective_range, inRange: range_limit]
                })
            } // 两分支共享method_id尾表达式，按noncopy返回家族移交同一结果，不额外retain。
        }

        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn immutable_copy(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.ivars().binding.checked("virtual.attr_copy", || {
                let counts = &self.ivars().binding.counts;
                counts.copy_calls.set(counts.copy_calls.get().saturating_add(1));
                self.retain() // copy家族只移交这一个+1；同一只读值没有字符复制。
            })
        }

        #[unsafe(method_id(attributedSubstringFromRange:))]
        #[unsafe(method_family = none)]
        fn virtual_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
            let entries = self.ivars().binding.counts.substring_entries.get().saturating_add(1);
            self.ivars().binding.counts.substring_entries.set(entries);
            if entries <= 4 && super::receiver_observation_requested() {
                // 入口计数在首个ticket/Weak Native加载前，区分未调用override与已进入但尚未返回。
                eprintln!(
                    "content_storage_probe stage=virtual_substring_entry call={} receiver=virtual_attributed location={} length={} synthetic_only=true diagnostic_formal=false",
                    entries, range.location, range.length,
                );
            }
            self.requested_substring(range) // 所有真实partial范围都返回独立虚拟值，不再经super先fullcopy。
        }
    }
);

impl SyntheticVirtualAttributedString {
    fn new(
        provider: &NativeMarkdownContentStorageProbe,
        ticket: &ProviderTicket,
        binding: Rc<VirtualBinding>,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>, ProviderFailure> {
        let source_string = ticket.native("virtual.original_string", || {
            ticket.storage.as_super().string()
        })?;
        let slice = Utf16Slice::new(0, ticket.key.length, ticket.key.length)
            .ok_or(ProviderFailure::Range)?;
        let view_source = ticket.native("virtual.slice_source_retain", || source_string.clone())?;
        // 完整物理段落也经同一immutable NSString视图；范围仍是真实0..EOF，不切段或缩短bulk请求。
        // 新视图由已有factory纳入原ledger，保留同票只读约束和原source的完整计量路径。
        let view_string =
            SyntheticVirtualStringSlice::create(Rc::clone(&binding), view_source, slice, mtm)?;
        let allocated = ticket.native("virtual.attr_alloc", || {
            Self::alloc(mtm).set_ivars(VirtualAttributedIvars {
                binding,
                source_string,
                view_string,
                slice,
            })
        })?;
        // SAFETY: 自定义NSAttributedString的必需primitive和ivars已就绪；仅父类init，不传完整正文值。
        let value: Retained<Self> = ticket.native("virtual.attr_init", || unsafe {
            msg_send![super(allocated), init]
        })?;
        if !std::ptr::eq(value.class(), Self::class()) {
            return Err(ProviderFailure::Graph); // class-cluster若替换对象，不访问其它类的Rust ivars。
        }
        provider.include_owned(ticket, "editor.virtual_paragraph.attributed", &value)?;
        Ok(value)
    }

    fn new_slice(
        binding: Rc<VirtualBinding>,
        source_string: Retained<NSString>,
        slice: Utf16Slice,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSAttributedString>, ProviderFailure> {
        let ticket = binding.ticket();
        let source = ticket.native("virtual.slice_source_retain", || source_string.clone())?;
        let view_string =
            SyntheticVirtualStringSlice::create(Rc::clone(&binding), source, slice, mtm)?;
        let allocated = ticket.native("virtual.attr_slice_alloc", || {
            Self::alloc(mtm).set_ivars(VirtualAttributedIvars {
                binding: Rc::clone(&binding),
                source_string,
                view_string,
                slice,
            })
        })?;
        // SAFETY: 两个公开primitive和真实slice已就绪；初始化不扫描或复制原source。
        let value: Retained<Self> = ticket.native("virtual.attr_slice_init", || unsafe {
            msg_send![super(allocated), init]
        })?;
        if !std::ptr::eq(value.class(), Self::class()) {
            return Err(ProviderFailure::Graph); // 不用错误class继续提供虚拟属性值。
        }
        binding.include_owned(&ticket, "editor.virtual_paragraph.attributed_slice", &value)?;
        Ok(value.into_super())
    }

    fn requested_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
        let ticket = self.ivars().binding.ticket();
        let actual = self
            .ivars()
            .slice
            .subrange(range.location, range.length)
            .unwrap_or_else(|| throw_virtual_range());
        self.ivars().binding.log_substring(true, range, actual);
        if actual == self.ivars().slice {
            self.ivars().binding.checked("virtual.attr_full_substring", || {
                let counts = &self.ivars().binding.counts;
                let count = counts.full_substrings.get().saturating_add(1);
                counts.full_substrings.set(count);
                if count == 1 {
                    eprintln!(
                        "content_storage_probe stage=virtual_full_substring_shared count=1 source_start={} source_end={} synthetic_only=true read_only=true parity=false diagnostic_formal=false bulk_result=unmeasured",
                        actual.start, actual.end(),
                    );
                }
                self.retain().into_super() // 只共享恰好当前值的真实完整范围，noncopy返回由method_id处理。
            })
        } else {
            let counts = &self.ivars().binding.counts;
            counts
                .partial_substrings
                .set(counts.partial_substrings.get().saturating_add(1));
            let source = ticket
                .native("virtual.slice_source_retain", || {
                    self.ivars().source_string.clone()
                })
                .unwrap_or_else(|_| throw_virtual_contract());
            let mtm = MainThreadMarker::new().unwrap_or_else(|| throw_virtual_contract());
            Self::new_slice(Rc::clone(&self.ivars().binding), source, actual, mtm)
                .unwrap_or_else(|_| throw_virtual_contract()) // 原source+global offset/len，不复制也不链式保留旧slice。
        }
    }
}

pub(super) struct VirtualParagraphIvars {
    binding: Rc<VirtualBinding>,
    value: Retained<SyntheticVirtualAttributedString>,
    content: Retained<NSTextRange>,
    separator: Retained<NSTextRange>,
}

define_class!(
    // SAFETY: 完整真实单段的只读属性由不可变值和原provider全局范围提供，不调用私有setter或插入分隔符。
    #[unsafe(super = NSTextParagraph)]
    #[thread_kind = MainThreadOnly]
    #[ivars = VirtualParagraphIvars]
    struct SyntheticVirtualPhysicalParagraph;

    // SAFETY: 无额外NSObject协议义务；manager依旧是继承的weak属性，不建立provider强环。
    unsafe impl NSObjectProtocol for SyntheticVirtualPhysicalParagraph {}

    impl SyntheticVirtualPhysicalParagraph {
        #[unsafe(method_id(attributedString))]
        #[unsafe(method_family = none)]
        fn virtual_value(&self) -> Retained<NSAttributedString> {
            self.ivars().binding.checked("virtual.paragraph_value", || {
                self.ivars().value.clone().into_super() // getter只保留同一个值，不经NSConcreteAttributedString复制。
            })
        }

        #[unsafe(method_id(paragraphContentRange))]
        #[unsafe(method_family = none)]
        fn virtual_content_range(&self) -> Option<Retained<NSTextRange>> {
            Some(self.ivars().binding.checked("virtual.paragraph_content", || {
                self.ivars().content.clone() // 只读fixture无段落分隔符，内容精确覆盖真实0..EOF。
            }))
        }

        #[unsafe(method_id(paragraphSeparatorRange))]
        #[unsafe(method_family = none)]
        fn virtual_separator_range(&self) -> Option<Retained<NSTextRange>> {
            Some(self.ivars().binding.checked("virtual.paragraph_separator", || {
                self.ivars().separator.clone() // 真实EOF没有终止字符，空范围仍有有效的原provider两端位置。
            }))
        }
    }
);

/// 只支持Root生成且保证无CR/LF/PS的已知只读巨段fixture；line_count1不被当作无PS的证明。
pub(super) fn build(
    provider: &NativeMarkdownContentStorageProbe,
    ticket: &ProviderTicket,
    origin: &ProtocolObject<dyn NSTextLocation>,
) -> Result<Retained<NSTextParagraph>, ProviderFailure> {
    if std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_SYNTHETIC_EXCEPTION").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Err(ProviderFailure::Source); // 必须是harness明确授权的合成输入，不把用户笔记送入实验provider。
    }
    let document = ticket.document.upgrade().ok_or(ProviderFailure::Source)?;
    let supported = {
        let document = document.borrow();
        document.byte_len() == SYNTHETIC_GIANT_BYTES
            && document.utf16_len() == SYNTHETIC_GIANT_UTF16
            && document.line_count() == 1
            && document.generation() == 0
            && ticket.key.input_depth == 0
            && !ticket.key.composition
    };
    drop(document); // 借用和临时canonical owner必须在后续Native初始化前结束。
    if !supported || !ticket.current() {
        return Err(ProviderFailure::Range); // 不扫描全文寻找替代证明，也不悄悄退回有限假段落。
    }
    let mtm = MainThreadMarker::new().ok_or(ProviderFailure::Graph)?;
    let binding = Rc::new(VirtualBinding {
        storage: Weak::from_retained(&ticket.storage),
        document: ticket.document.clone(),
        key: ticket.key,
        trace: Rc::clone(&ticket.trace),
        ledger: provider.ivars().ledger.clone(),
        counts: VirtualCounts::default(),
        attributed_substring_logs: Cell::new(0),
        string_substring_logs: Cell::new(0),
    });
    let value = SyntheticVirtualAttributedString::new(provider, ticket, Rc::clone(&binding), mtm)?;
    let start = provider.location(ticket, origin, 0)?;
    let end = provider.location(ticket, origin, ticket.key.length)?;
    let content = original_range(
        provider,
        ticket,
        &start,
        &end,
        "editor.virtual_paragraph.content",
    )?;
    let separator = original_range(
        provider,
        ticket,
        &end,
        &end,
        "editor.virtual_paragraph.separator",
    )?;
    let allocated = ticket.native("virtual.paragraph_alloc", || {
        SyntheticVirtualPhysicalParagraph::alloc(mtm).set_ivars(VirtualParagraphIvars {
            binding,
            value: value.clone(),
            content: content.clone(),
            separator,
        })
    })?;
    // SAFETY: 原属性值是本类已初始化的不可变公开NSAttributedString子类；super仍执行指定初始化器一次。
    let paragraph: Retained<SyntheticVirtualPhysicalParagraph> = ticket
        .native("virtual.paragraph_init", || unsafe {
            msg_send![super(allocated), initWithAttributedString: &*value]
        })?;
    if !std::ptr::eq(
        paragraph.class(),
        SyntheticVirtualPhysicalParagraph::class(),
    ) {
        return Err(ProviderFailure::Graph); // 不访问意外替代对象的Rust ivars。
    }
    provider.include_owned(ticket, "editor.virtual_paragraph.paragraph", &paragraph)?;
    ticket.native("virtual.paragraph_range", || {
        paragraph.setElementRange(Some(&content))
    })?;
    ticket.native("virtual.paragraph_manager", || {
        paragraph.setTextContentManager(Some(provider.as_super().as_super()))
    })?;
    eprintln!(
        "content_storage_probe stage=virtual_physical_paragraph source_start=0 source_end={} synthetic_only=true read_only=true parity=false diagnostic_formal=false whole_bulk_forwarded=true",
        ticket.key.length,
    );
    Ok(paragraph.into_super())
}

fn original_range(
    provider: &NativeMarkdownContentStorageProbe,
    ticket: &ProviderTicket,
    start: &ProtocolObject<dyn NSTextLocation>,
    end: &ProtocolObject<dyn NSTextLocation>,
    role: &'static str,
) -> Result<Retained<NSTextRange>, ProviderFailure> {
    let allocated = ticket.native("virtual.range_alloc", NSTextRange::alloc)?;
    let range = ticket
        .native("virtual.range_init", || {
            NSTextRange::initWithLocation_endLocation(allocated, start, Some(end))
        })?
        .ok_or(ProviderFailure::Location)?;
    provider.include_owned(ticket, role, &range)?;
    Ok(range)
}

fn throw_virtual_contract() -> ! {
    let class = objc2::runtime::AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: 只调用公开NSException工厂；固定名称/原因不含源内容，nil userInfo符合公开契约。
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class,
            exceptionWithName: ns_string!("NSInternalInconsistencyException"),
            reason: ns_string!("Synthetic virtual paragraph read-only ticket is no longer current"),
            userInfo: None::<&AnyObject>]
    };
    objc2::exception::throw(exception) // 原票失效就是实验失败，不能返回伪空字符串或后继数据。
}

fn throw_virtual_range() -> ! {
    let class = objc2::runtime::AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: 仅公开NSException工厂；无效NSString/NSAttributedString范围必须异常，不截断成合法片段。
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class,
            exceptionWithName: ns_string!("NSRangeException"),
            reason: ns_string!("Synthetic virtual UTF16 slice request is outside the original value"),
            userInfo: None::<&AnyObject>]
    };
    objc2::exception::throw(exception)
}

/// 不触发类注册或任何数据getter；NAME与实际注册类名由同一个ClassType契约给出。
pub(super) fn virtual_attributed_receiver_is_ours(value: &NSAttributedString) -> bool {
    value.class().name().to_bytes() == SyntheticVirtualAttributedString::NAME.as_bytes()
}
