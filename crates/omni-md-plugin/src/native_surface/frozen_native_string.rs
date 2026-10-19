//! 独立不可变NSString值：真实UTF-16子范围共享Core快照，不借活动文档或可撤销适配器。

use std::ops::Range;
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::frozen_native_source::{FrozenNativeSource, FrozenReadKind};
use super::frozen_value_family::{FrozenValueClaim, FrozenValueFamilyError};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSZone};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSObjectProtocol, NSRange, NSString, NSUInteger, ns_string, unichar};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// Rust构造入口先拒绝非法范围；此时尚未创建任何Foundation值。
pub(super) enum FrozenMarkdownStringError {
    #[error("不可变文本范围越过快照边界")]
    RangeOutsideSnapshot,
    #[error("系统没有保留不可变文本的实际子类")]
    NativeClassChanged,
    #[error("不可变字符串无法登记真实生命周期：{0}")]
    Family(#[from] FrozenValueFamilyError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 真实快照坐标允许代理对内部端点，保持NSString按UTF-16单元切片的原合同。
struct FrozenUtf16Range {
    start: usize,
    length: usize,
}

impl FrozenUtf16Range {
    fn new(range: Range<usize>, source_length: usize) -> Option<Self> {
        // 倒置或越界必须先拒绝，不能用饱和运算把非法请求改成空值。
        let length = range.end.checked_sub(range.start)?;
        (range.end <= source_length).then_some(Self {
            start: range.start,
            length,
        })
    }

    fn subrange(self, range: NSRange) -> Option<Self> {
        let local_end = range.location.checked_add(range.length)?;
        if local_end > self.length {
            // 接收者的EOF可形成空子串，但不能越过当前子串读取同一快照的后继文本。
            return None;
        }
        Some(Self {
            start: self.start.checked_add(range.location)?,
            length: range.length,
        })
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "独立Foundation控制消费观测；库单测只编译同源实现而不启动控制"
)]
/// 独立控制的固定标量观测；计数只描述实际primitive请求，不声明框架内部零复制。
pub(super) struct FrozenMarkdownStringReadObservation {
    pub(super) character_calls: usize,
    pub(super) bulk_calls: usize,
    pub(super) requested_units: usize,
    pub(super) maximum_bulk_units: usize,
    pub(super) full_snapshot_bulk_calls: usize,
    pub(super) copy_calls: usize,
    pub(super) substring_calls: usize,
}

#[cfg(test)]
#[derive(Default)]
/// 所有派生子串共享有限计数，不持正文、原生对象或生命周期回调。
struct FrozenReadCounters {
    character_calls: AtomicUsize,
    bulk_calls: AtomicUsize,
    requested_units: AtomicUsize,
    maximum_bulk_units: AtomicUsize,
    full_snapshot_bulk_calls: AtomicUsize,
    copy_calls: AtomicUsize,
    substring_calls: AtomicUsize,
}

#[cfg(test)]
impl FrozenReadCounters {
    #[allow(
        dead_code,
        reason = "独立Foundation控制分阶段采样；库单测不执行该原生入口"
    )]
    fn observation(&self) -> FrozenMarkdownStringReadObservation {
        // 观测不调用Native getter，也不为读取计数再读一次正文；控制在各同步阶段后采集。
        FrozenMarkdownStringReadObservation {
            character_calls: self.character_calls.load(Ordering::Relaxed),
            bulk_calls: self.bulk_calls.load(Ordering::Relaxed),
            requested_units: self.requested_units.load(Ordering::Relaxed),
            maximum_bulk_units: self.maximum_bulk_units.load(Ordering::Relaxed),
            full_snapshot_bulk_calls: self.full_snapshot_bulk_calls.load(Ordering::Relaxed),
            copy_calls: self.copy_calls.load(Ordering::Relaxed),
            substring_calls: self.substring_calls.load(Ordering::Relaxed),
        }
    }
}

pub(super) struct FrozenMarkdownStringIvars {
    source: FrozenNativeSource,
    range: FrozenUtf16Range,
    // 局部观测仅独立控制编译；生产试验只有原SourceHook，不多建Arc或原子计数。
    #[cfg(test)]
    reads: Arc<FrozenReadCounters>,
    // 真实source等资源字段先析构，最后才结算此具体Native字符串值。
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "独立字符串控制保留真实claim析构；读取该字段的注解路径由独立注解控制覆盖"
        )
    )]
    claim: FrozenValueClaim,
}

define_class!(
    // SAFETY: 只提供NSString公开的UTF-16primitive、不可变copy与真实substring。
    // 所有ivars不可变或原子化，Core快照Send+Sync；不包含AppKit或线程绑定资源。
    #[unsafe(super = NSString)]
    #[thread_kind = AnyThread]
    #[ivars = FrozenMarkdownStringIvars]
    pub(super) struct FrozenMarkdownString;

    // SAFETY: NSObject协议的继承行为保持；类没有可变正文或额外原生资源回调。
    unsafe impl NSObjectProtocol for FrozenMarkdownString {}

    impl FrozenMarkdownString {
        #[unsafe(method(length))]
        fn frozen_length(&self) -> NSUInteger {
            self.ivars().range.length // 这是不可变值的真实长度，不随编辑或关闭变为0。
        }

        #[unsafe(method(characterAtIndex:))]
        fn frozen_character(&self, index: NSUInteger) -> unichar {
            let read = self.ivars().source.request_read(
                FrozenReadKind::Character, self.ivars().range.start, self.ivars().range.length, index, 1,
            ).unwrap_or_else(|_| throw_range());
            #[cfg(test)]
            {
                // 独立控制记录原真实请求；生产原SourceHook已在request_read内完成观测。
                self.ivars().reads.character_calls.fetch_add(1, Ordering::Relaxed);
                self.ivars().reads.requested_units.fetch_add(1, Ordering::Relaxed);
            }
            read.code_unit().unwrap_or_else(|_| throw_storage())
            // 已验证的真实快照读取若失败就是内部不变量故障，不可返回替换字符掩盖错误。
        }

        #[unsafe(method(getCharacters:range:))]
        /// # Safety
        /// destination在本次同步调用中须可写range.length个UTF-16单元且满足u16对齐。
        unsafe fn frozen_characters_range(&self, destination: NonNull<unichar>, range: NSRange) {
            // SAFETY: caller提供真实目标容量；helper先通知hook并完整验证，之后才构造Rust slice。
            unsafe { self.copy_requested_units(destination, range) };
        }

        #[unsafe(method(getCharacters:))]
        /// # Safety
        /// destination在本次同步调用中须可写接收者完整length个UTF-16单元。
        unsafe fn frozen_all_characters(&self, destination: NonNull<unichar>) {
            // 全值请求仍按真实范围完整执行，不截断、不伪造计数；未来调用方必须自行限定工作量。
            // SAFETY: caller已提供接收者真实length个可写单元，范围来自不可变构造时校验。
            unsafe { self.copy_requested_units(destination, NSRange::new(0, self.ivars().range.length)) };
        }

        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn frozen_copy(&self, _zone: *mut NSZone) -> Retained<Self> {
            #[cfg(test)]
            self.ivars().reads.copy_calls.fetch_add(1, Ordering::Relaxed);
            self.retain() // 不可变copy只交付这一个+1；不复制字符、快照或资源索引。
        }

        #[unsafe(method_id(substringWithRange:))]
        #[unsafe(method_family = none)]
        fn frozen_substring(&self, range: NSRange) -> Retained<NSString> {
            self.substring_value(range)
        }

        #[unsafe(method_id(substringFromIndex:))]
        #[unsafe(method_family = none)]
        fn frozen_substring_from(&self, index: NSUInteger) -> Retained<NSString> {
            let length = self.ivars().range.length.checked_sub(index).unwrap_or_else(|| throw_range());
            self.substring_value(NSRange::new(index, length)) // EOF返回真实空子串，越界明确异常。
        }

        #[unsafe(method_id(substringToIndex:))]
        #[unsafe(method_family = none)]
        fn frozen_substring_to(&self, index: NSUInteger) -> Retained<NSString> {
            self.substring_value(NSRange::new(0, index))
        }
    }
);

// SAFETY: AnyThread只约束构造，通用NSString可能实为NSMutableString，不能据此自动推断Send。
// 本类唯一构造入口校验实际class精确Self，正文仅不可变且Send+Sync的Opaque Source；
// 范围不可变，测试计数原子化；claim家族只持线程安全计数/原子观察者，纯fn hook不捕业务owner。
// 没有活动文档、可变NSString、AppKit、主线程Drop或原生回调。
// Apple公开允许不可变NSString及其retain/release跨线程；各线程仍须使用自己的autoreleasepool。
unsafe impl Send for FrozenMarkdownString {}

// SAFETY: 上述具体不可变类没有可写正文别名；所有继承NSString读取只消费已实现的稳定primitive。
// copy共享同一不可变值，substring创建同快照的独立范围，测试统计仅用原子操作。异常使用官方
// ObjC C-unwind/catch合同，不共享异常对象或线程局部池；不能将此实现扩用于live或mutable适配器。
unsafe impl Sync for FrozenMarkdownString {}

impl FrozenMarkdownString {
    /// 创建指定真实全局范围的不可变值；整个Opaque源使用0..source.utf16_len()。
    pub(super) fn new(
        source: FrozenNativeSource,
        global_utf16_range: Range<usize>,
    ) -> Result<Retained<Self>, FrozenMarkdownStringError> {
        let range = FrozenUtf16Range::new(global_utf16_range, source.utf16_len())
            .ok_or(FrozenMarkdownStringError::RangeOutsideSnapshot)?;
        Self::from_parts(
            source,
            range,
            #[cfg(test)]
            Arc::new(FrozenReadCounters::default()),
        )
    }

    /// 读取固定标量，供独立Foundation控制分阶段核对实际请求，不读取正文。
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "仅独立Foundation控制调用；库单测不能据编译自行启动原生控制"
    )]
    pub(super) fn read_observation(&self) -> FrozenMarkdownStringReadObservation {
        self.ivars().reads.observation()
    }

    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "独立字符串控制不链接注解；真实注解派生由独立注解控制验证"
        )
    )]
    pub(super) fn claim_annotation_payload(
        &self,
    ) -> Result<FrozenValueClaim, FrozenValueFamilyError> {
        self.ivars().claim.claim_annotation_payload()
    }

    fn from_parts(
        source: FrozenNativeSource,
        range: FrozenUtf16Range,
        #[cfg(test)] reads: Arc<FrozenReadCounters>,
    ) -> Result<Retained<Self>, FrozenMarkdownStringError> {
        // 先从仍活SourceClaim派生；seal之后旧源仍可创建真实不可变子串。
        let claim = source.claim_string_value()?;
        let allocated = Self::alloc().set_ivars(FrozenMarkdownStringIvars {
            source,
            range,
            #[cfg(test)]
            reads,
            claim,
        });
        // SAFETY: NSString公开子类要求的两个primitive及ivars已完成，父类init仅初始化同一值。
        // 返回类型必须是实际Self，保持objc2的PartialInit与init所有权合同。
        let value: Retained<Self> = unsafe { msg_send![super(allocated), init] };
        if !std::ptr::eq(value.class(), Self::class()) {
            // class cluster若替换了对象，不再访问Rust ivars；原Retained沿正常release结束。
            return Err(FrozenMarkdownStringError::NativeClassChanged);
        }
        Ok(value)
    }

    fn substring_value(&self, local_range: NSRange) -> Retained<NSString> {
        let range = self
            .ivars()
            .range
            .subrange(local_range)
            .unwrap_or_else(|| throw_range());
        #[cfg(test)]
        self.ivars()
            .reads
            .substring_calls
            .fetch_add(1, Ordering::Relaxed);
        if range == self.ivars().range {
            self.retain().into_super() // 恰好整个接收者可共享同一值，由noncopy method_id处理返回。
        } else {
            // 只分享Opaque源并保存真实全局偏移；不保留父串，也不链式访问原Native对象。
            Self::from_parts(
                self.ivars().source.clone(),
                range,
                #[cfg(test)]
                Arc::clone(&self.ivars().reads),
            )
            .unwrap_or_else(|_| throw_storage())
            .into_super()
        }
    }

    /// 在范围已验证后完整初始化调用方请求；不分配UTF-16中转或截断Native bulk。
    ///
    /// # Safety
    /// destination在本同步调用中覆盖local.length个可写且对齐的UTF-16单元。
    unsafe fn copy_requested_units(&self, destination: NonNull<unichar>, local: NSRange) {
        // 非法范围也先交原始hook事实；失败在任何目标slice、计数更新或字符读取之前发生。
        let read = self
            .ivars()
            .source
            .request_read(
                FrozenReadKind::Bulk,
                self.ivars().range.start,
                self.ivars().range.length,
                local.location,
                local.length,
            )
            .unwrap_or_else(|_| throw_range());
        let length = read.length();
        #[cfg(test)]
        {
            // 仅独立控制累计实际bulk；生产仍由同一request_read的全局hook完整记量。
            let reads = &self.ivars().reads;
            reads.bulk_calls.fetch_add(1, Ordering::Relaxed);
            reads.requested_units.fetch_add(length, Ordering::Relaxed);
            reads
                .maximum_bulk_units
                .fetch_max(length, Ordering::Relaxed);
            if read.global_start() == 0 && length == self.ivars().source.utf16_len() {
                // 只记录真实整快照请求；即使是框架要求，也不能隐藏为有限加载成功。
                reads
                    .full_snapshot_bulk_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        if length == 0 {
            // 空范围无需构造slice或解引用destination，包含真实EOF空子串。
            return;
        }
        // SAFETY: 外层公开primitive已验证local/global等长且位于快照内；caller保证这段
        // 单一可写内存满足UTF-16对齐和长度，期间没有Native重入或其他可写别名。
        let output = unsafe { std::slice::from_raw_parts_mut(destination.as_ptr(), length) };
        if read.copy_to(output).is_err() {
            // Core快照不会撤权；不把真实存储不变量故障当成短读成功或替换字符。
            throw_storage();
        }
    }
}

fn throw_range() -> ! {
    throw_named(
        ns_string!("NSRangeException"),
        ns_string!("不可变字符串的UTF-16请求越过接收者范围"),
    )
}

fn throw_storage() -> ! {
    throw_named(
        ns_string!("NSInternalInconsistencyException"),
        ns_string!("不可变字符串的已验证快照资源不一致"),
    )
}

fn throw_named(name: &NSString, reason: &NSString) -> ! {
    let class = AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: 仅调用公开NSException工厂；固定名称/原因无正文，nil userInfo符合公开ABI。
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: name, reason: reason, userInfo: None::<&AnyObject>]
    };
    // define_class生成extern C-unwind；只用官方ObjC异常helper，调用方必须以ObjC catch接收。
    // 独立控制真实验证此边界，不用Rust panic或catch_unwind替代Foundation的异常合同。
    objc2::exception::throw(exception)
}
