//! 显式试验中的live→不可变值边界；只缓存Weak源，不保留活动文档或全文副本。

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::Arc;

use objc2::Message;
use objc2::rc::Retained;
use objc2_foundation::{MainThreadMarker, NSRange, NSString, ns_string};

use super::frozen_native_source::{FrozenNativeSource, FrozenNativeSourceWeak};
use super::frozen_native_string::{FrozenMarkdownString, FrozenMarkdownStringError};
use super::frozen_value_family::FrozenValueFamily;
use super::{SharedMarkdownDocument, WeakMarkdownDocument};

struct FrozenLiveSourceCache {
    identity: WeakMarkdownDocument,
    generation: u64,
    length: usize,
    source: FrozenNativeSourceWeak,
}

pub(super) struct FrozenLiveValues {
    family: Arc<FrozenValueFamily>,
    source: RefCell<Option<FrozenLiveSourceCache>>,
    sealed: Cell<bool>,
}

impl FrozenLiveValues {
    pub(super) fn new_if_requested() -> Option<Self> {
        // 开关仅在live适配器构造时读取一次；运行中环境变化不能给既有会话换一种copy语义。
        (std::env::var_os("OMNI_NATIVE_FROZEN_VALUES").as_deref()
            == Some(std::ffi::OsStr::new("1")))
        .then(Self::enabled)
    }

    fn enabled() -> Self {
        Self {
            family: FrozenValueFamily::new(),
            source: RefCell::new(None),
            sealed: Cell::new(false),
        }
    }

    pub(super) fn family(&self) -> Arc<FrozenValueFamily> {
        Arc::clone(&self.family) // 只交出原家族，不创建第二份源/值清单或业务owner。
    }

    pub(super) fn seal(&self) {
        // Native图已在原accepted-release顺序断开；先拒绝live新捕获再归还唯一Weak缓存。
        // 既有不可变值仍持自己的原SourceClaim，之后合法copy/substring不会被撤空。
        self.sealed.set(true);
        let obsolete = self.source.borrow_mut().take();
        drop(obsolete);
        self.family.seal_sources();
    }

    pub(super) fn live_cache_is_released(&self) -> bool {
        // 只证明live入口已封闭且Weak缓存已实际取走；派生值仍须由原家族独立证明最后析构。
        self.sealed.get() && self.source.borrow().is_none()
    }

    pub(super) fn copy_value(
        &self,
        document: &RefCell<WeakMarkdownDocument>,
    ) -> Retained<NSString> {
        let source = self.capture_live_source(document);
        match source {
            Some(source) => {
                let length = source.utf16_len();
                self.string_value(source, 0..length)
            }
            None => ns_string!("").retain(), // 原DocWeak确实失效后，live空壳才按原长度0交空值。
        }
    }

    pub(super) fn substring_value(
        &self,
        document: &RefCell<WeakMarkdownDocument>,
        range: NSRange,
    ) -> Retained<NSString> {
        let source = self.capture_live_source(document);
        match source {
            Some(source) => {
                let end = range
                    .location
                    .checked_add(range.length)
                    .unwrap_or_else(|| throw_range());
                self.string_value(source, range.location..end)
            }
            None if range.location == 0 && range.length == 0 => ns_string!("").retain(),
            None => throw_range(), // 空live接收者不能把旧非空请求重定向到另一源或假装成功。
        }
    }

    fn capture_live_source(
        &self,
        document: &RefCell<WeakMarkdownDocument>,
    ) -> Option<FrozenNativeSource> {
        if MainThreadMarker::new().is_none() {
            // live文档仍为主线程RefCell；后台只能消费已冻结值，不能跨线程升级live owner。
            throw_storage();
        }
        let identity = document.borrow().clone();
        let document = identity.upgrade()?;
        if self.sealed.get() {
            // DocWeak仍可升级时封源绝不等于空文档；这是accepted-release顺序破坏，显式失败。
            throw_storage();
        }
        let source = self
            .source_for_document(&document)
            .unwrap_or_else(|| throw_storage());
        drop(document); // Native构造之前结束唯一活动Doc强租约及其全部RefCell借用。
        Some(source)
    }

    pub(super) fn source_for_document(
        &self,
        document: &SharedMarkdownDocument,
    ) -> Option<FrozenNativeSource> {
        if self.sealed.get() {
            return None; // 只拒绝新live捕获，不影响早已派生出来的不可变值读取。
        }
        let document_ref = document.try_borrow().ok()?;
        let identity = std::rc::Rc::downgrade(document);
        let generation = document_ref.generation();
        let length = document_ref.utf16_len();
        let cached = {
            let slot = self.source.borrow();
            slot.as_ref().and_then(|cached| {
                // 同代次数值不是文档身份；Weak保留原分配身份，防止另一笔记g0/长度相同的ABA。
                (cached.identity.ptr_eq(&identity)
                    && cached.generation == generation
                    && cached.length == length)
                    .then(|| cached.source.upgrade())
                    .flatten()
            })
        };
        if let Some(source) = cached {
            if source.generation() != generation || source.utf16_len() != length {
                // 命中cache还须核真实Opaque源元数据；损坏或错源不能用重捕获补签掩盖。
                return None;
            }
            return Some(source); // 只分享原完整OpaqueData和一份SourceClaim，不重新捕获snapshot。
        }
        let source = FrozenNativeSource::capture(
            &document_ref,
            &self.family,
            Some(super::string_read_diagnostic::observe_frozen_read),
        )
        .ok()?;
        drop(document_ref);
        let obsolete = self.source.replace(Some(FrozenLiveSourceCache {
            identity,
            generation,
            length,
            source: source.downgrade(),
        }));
        drop(obsolete); // 缓存只有Weak；旧代次若仍被系统值持有，由原家族等其真实释放。
        Some(source)
    }

    fn string_value(&self, source: FrozenNativeSource, range: Range<usize>) -> Retained<NSString> {
        match FrozenMarkdownString::new(source, range) {
            Ok(value) => value.into_super(),
            Err(FrozenMarkdownStringError::RangeOutsideSnapshot) => throw_range(),
            Err(_) => throw_storage(), // 真实家族或Native类不变量失败，不能回退整篇复制掩盖失败。
        }
    }
}

fn throw_range() -> ! {
    super::text_storage::throw_markdown_native_exception(
        ns_string!("NSRangeException"),
        ns_string!("live字符串的不可变子范围越过原接收者"),
    )
}

fn throw_storage() -> ! {
    // 与默认有限属性读取共享同一个公开异常出口，不为实验值复制第二套工厂。
    super::text_storage::throw_markdown_storage_unavailable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownTextSelection};

    fn fixture(text: &str) -> (tempfile::TempDir, SharedMarkdownDocument) {
        // 纯Rust控制只写自有有限输入，不创建Foundation值或访问用户笔记。
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("frozen-live-source.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        (directory, std::rc::Rc::new(RefCell::new(document)))
    }

    #[test]
    fn same_generation_reuses_live_source_without_extra_payload_claim() {
        let (_directory, document) = fixture("前😀后");
        let values = FrozenLiveValues::enabled();
        let source = values.source_for_document(&document).unwrap();
        let weak = source.downgrade();
        let again = values.source_for_document(&document).unwrap();
        assert_eq!(values.family.snapshot().constructed[0], 1);
        drop(source);
        drop(again);
        assert!(weak.upgrade().is_none()); // Weak缓存不能让源载荷常驻。
        assert_eq!(values.family.snapshot().active, [0; 4]);
    }

    #[test]
    fn new_generation_and_same_numeric_generation_other_document_keep_distinct_sources() {
        let (_first_directory, first) = fixture("a😀b");
        let (_second_directory, second) = fixture("c😀d");
        let values = FrozenLiveValues::enabled();
        let first_source = values.source_for_document(&first).unwrap();
        let second_source = values.source_for_document(&second).unwrap();
        assert_eq!(values.family.snapshot().constructed[0], 2);
        first
            .borrow_mut()
            .replace_utf16_range(0..1, "新版", MarkdownTextSelection::caret(0))
            .unwrap();
        let changed = values.source_for_document(&first).unwrap();
        assert_eq!(values.family.snapshot().constructed[0], 3);
        assert_ne!(changed.generation(), first_source.generation());
        assert_eq!(first_source.utf16_len(), second_source.utf16_len());
        assert_ne!(changed.utf16_len(), first_source.utf16_len());
    }

    #[test]
    fn seal_clears_weak_cache_and_blocks_capture_without_revoking_existing_source() {
        let (_directory, document) = fixture("旧😀值");
        let values = FrozenLiveValues::enabled();
        let source = values.source_for_document(&document).unwrap();
        let weak = source.downgrade();
        values.seal();
        assert!(values.live_cache_is_released());
        assert!(values.source_for_document(&document).is_none());
        assert_eq!(source.utf16_len(), 4);
        assert!(weak.upgrade().is_some());
        assert!(!values.family.snapshot().payload_value_destructors_complete);
        drop(source);
        assert!(weak.upgrade().is_none());
        assert!(values.family.snapshot().payload_value_destructors_complete);
    }

    #[test]
    fn upgraded_source_metadata_mismatch_rejects_without_recapture() {
        let (_directory, document) = fixture("a😀b");
        let (_other_directory, other_document) = fixture("other😀");
        let values = FrozenLiveValues::enabled();
        let source = values.source_for_document(&document).unwrap();
        let other =
            FrozenNativeSource::capture(&other_document.borrow(), &values.family, None).unwrap();
        values.source.borrow_mut().as_mut().unwrap().source = other.downgrade();
        let before = values.family.snapshot().constructed;
        assert!(values.source_for_document(&document).is_none());
        assert_eq!(values.family.snapshot().constructed, before);
        assert_eq!(source.utf16_len(), 4); // 原不可变值未被错误cache条目改写或撤权。
    }
}
