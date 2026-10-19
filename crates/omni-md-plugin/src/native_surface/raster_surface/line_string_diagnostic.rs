//! 仅显式Raster诊断保留有限行来源Weak；没有字符、文档或Native图的强回链。

use super::*;

const MAX_LINE_STRING_ORIGINS: usize = 1_024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_surface) struct NativeMarkdownRasterLineStringOrigin {
    pub(in crate::native_surface) generation: u64,
    pub(in crate::native_surface) document_units: usize,
    pub(in crate::native_surface) element_source: (usize, usize),
    pub(in crate::native_surface) fragment_source: (usize, usize),
    pub(in crate::native_surface) line_range: NSRange,
    pub(in crate::native_surface) paragraph_units: usize,
    pub(in crate::native_surface) line_attribute_units: usize,
    pub(in crate::native_surface) mapped_top: Option<(usize, usize)>,
    pub(in crate::native_surface) mapped_bottom: Option<(usize, usize)>,
}

struct Entry {
    string: Weak<NSString>,
    origin: NativeMarkdownRasterLineStringOrigin,
}

struct Origins(Vec<Entry>);
impl CachePayload for Origins {
    fn empty() -> Self {
        Self(Vec::new())
    }
}

/// 生命周期只随当前surface和它的关闭收据；不进入跨session的静态注册表。
pub(super) struct LineStringDiagnosticStore {
    entries: CacheOwner<Origins>,
    sealed: Cell<bool>,
    incomplete: Cell<bool>,
}

impl LineStringDiagnosticStore {
    pub(super) fn new() -> Self {
        Self {
            entries: CacheOwner::new(Origins::empty()),
            sealed: Cell::new(false),
            incomplete: Cell::new(false),
        }
    }

    pub(super) fn seal(&self) {
        self.sealed.set(true); // 只撤后续捕获；现有Weak证据留给唯一关闭观察，最后Rc归还时清容量。
    }

    pub(super) fn record(
        &self,
        string: &NSString,
        origin: NativeMarkdownRasterLineStringOrigin,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<(), String> {
        let result = self.record_inner(string, origin, current);
        if result.is_err() {
            self.incomplete.set(true); // 不能把预算/重入丢掉的来源伪装成完整捕获。
        }
        result
    }

    fn record_inner(
        &self,
        string: &NSString,
        origin: NativeMarkdownRasterLineStringOrigin,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<(), String> {
        if self.sealed.get() || !current() {
            return Err("行字符串来源捕获已封闭或原票撤销".into());
        }
        if origin.line_attribute_units != 1 {
            return Err("有限行字符串诊断仅接受实际单unit候选".into());
        }
        let CacheAdmission::Ready(mut lease) = self.entries.begin() else {
            return Err("行字符串来源捕获发生重入或已关闭".into());
        };
        let entries = &mut lease.payload_mut().0;
        let mut duplicate = false;
        let mut failed = false;
        entries.retain(|entry| {
            if failed || !current() {
                failed = true;
                return true;
            }
            let actual = entry.string.load();
            if !current() {
                failed = true;
                drop(actual);
                return true;
            }
            if let Some(actual) = actual.as_deref() {
                duplicate |= std::ptr::eq::<NSString>(actual, string) && entry.origin == origin;
            }
            let keep = actual.is_some();
            drop(actual);
            if !current() {
                failed = true;
                return true;
            }
            keep // 只有真实nil才淘汰；同一活string的不同generation/source事实必须分别保留。
        });
        if failed || !current() {
            return Err("行字符串来源Weak扫描中原票撤销".into());
        }
        if duplicate {
            drop(lease); // 恢复槽也可能归还失效payload；返回前仍须原票，不能由去重提前绕开。
            if !current() {
                return Err("行字符串来源去重归还后原票撤销".into());
            }
            return Ok(());
        }
        if entries.len() >= MAX_LINE_STRING_ORIGINS {
            return Err("行字符串来源诊断超过1024条硬预算".into());
        }
        let weak = Weak::from(string);
        if !current() {
            return Err("行字符串来源Weak初始化后原票撤销".into());
        }
        entries.push(Entry {
            string: weak,
            origin,
        });
        drop(lease); // 结束缓存借出后仍需核原票；不在RefCell借用下加载/归还Native。
        if !current() {
            return Err("行字符串来源元数据归还后原票撤销".into());
        }
        Ok(())
    }

    #[cfg(feature = "native-close-stage-diagnostic")]
    pub(super) fn find(
        &self,
        string: &NSString,
    ) -> Result<Vec<NativeMarkdownRasterLineStringOrigin>, String> {
        if MainThreadMarker::new().is_none() {
            return Err("行字符串来源只允许主线程关闭诊断查询".into());
        }
        if !self.sealed.get() || self.incomplete.get() {
            return Err("行字符串来源尚未封闭或捕获不完整，结果未知".into());
        }
        let CacheAdmission::Ready(mut lease) = self.entries.begin() else {
            return Err("行字符串来源查询正被借出，结果未知".into());
        };
        let entries = &lease.payload_mut().0;
        if entries.len() > MAX_LINE_STRING_ORIGINS {
            return Err("行字符串来源查询超过硬预算".into());
        }
        let mut found = Vec::new();
        for entry in entries {
            let actual = entry.string.load();
            if actual
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<NSString>(actual, string))
            {
                found.push(entry.origin); // 输出只有原capture纯标量，不读取string长度、正文或line getter。
            }
            drop(actual);
        }
        drop(lease);
        if self.incomplete.get() || !self.sealed.get() {
            return Err("行字符串来源查询期间失去完整性，结果未知".into());
        }
        Ok(found) // 空列表只表示没有匹配证据，不代表这个业务值为空或已释放。
    }
}
