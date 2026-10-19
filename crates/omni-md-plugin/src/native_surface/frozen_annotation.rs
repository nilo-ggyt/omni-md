//! 独立不可变属性值只共享冻结NSString和有限run元数据，不通过mutable属性容器复制正文。

use std::ops::Range;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSZone};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSDictionary, NSIntersectionRange, NSObjectProtocol,
    NSRange, NSString, NSUInteger, ns_string,
};

use super::frozen_native_string::FrozenMarkdownString;
use super::frozen_value_family::{FrozenValueClaim, FrozenValueFamilyError};

pub(super) type FrozenAnnotationAttributes = NSDictionary<NSAttributedStringKey, AnyObject>;

/// 调用方提供真实source-local UTF-16范围；字典容器在构造时copy，值保留Cocoa浅属性语义。
pub(super) struct FrozenAnnotationRun {
    pub(super) range: Range<usize>,
    pub(super) attributes: Retained<FrozenAnnotationAttributes>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum FrozenAnnotationError {
    #[error("不可变属性段范围无效或越过原值")]
    InvalidRunRange,
    #[error("不可变属性段未按原坐标排序或相互重叠")]
    RunsNotOrdered,
    #[error("系统没有保留不可变属性值的实际子类")]
    NativeClassChanged,
    #[error("不可变属性值无法登记真实生命周期：{0}")]
    Family(#[from] FrozenValueFamilyError),
}

struct FrozenAnnotationSegment {
    range: Range<usize>,
    attributes: Retained<FrozenAnnotationAttributes>,
}

/// Arc只保证实际Native后台copy/substring的元数据引用计数，不承诺任意属性对象线程安全。
///
/// 字典映射冻结，属性值不deep-copy；外部修改或释放线程须满足标准Cocoa属性对象合同。
/// 该类型及Native子类不实现Rust Send/Sync，也不保存主线程图、live文档或可撤销票据。
struct FrozenAnnotationData {
    source: Retained<FrozenMarkdownString>,
    length: usize,
    segments: Box<[FrozenAnnotationSegment]>,
    // 原字符串与字典payload先实际析构，最后结算共享AnnotationPayload。
    claim: FrozenValueClaim,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AnnotationView {
    start: usize,
    length: usize,
}

impl AnnotationView {
    fn subrange(self, range: NSRange) -> Option<Self> {
        let end = range.location.checked_add(range.length)?;
        if end > self.length {
            // 原值EOF可产生空substring；不能借共同source读取接收者范围以外的属性。
            return None;
        }
        Some(Self {
            start: self.start.checked_add(range.location)?,
            length: range.length,
        })
    }

    fn index(self, index: usize) -> Option<usize> {
        if index >= self.length {
            // 空值与EOF没有可查询的属性字符；与NSString原语一样明确拒绝。
            return None;
        }
        self.start.checked_add(index)
    }

    fn end(self) -> usize {
        self.start + self.length // 只由已校验的真实source范围构造，不接受独立未检查的两端。
    }
}

pub(super) struct FrozenAnnotationIvars {
    data: Arc<FrozenAnnotationData>,
    view: AnnotationView,
    string: Retained<NSString>,
    // Data与精确string view先释放；每个实际Native属性值独立配平这一个claim。
    _claim: FrozenValueClaim,
}

define_class!(
    // SAFETY: 实现NSAttributedString公开的string/attributes primitive，并保留真实UTF-16范围。
    // AnyThread仅允许构造，类没有unsafe Send/Sync；共享元数据使用Arc，属性值保持标准浅语义。
    #[unsafe(super = NSAttributedString)]
    #[thread_kind = AnyThread]
    #[ivars = FrozenAnnotationIvars]
    pub(super) struct FrozenAnnotation;

    // SAFETY: 保留NSObject继承行为；所有资源随最后值自然释放，不回调活动文档或AppKit。
    unsafe impl NSObjectProtocol for FrozenAnnotation {}

    impl FrozenAnnotation {
        #[unsafe(method_id(string))]
        #[unsafe(method_family = none)]
        fn annotation_string(&self) -> Retained<NSString> {
            self.ivars().string.clone() // 只retain真实范围值，不复制或物化字符。
        }

        #[unsafe(method(length))]
        fn annotation_length(&self) -> NSUInteger {
            self.ivars().view.length
        }

        #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
        #[unsafe(method_family = none)]
        /// # Safety
        /// effective_range须为null或在本同步调用中可写的一个NSRange。
        unsafe fn annotation_attributes(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
        ) -> Retained<FrozenAnnotationAttributes> {
            let segment_index = self.segment_at(index);
            let segment = &self.ivars().data.segments[segment_index];
            if !effective_range.is_null() {
                let local = self.local_segment_range(segment);
                // SAFETY: caller提供可写输出，真实segment与view相交且包含已校验的index。
                unsafe { effective_range.write(local) };
            }
            segment.attributes.clone() // 非copy返回家族仅移交一个有效字典claim。
        }

        #[unsafe(method_id(attributesAtIndex:longestEffectiveRange:inRange:))]
        #[unsafe(method_family = none)]
        /// # Safety
        /// effective_range须为null或在本同步调用中可写的一个NSRange。
        unsafe fn annotation_longest_attributes(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
            range_limit: NSRange,
        ) -> Retained<FrozenAnnotationAttributes> {
            // NULL只免除求最大范围，不能免除index和rangeLimit的公开边界校验。
            let segment_index = self.segment_at(index);
            self.ivars().view.subrange(range_limit).unwrap_or_else(|| throw_range());
            let attributes = &self.ivars().data.segments[segment_index].attributes;
            if !effective_range.is_null() {
                let full = self.longest_attributes_range(segment_index);
                let clipped = NSIntersectionRange(full, range_limit);
                // SAFETY: 同一view的最大实际同字典区间按合法limit裁切，caller拥有此输出。
                unsafe { effective_range.write(clipped) };
            }
            attributes.clone()
        }

        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn annotation_copy(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain() // 不可变copy只返回这一个+1，不重新capture run、字典或字符。
        }

        #[unsafe(method_id(attributedSubstringFromRange:))]
        #[unsafe(method_family = none)]
        fn annotation_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
            let view = self.ivars().view.subrange(range).unwrap_or_else(|| throw_range());
            if view == self.ivars().view {
                self.retain().into_super() // 整范围共享同值；noncopy method_id负责正确返回家族。
            } else {
                Self::from_parts(Arc::clone(&self.ivars().data), view)
                    .unwrap_or_else(|_| throw_storage())
                    .into_super() // 新值只共享原Data与精确偏移，不保留父substring。
            }
        }
    }
);

impl FrozenAnnotation {
    /// 创建有序真实runs的不可变值；未覆盖部分具有空字典，不合成正文或分隔字符。
    pub(super) fn new(
        source: Retained<FrozenMarkdownString>,
        runs: Vec<FrozenAnnotationRun>,
    ) -> Result<Retained<Self>, FrozenAnnotationError> {
        let length = source.length();
        let mut previous_end = 0;
        for run in &runs {
            if run.range.start >= run.range.end || run.range.end > length {
                // 零长、倒置、越界均在任何字典copy前拒绝，不静默丢掉调用方的属性意图。
                return Err(FrozenAnnotationError::InvalidRunRange);
            }
            if run.range.start < previous_end {
                // 不排序或裁切原runs来掩盖重叠；原坐标顺序就是唯一属性来源。
                return Err(FrozenAnnotationError::RunsNotOrdered);
            }
            previous_end = run.range.end;
        }

        // 只有原StringClaim能派生共享payload；参数失败尚未签发claim，Native失败沿RAII配平。
        let claim = source.claim_annotation_payload()?;
        let empty = FrozenAnnotationAttributes::new();
        let mut segments = Vec::new();
        let mut next = 0;
        for run in runs {
            if next < run.range.start {
                // 只填range元数据和共享空字典；gap既不读字符，也不制造隐含样式。
                segments.push(FrozenAnnotationSegment {
                    range: next..run.range.start,
                    attributes: empty.clone(),
                });
            }
            // SAFETY: NSDictionary公开copy家族冻结容器映射；键仍为NSString，值只retain，
            // 不深拷贝、不把NSNumber/NSString/NSNull或其他Cocoa属性对象转成另一种类型。
            let attributes: Retained<FrozenAnnotationAttributes> =
                unsafe { msg_send![&*run.attributes, copy] };
            next = run.range.end;
            segments.push(FrozenAnnotationSegment {
                range: run.range,
                attributes,
            });
        }
        if next < length {
            // 尾部未覆盖区间同样保留真实空字典；空source不创建伪字符segment。
            segments.push(FrozenAnnotationSegment {
                range: next..length,
                attributes: empty,
            });
        }
        // 同一不可变ObjC值可由Native后台回调派生substring；其共享payload必须原子clone/drop，
        // 不能换Rc。字典容器已copy，source/范围不可变，claim内部同步；通用属性值仍受Cocoa
        // 自身线程合同约束，因此不为Data或Native值添加Rust Send/Sync。独立控制在main持有
        // 原值至worker join，worker只用不可变Foundation属性并核实际weak释放，不概括任意AnyObject。
        #[allow(
            clippy::arc_with_non_send_sync,
            reason = "Native AnyThread回调共享不可变payload需要原子计数；通用Cocoa属性不具Rust Send/Sync"
        )]
        let data = Arc::new(FrozenAnnotationData {
            source,
            length,
            segments: segments.into_boxed_slice(),
            claim,
        });
        Self::from_parts(data, AnnotationView { start: 0, length })
    }

    fn from_parts(
        data: Arc<FrozenAnnotationData>,
        view: AnnotationView,
    ) -> Result<Retained<Self>, FrozenAnnotationError> {
        // 每个真正new的属性对象先登记原payload血缘，copy(self)不会经过此工厂。
        let claim = data.claim.claim_annotation_value()?;
        let string = if view.start == 0 && view.length == data.length {
            data.source.clone().into_super() // 完整源直接共享原冻结NSString，绝不经过mutable容器。
        } else {
            data.source
                .substringWithRange(NSRange::new(view.start, view.length))
            // 真实子串仍由FrozenMarkdownString公开override创建共享快照值，包括单独surrogate。
        };
        let allocated = Self::alloc().set_ivars(FrozenAnnotationIvars {
            data,
            view,
            string,
            _claim: claim,
        });
        // SAFETY: 两个公开primitive与全部ivars已就绪；父类init不接收或复制正文参数。
        let value: Retained<Self> = unsafe { msg_send![super(allocated), init] };
        if !std::ptr::eq(value.class(), Self::class()) {
            // 若class cluster替换对象就不访问Rust ivars，原Retained正常release后报告失败。
            return Err(FrozenAnnotationError::NativeClassChanged);
        }
        Ok(value)
    }

    fn segment_at(&self, index: usize) -> usize {
        let global = self
            .ivars()
            .view
            .index(index)
            .unwrap_or_else(|| throw_range());
        let segments = &self.ivars().data.segments;
        let position = segments.partition_point(|segment| segment.range.end <= global);
        if segments.get(position).is_none_or(|segment| {
            // 每个真实字符必须恰好落在一个显式run或gap中；不把缺失覆盖伪装为空属性成功。
            global < segment.range.start || global >= segment.range.end
        }) {
            throw_storage();
        }
        position
    }

    fn local_segment_range(&self, segment: &FrozenAnnotationSegment) -> NSRange {
        let view = self.ivars().view;
        let start = segment.range.start.max(view.start);
        let end = segment.range.end.min(view.end());
        NSRange::new(start - view.start, end - start)
    }

    fn longest_attributes_range(&self, index: usize) -> NSRange {
        let data = &self.ivars().data;
        let view = self.ivars().view;
        let attributes = &data.segments[index].attributes;
        let mut first = index;
        let mut last = index;
        while first > 0 {
            let previous = &data.segments[first - 1];
            if previous.range.end <= view.start
                || !attributes.isEqualToDictionary(&previous.attributes)
            {
                // 按实际字典值比较而不是对象地址；跨view或字典不等即是真实最大边界。
                break;
            }
            first -= 1;
        }
        while last + 1 < data.segments.len() {
            let next = &data.segments[last + 1];
            if next.range.start >= view.end() || !attributes.isEqualToDictionary(&next.attributes) {
                // 相邻段即使来自不同原字典也可同值连续；不读字符或生成中间属性串。
                break;
            }
            last += 1;
        }
        let start = data.segments[first].range.start.max(view.start);
        let end = data.segments[last].range.end.min(view.end());
        NSRange::new(start - view.start, end - start)
    }
}

fn throw_range() -> ! {
    throw_named(
        ns_string!("NSRangeException"),
        ns_string!("不可变属性值的UTF-16请求越过接收者范围"),
    )
}

fn throw_storage() -> ! {
    throw_named(
        ns_string!("NSInternalInconsistencyException"),
        ns_string!("不可变属性值的已验证范围或资源不一致"),
    )
}

fn throw_named(name: &NSString, reason: &NSString) -> ! {
    let class = AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: 公开NSException工厂参数类型正确；固定原因不含正文，nil userInfo符合公开合同。
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: name, reason: reason, userInfo: None::<&AnyObject>]
    };
    // define_class生成C-unwind入口；仅使用已实测官方ObjC throw/catch，不以Rust panic代替。
    objc2::exception::throw(exception)
}
