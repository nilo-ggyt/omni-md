//! 同一原生动作的检查注解快照；只取真实rendering元数据，正文由原Opaque源共享。

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, msg_send};
use objc2_app_kit::{
    NSTextCheckingController, NSTextContentManager, NSTextContentStorage, NSTextElementProvider,
    NSTextLayoutManager, NSTextLocation, NSTextRange, NSTextStorageObserving, NSTextView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSComparisonResult, NSNotFound, NSRange, NSString, ns_string,
};

use super::frozen_annotation::{FrozenAnnotation, FrozenAnnotationAttributes, FrozenAnnotationRun};
use super::frozen_native_source::FrozenNativeSource;
use super::frozen_native_string::FrozenMarkdownString;
use super::{
    MarkdownPresentationMode, NativeMarkdownSelectionCapture, PieceBackedMarkdownTextStorage,
    PieceBackedMarkdownTextView, WeakMarkdownDocument,
};

pub(super) enum FrozenAnnotationCapture {
    PassThrough,
    Outside,
    Cancelled,
    Value(Retained<NSAttributedString>, NSRange),
}

static PRE_OWNER_PASSTHROUGH: AtomicUsize = AtomicUsize::new(0);
static RANGE_REQUEST_DIAGNOSTICS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn pre_owner_passthrough_count() -> usize {
    PRE_OWNER_PASSTHROUGH.load(Ordering::Relaxed) // 仅固定标量；不能据此掩盖fallback发生的原正文读取。
}

#[derive(Clone, Copy)]
enum CaptureFailure {
    Cancelled,
    NativeGraph,
    NativeRange,
    Source,
    Construction,
}

/// 全部为原动作弱身份与纯标量；不借文档、slot或Native对象跨任何getter。
struct CaptureOrigin {
    selection: NativeMarkdownSelectionCapture,
    identity: WeakMarkdownDocument,
    generation: u64,
    length: usize,
    semantic_epoch: u64,
    table_epoch: u64,
    fold_epoch: u64,
    input_epoch: u64,
    mode: MarkdownPresentationMode,
}

impl CaptureOrigin {
    fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        if view.ivars().textkit_graph_teardown.get()
            || !self.selection.current()
            || !storage.ivars().accepts_edits.get()
            || storage.ivars().font_run_scratch.epoch() != self.semantic_epoch
            || storage.ivars().table_geometry_epoch.get() != self.table_epoch
            || view.ivars().preview_block_fold_epoch.get() != self.fold_epoch
            || view.ivars().table_input_intent_epoch.get() != self.input_epoch
            || storage.presentation_mode() != self.mode
            || !self.identity.ptr_eq(&storage.ivars().document.borrow())
        {
            return false; // 原动作撤权后不能采当前g或新manager重新签发同一检查请求。
        }
        let Some(document) = self.identity.upgrade() else {
            return false; // 原Weak仍保分配身份，但正文已经结束。
        };
        let Ok(document) = document.try_borrow() else {
            return false; // 正文正在独占提交时不重入读取或把暂态当作同一冻结来源。
        };
        document.generation() == self.generation && document.utf16_len() == self.length
    }
}

/// 临时controller只用于公开validAnnotations；任何正常、撤权或外来异常出口都对称失效。
struct TemporaryCheckingController(Retained<NSTextCheckingController>);

impl Drop for TemporaryCheckingController {
    fn drop(&mut self) {
        // 即使原业务票已撤销，仍有权限清理本同步栈创建的同一个临时controller。
        // 不发送检查、不替换view原controller，也不吞掉invalidate或此前Native抛出的异常。
        self.0.invalidate();
    }
}

pub(super) fn capture(
    view: &PieceBackedMarkdownTextView,
    proposed: NSRange,
) -> FrozenAnnotationCapture {
    if !view.ivars().frozen_values_enabled {
        return FrozenAnnotationCapture::PassThrough; // 原构造门关闭时不碰fold或任何Native getter。
    }
    let Ok(selection) = view.ivars().folds.capture_selection() else {
        // 初始构造尚无原owner票时保留系统原入口；不能猜一个owner或将未验证结果冒充Ready。
        // 只纯计量已经确定启用的会话，不升级Native Weak；实际super仍完整进入全局读诊断。
        let ordinal = PRE_OWNER_PASSTHROUGH
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_add(1)
            })
            .map_or(usize::MAX, |count| count + 1);
        if ordinal <= 4 || (ordinal <= 262_144 && ordinal.is_power_of_two()) {
            eprintln!(
                "frozen_annotation_capture event=pre_owner_passthrough count={ordinal} formal=false"
            );
        } // 固定上限日志；计数仍保留全部fallback，不将未打印请求当作不存在。
        return FrozenAnnotationCapture::PassThrough;
    };
    let input_epoch = view.ivars().table_input_intent_epoch.get();
    let fold_epoch = view.ivars().preview_block_fold_epoch.get();
    let storage = view.ivars().text_storage.load();
    if !selection.current()
        || view.ivars().textkit_graph_teardown.get()
        || view.ivars().table_input_intent_epoch.get() != input_epoch
        || view.ivars().preview_block_fold_epoch.get() != fold_epoch
    {
        return FrozenAnnotationCapture::Cancelled; // 第一Native Weak读取也必须沿首调用前原票。
    }
    let Some(storage) = storage else {
        return fail(CaptureFailure::NativeGraph); // enabled且已有原票不能静默改走未计量的super回退。
    };
    let Some(values) = &storage.ivars().string.ivars().frozen_values else {
        return fail(CaptureFailure::Source); // view/storage必须共享同一constructor决定，不重新读环境补签。
    };
    let identity = storage.ivars().document.borrow().clone();
    let Some(document) = identity.upgrade() else {
        return FrozenAnnotationCapture::Cancelled;
    };
    if !view.ivars().folds.matches_document(&document) {
        return FrozenAnnotationCapture::Cancelled; // 同g的另一storage也不属于这份原动作票。
    }
    let Some(source) = values.source_for_document(&document) else {
        drop(document);
        return fail(CaptureFailure::Source);
    };
    drop(document); // 从此直到返回，没有活动Doc强租约或RefCell借用跨Native。
    let origin = CaptureOrigin {
        selection,
        identity,
        generation: source.generation(),
        length: source.utf16_len(),
        semantic_epoch: storage.ivars().font_run_scratch.epoch(),
        table_epoch: storage.ivars().table_geometry_epoch.get(),
        fold_epoch,
        input_epoch,
        mode: storage.presentation_mode(),
    };
    let current = || origin.current(view, &storage);
    if !current() {
        return FrozenAnnotationCapture::Cancelled;
    }
    let Some(intersection) = intersect_proposed_range(proposed, source.utf16_len()) else {
        observe_request_range(proposed, source.utf16_len(), None, None);
        return FrozenAnnotationCapture::Outside; // 完全不相交时不写actualRange，也不创建检查值。
    };
    let actual = match source.text_checking_paragraph_range(intersection.clone()) {
        Ok(range) => range,
        Err(_) => {
            observe_request_range(proposed, source.utf16_len(), Some(&intersection), None);
            return fail(CaptureFailure::Source); // 保留真实调整失败，不为诊断读取正文或改走super。
        }
    };
    observe_request_range(
        proposed,
        source.utf16_len(),
        Some(&intersection),
        Some(&actual),
    );
    let result = capture_value(view, &storage, &source, actual.clone(), &current);
    // capture_value的manager、controller、临时字典等Native claims已析构；再核一次原权限。
    if !current() {
        drop(result);
        return FrozenAnnotationCapture::Cancelled;
    }
    match result {
        Ok(value) => FrozenAnnotationCapture::Value(
            value,
            NSRange::new(actual.start, actual.end - actual.start),
        ),
        Err(error) => fail(error),
    }
}

fn observe_request_range(
    proposed: NSRange,
    length: usize,
    intersection: Option<&Range<usize>>,
    actual: Option<&Range<usize>>,
) {
    // 全进程最多8条固定标量事实；只复用当前已完成的范围查询，不请求正文、Native或新权限。
    let ordinal = RANGE_REQUEST_DIAGNOSTICS
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            count.checked_add(1)
        })
        .map_or(usize::MAX, |count| count + 1);
    if ordinal > 8 {
        return; // 预算结束后不格式化或保留任何请求；真实检查和全局primitive诊断仍原样执行。
    }
    let (intersection_start, intersection_length) =
        intersection.map_or((0, 0), |range| (range.start, range.end - range.start));
    let (actual_start, actual_length) =
        actual.map_or((0, 0), |range| (range.start, range.end - range.start));
    eprintln!(
        "frozen_annotation_capture event=range_request ordinal={ordinal} proposed_start={} proposed_length={} intersection_present={} intersection_start={intersection_start} intersection_length={intersection_length} actual_present={} actual_start={actual_start} actual_length={actual_length} source_length={length} formal=false",
        proposed.location,
        proposed.length,
        intersection.is_some(),
        actual.is_some(),
    );
}

fn capture_value(
    view: &PieceBackedMarkdownTextView,
    storage: &PieceBackedMarkdownTextStorage,
    source: &FrozenNativeSource,
    actual: Range<usize>,
    current: &impl Fn() -> bool,
) -> Result<Retained<NSAttributedString>, CaptureFailure> {
    let runs = if actual.is_empty() {
        Vec::new() // 合法空文档或尾caret没有属性字符，仍须返回真实空值而不是nil。
    } else {
        capture_rendering_runs(view, storage, source.utf16_len(), &actual, current)?
    };
    let string = native_step(current, || {
        FrozenMarkdownString::new(source.clone(), actual)
    })?
    .map_err(|_| CaptureFailure::Construction)?;
    let value = native_step(current, || FrozenAnnotation::new(string, runs))?
        .map_err(|_| CaptureFailure::Construction)?;
    Ok(value.into_super()) // 不经过mutable属性容器或super检查，不复制正文。
}

fn capture_rendering_runs(
    view: &PieceBackedMarkdownTextView,
    storage: &PieceBackedMarkdownTextStorage,
    length: usize,
    actual: &Range<usize>,
    current: &impl Fn() -> bool,
) -> Result<Vec<FrozenAnnotationRun>, CaptureFailure> {
    let manager =
        native_step(current, || view.textLayoutManager())?.ok_or(CaptureFailure::NativeGraph)?;
    let expected_manager = native_step(current, || {
        view.ivars()
            .text_layout_manager
            .get()
            .and_then(|weak| weak.load())
    })?
    .ok_or(CaptureFailure::NativeGraph)?;
    if !std::ptr::eq(&*manager, &*expected_manager)
        || native_step(current, || manager.layoutQueue())?.is_some()
    {
        return Err(CaptureFailure::NativeGraph); // 未认证的另一图或后台布局不能为原请求提供注解快照。
    }
    let content = native_step(current, || manager.textContentManager())?
        .ok_or(CaptureFailure::NativeGraph)?;
    verify_graph(view, storage, &manager, &content, current)?;
    let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
    let document_range = native_step(current, || provider.documentRange())?;
    let document_start = native_step(current, || document_range.location())?;
    let document_end = native_step(current, || document_range.endLocation())?;
    if location_offset(provider, &document_start, &document_end, length, current)? != length {
        return Err(CaptureFailure::NativeRange); // Native全局坐标必须与同一冻结source完整长度相等。
    }
    let keys = valid_annotation_keys(view, current)?;
    let runs = RefCell::new(Vec::new());
    let previous_end = Cell::new(0usize);
    let failure = Cell::new(None);
    let callback = RcBlock::new(
        |callback_manager: std::ptr::NonNull<NSTextLayoutManager>,
         attributes: std::ptr::NonNull<FrozenAnnotationAttributes>,
         range: std::ptr::NonNull<NSTextRange>|
         -> Bool {
            let result = (|| {
                if !current() {
                    return Err(CaptureFailure::Cancelled);
                }
                if !std::ptr::eq(callback_manager.as_ptr(), &*manager) {
                    return Err(CaptureFailure::NativeGraph); // callback只能属于本次原manager。
                }
                // SAFETY: 公开同步enumeration提供的三个nonnull对象在本次block中真实存活；不外借裸指针。
                let range = unsafe { range.as_ref() };
                let start_location = native_step(current, || range.location())?;
                let end_location = native_step(current, || range.endLocation())?;
                let start =
                    location_offset(provider, &document_start, &start_location, length, current)?;
                let end =
                    location_offset(provider, &document_start, &end_location, length, current)?;
                if start > end || start < previous_end.get() {
                    return Err(CaptureFailure::NativeRange); // 不能排序/裁重叠来掩盖真实Native枚举合同损坏。
                }
                previous_end.set(end);
                if start >= actual.end {
                    return Ok(false); // 已越过真实调整范围，仅终止后继元数据，不截断任何相交run。
                }
                let intersection = start.max(actual.start)..end.min(actual.end);
                if intersection.start >= intersection.end {
                    return Ok(true); // 早于目标的run或零范围没有目标属性字符，不能制造空run覆盖。
                }
                // SAFETY: 同一同步block保证原属性字典存活；下面只按实际valid键取值，不看正文/描述。
                let attributes = unsafe { attributes.as_ref() };
                let filtered = filtered_attributes(attributes, &keys, current)?;
                if let Some(attributes) = filtered {
                    runs.borrow_mut().push(FrozenAnnotationRun {
                        range: intersection.start - actual.start..intersection.end - actual.start,
                        attributes,
                    }); // 只持实际非空注解run；未覆盖gap由FrozenAnnotation保留空字典语义。
                }
                Ok(true)
            })();
            match result {
                Ok(keep_going) => Bool::new(keep_going),
                Err(error) => {
                    failure.set(Some(error));
                    Bool::NO // 原票或坐标失效立即停止；不返回一半结果当作Ready，也不自行重试。
                }
            }
        },
    );
    // 从已验证的document origin枚举真实元数据，避免假设API会补回起点之前跨入目标的run。
    // 这里只读稀疏annotation/rendering范围，不读正文、不要求layout、不限制dense元数据条数。
    native_step(current, || {
        manager.enumerateRenderingAttributesFromLocation_reverse_usingBlock(
            &document_start,
            false,
            &callback,
        )
    })?;
    drop(callback); // block不带未归还RefCell借用；其后所有Native析构仍由原current复核。
    if let Some(error) = failure.get() {
        return Err(error);
    }
    verify_graph(view, storage, &manager, &content, current)?;
    Ok(runs.into_inner())
}

fn verify_graph(
    view: &PieceBackedMarkdownTextView,
    storage: &PieceBackedMarkdownTextStorage,
    manager: &NSTextLayoutManager,
    content: &NSTextContentManager,
    current: &impl Fn() -> bool,
) -> Result<(), CaptureFailure> {
    let actual_storage = native_step(current, || view.ivars().text_storage.load())?;
    let actual_manager = native_step(current, || view.textLayoutManager())?;
    let actual_content = native_step(current, || manager.textContentManager())?;
    if actual_storage
        .as_deref()
        .is_none_or(|actual| !std::ptr::eq(actual, storage))
        || actual_manager
            .as_deref()
            .is_none_or(|actual| !std::ptr::eq(actual, manager))
        || actual_content
            .as_deref()
            .is_none_or(|actual| !std::ptr::eq(actual, content))
    {
        return Err(CaptureFailure::NativeGraph); // Getter同步换图也不能把原源和后继注解拼起来。
    }
    let content_storage = content
        .downcast_ref::<NSTextContentStorage>()
        .ok_or(CaptureFailure::NativeGraph)?;
    let observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(content_storage);
    let canonical = native_step(current, || observer.textStorage())?;
    let expected: &objc2_app_kit::NSTextStorage = storage;
    if canonical
        .as_deref()
        .is_none_or(|actual| !std::ptr::eq(actual, expected))
    {
        return Err(CaptureFailure::NativeGraph); // 真实content provider必须仍观察同一canonical存储。
    }
    Ok(())
}

fn valid_annotation_keys(
    view: &PieceBackedMarkdownTextView,
    current: &impl Fn() -> bool,
) -> Result<Vec<Retained<NSString>>, CaptureFailure> {
    let controller = native_step(current, || {
        let client: &NSTextView = view;
        // SAFETY: 公开initWithClient:接收id<NSTextCheckingClient>，原NSTextView真实提供这些selector；
        // 绑定未声明Rust conformance，不能伪造trait。离线原生控制已核相同公开调用和注解语义。
        let controller: Retained<NSTextCheckingController> =
            unsafe { msg_send![NSTextCheckingController::alloc(), initWithClient: client] };
        TemporaryCheckingController(controller) // 原init返回即入RAII，撤权也必须invalidate同一对象。
    })?;
    let original_keys = native_step(current, || controller.0.validAnnotations())?;
    // SAFETY: 公开NSArray copy取得不可变键集合+1；不创建业务controller或读取正文。
    let keys: Retained<NSArray<NSString>> =
        native_step(current, || unsafe { msg_send![&*original_keys, copy] })?;
    drop(original_keys);
    drop(controller); // 必须在枚举rendering前立即invalidate/drop，不替换原view的默认controller。
    if !current() {
        return Err(CaptureFailure::Cancelled);
    }
    let count = native_step(current, || keys.count())?;
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        result.push(native_step(current, || keys.objectAtIndex(index))?);
        // 每项只保真实系统键；不按猜测的固定字符串集合删掉未来系统注解。
    }
    Ok(result)
}

fn filtered_attributes(
    attributes: &FrozenAnnotationAttributes,
    keys: &[Retained<NSString>],
    current: &impl Fn() -> bool,
) -> Result<Option<Retained<FrozenAnnotationAttributes>>, CaptureFailure> {
    let mut kept_keys: Vec<&NSString> = Vec::new();
    let mut values = Vec::new();
    for key in keys {
        if let Some(value) = native_step(current, || attributes.objectForKey(key))? {
            kept_keys.push(key.as_ref());
            values.push(value); // 实际NSNumber、NSNull、NSString等均按原浅值保留，绝不强转/改写。
        }
    }
    if values.is_empty() {
        return Ok(None); // 纯视觉run没有有效注解；不能将font、颜色等原storage属性带入检查结果。
    }
    let value_refs: Vec<&AnyObject> = values.iter().map(|value| &**value).collect();
    Ok(Some(native_step(current, || {
        FrozenAnnotationAttributes::from_slices(&kept_keys, &value_refs)
    })?))
}

fn location_offset(
    provider: &ProtocolObject<dyn NSTextElementProvider>,
    origin: &ProtocolObject<dyn NSTextLocation>,
    location: &ProtocolObject<dyn NSTextLocation>,
    length: usize,
    current: &impl Fn() -> bool,
) -> Result<usize, CaptureFailure> {
    let offset = native_step(current, || {
        provider.offsetFromLocation_toLocation(origin, location)
    })?;
    let offset = usize::try_from(offset).map_err(|_| CaptureFailure::NativeRange)?;
    if offset > length {
        return Err(CaptureFailure::NativeRange); // 仅真实document坐标合法，不能饱和夹取错误Native位置。
    }
    let signed = isize::try_from(offset).map_err(|_| CaptureFailure::NativeRange)?;
    let roundtrip = native_step(current, || {
        provider.locationFromLocation_withOffset(origin, signed)
    })?
    .ok_or(CaptureFailure::NativeRange)?;
    if native_step(current, || location.compare(&roundtrip))? != NSComparisonResult::Same {
        return Err(CaptureFailure::NativeRange); // 正向offset与逆向位置必须精确对应原provider。
    }
    Ok(offset)
}

fn native_step<T>(
    current: &impl Fn() -> bool,
    action: impl FnOnce() -> T,
) -> Result<T, CaptureFailure> {
    if !current() {
        return Err(CaptureFailure::Cancelled);
    }
    let result = action();
    if !current() {
        return Err(CaptureFailure::Cancelled); // 返回对象在原栈自然释放，不能借它给新动作补票。
    }
    Ok(result)
}

fn intersect_proposed_range(proposed: NSRange, length: usize) -> Option<Range<usize>> {
    if proposed.location == NSNotFound as usize && proposed.length == 0 {
        return Some(0..length); // 协议明确的全文sentinel，不把它改为视口请求。
    }
    if proposed.location > length {
        return None; // 文档两端是合法caret，只有严格越过EOF才完全不相交。
    }
    let start = proposed.location;
    let end = if proposed.length > length - start {
        length // 标准文档交集按差值比较防溢出；不是为了性能裁短合法段落。
    } else {
        start + proposed.length
    };
    Some(start..end)
}

fn fail(error: CaptureFailure) -> FrozenAnnotationCapture {
    if matches!(error, CaptureFailure::Cancelled) {
        return FrozenAnnotationCapture::Cancelled;
    }
    let class = AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: 同一实验入口明确失败；只发送公开工厂和固定诊断，不输出正文/路径或吞异常继续。
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: ns_string!("NSInternalInconsistencyException"),
            reason: ns_string!("不可变检查注解无法认证原Native来源或完整范围"), userInfo: None::<&AnyObject>]
    };
    objc2::exception::throw(exception)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposed_document_intersection_preserves_eof_empty_and_full_sentinel() {
        assert_eq!(
            intersect_proposed_range(NSRange::new(NSNotFound as usize, 0), 19),
            Some(0..19)
        );
        assert_eq!(
            intersect_proposed_range(NSRange::new(19, 0), 19),
            Some(19..19)
        );
        assert_eq!(
            intersect_proposed_range(NSRange::new(19, 8), 19),
            Some(19..19)
        );
        assert_eq!(intersect_proposed_range(NSRange::new(20, 0), 19), None);
        assert_eq!(intersect_proposed_range(NSRange::new(0, 0), 0), Some(0..0));
    }

    #[test]
    fn proposed_document_intersection_clamps_only_standard_bounds_without_overflow() {
        assert_eq!(
            intersect_proposed_range(NSRange::new(4, 7), 19),
            Some(4..11)
        );
        assert_eq!(
            intersect_proposed_range(NSRange::new(4, usize::MAX), 19),
            Some(4..19)
        );
        assert_eq!(
            intersect_proposed_range(NSRange::new(NSNotFound as usize, 1), 19),
            None
        );
    }
}
