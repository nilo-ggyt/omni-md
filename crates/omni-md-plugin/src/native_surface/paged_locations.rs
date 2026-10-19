//! 分页正文的原生位置值：仅持原源弱身份和全局UTF16坐标，不创建TextKit段落或保活正文。

use std::{
    cmp::Ordering,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use objc2::{
    ClassType, DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::NSTextLocation;
use objc2_foundation::{
    MainThreadMarker, NSComparisonResult, NSObject, NSObjectProtocol, NSUInteger,
};
use omni_markdown_core::{MarkdownLayoutTextAuthority, MarkdownLayoutTextSource};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NativeMarkdownPagedLocationError {
    #[error("原文本位置构造权限已撤销")]
    Revoked,
    #[error("文本位置超出原冻结正文")]
    OutsideSource,
}

/// 位置对象的固定实现字段；保持字段私有，不能从原生键重新取得正文所有权。
pub struct LocationIvars {
    source: MarkdownLayoutTextAuthority,
    utf16: usize,
    extent: usize,
}

fn native_order(order: Ordering) -> NSComparisonResult {
    match order {
        Ordering::Less => NSComparisonResult::Ascending, // 同源内严格按全局UTF16逻辑顺序。
        Ordering::Equal => NSComparisonResult::Same,     // 只有同源同位置才是相同位置。
        Ordering::Greater => NSComparisonResult::Descending,
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = LocationIvars]
    pub struct NativeMarkdownPagedTextLocation;

    // SAFETY: NSObject基类提供基础协议行为，本类只覆盖无副作用的位置相等和hash。
    unsafe impl NSObjectProtocol for NativeMarkdownPagedTextLocation {}

    impl NativeMarkdownPagedTextLocation {
        #[unsafe(method(isEqual:))]
        fn is_equal_location(&self, other: Option<&AnyObject>) -> bool {
            other
                .and_then(|object| object.downcast_ref::<Self>())
                .is_some_and(|location| {
                    // affinity、宽度和可见区不参与位置等价，多个datasource方法返回的同位置必须一致。
                    self.ivars().source == location.ivars().source
                        && self.ivars().utf16 == location.ivars().utf16
                })
        }

        #[unsafe(method(hash))]
        fn location_hash(&self) -> NSUInteger {
            let mut hasher = DefaultHasher::new();
            self.ivars().source.hash(&mut hasher);
            self.ivars().utf16.hash(&mut hasher);
            hasher.finish() as NSUInteger // 只hash固定身份和坐标，无正文读取或Native字符串分配。
        }
    }

    // SAFETY: compare实现与isEqual/hash一致，本位置不读活动Doc或可撤销的布局缓存。
    unsafe impl NSTextLocation for NativeMarkdownPagedTextLocation {
        #[unsafe(method(compare:))]
        fn compare(
            &self,
            other: &ProtocolObject<dyn NSTextLocation>,
        ) -> NSComparisonResult {
            let object: &AnyObject = other.as_ref();
            if let Some(location) = object.downcast_ref::<Self>() {
                // 先按原source身份建立跨源总序，再比较同源坐标；不同capture不会伪装成同文档。
                return native_order(
                    self.ivars()
                        .source
                        .cmp(&location.ivars().source)
                        .then_with(|| self.ivars().utf16.cmp(&location.ivars().utf16)),
                );
            }
            // 异类位置不能进入本datasource，转换入口会拒绝；这里只保持跨类确定顺序，不解释外来坐标。
            native_order(
                Self::class()
                    .name()
                    .to_bytes()
                    .cmp(object.class().name().to_bytes()),
            )
        }
    }
);

impl NativeMarkdownPagedTextLocation {
    /// 在原票内创建仅含固定元数据的位置；允许UTF16代理项内部索引供协议转换，选择边界由字素层核验。
    ///
    /// # Errors
    /// 原票失效或坐标超界即拒绝，不重捕源、不饱和成EOF、不修改选择。
    pub fn new(
        source: &MarkdownLayoutTextSource,
        utf16: usize,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Retained<Self>, NativeMarkdownPagedLocationError> {
        if !current() {
            return Err(NativeMarkdownPagedLocationError::Revoked);
        }
        if utf16 > source.utf16_len() {
            return Err(NativeMarkdownPagedLocationError::OutsideSource);
        }
        let allocated = Self::alloc(mtm).set_ivars(LocationIvars {
            source: source.authority(),
            utf16,
            extent: source.utf16_len(),
        });
        if !current() {
            drop(allocated); // alloc之后失权也不能把未初始化Native对象交给后继owner。
            return Err(NativeMarkdownPagedLocationError::Revoked);
        }
        // SAFETY: 当前主线程独占新分配的NSObject子类，ivars已完整写入，init不借正文或活动Doc。
        let value: Retained<Self> = unsafe { msg_send![super(allocated), init] };
        if !current() {
            drop(value); // init回调之后仍核原票；失败只归还本次Native取得。
            return Err(NativeMarkdownPagedLocationError::Revoked);
        }
        Ok(value)
    }

    /// 只有同源同extent的位置可转换为本次冻结正文索引；外来类、同文不同capture及越界全部拒绝。
    #[must_use]
    pub fn utf16_for_source(
        location: &ProtocolObject<dyn NSTextLocation>,
        source: &MarkdownLayoutTextSource,
    ) -> Option<usize> {
        let object: &AnyObject = location.as_ref();
        let location = object.downcast_ref::<Self>()?;
        let data = location.ivars();
        (data.source == source.authority()
            && data.extent == source.utf16_len()
            && data.utf16 <= data.extent)
            .then_some(data.utf16)
    }
}
