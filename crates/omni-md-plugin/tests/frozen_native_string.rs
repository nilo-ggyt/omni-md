//! 独立Foundation值控制；只使用合成文件，不创建NSApplication、窗口或产品会话。

#[path = "../src/native_surface/frozen_native_string.rs"]
mod frozen_native_string;

#[path = "../src/native_surface/frozen_native_source.rs"]
mod frozen_native_source;
#[path = "../src/native_surface/frozen_value_family.rs"]
// include式控制没有普通test harness入口；允许完整Family测试superset中未被此main调用的项。
#[allow(dead_code, unused_imports)]
mod frozen_value_family;

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use frozen_native_source::{FrozenNativeSource, FrozenReadKind, FrozenReadRequest};
use frozen_native_string::{
    FrozenMarkdownString, FrozenMarkdownStringError, FrozenMarkdownStringReadObservation,
};
use frozen_value_family::{
    FrozenValueFamily, FrozenValueFamilyError, FrozenValueObserverRole, FrozenValueReleaseObserver,
};
use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{AnyThread, msg_send};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSDictionary,
    NSMutableAttributedString, NSNumber, NSRange, NSString, ns_string, unichar,
};
use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownTextSelection};
use tempfile::{TempDir, tempdir};

fn capture_source(document: &MarkdownDocumentBuffer) -> FrozenNativeSource {
    // 每项源都有独立真实家族；只从Opaque入口捕获，不向Native泄漏裸snapshot克隆。
    let family = FrozenValueFamily::new();
    let source = FrozenNativeSource::capture(document, &family, None).unwrap();
    assert_eq!(source.generation(), document.generation());
    source
}

fn fixture(source: &str) -> (TempDir, MarkdownDocumentBuffer) {
    // 每项控制独占临时文件和真实匿名映射；不会枚举或读取现有笔记库。
    let directory = tempdir().expect("创建不可变值合成夹具目录");
    let path = directory.path().join("frozen-native.md");
    std::fs::write(&path, source).expect("写入不可变值合成夹具");
    let document =
        MarkdownDocumentBuffer::open(&path, 4 * 1024 * 1024).expect("打开不可变值合成夹具");
    (directory, document)
}

fn read_units(value: &NSString) -> Vec<unichar> {
    // 测试oracle仅复制已声明有限夹具；该便捷函数不进入产品或不可变值实现。
    let mut result = vec![0; value.length()];
    if !result.is_empty() {
        let destination = NonNull::new(result.as_mut_ptr()).unwrap();
        // SAFETY: Vec恰好提供真实length个对齐可写单元，此同步调用无重入别名。
        unsafe { value.getCharacters_range(destination, NSRange::new(0, result.len())) };
    }
    result
}

fn assert_units(value: &NSString, expected: &[u16]) {
    // 同时使用真实Foundation length、逐单元characterAtIndex与bulk，不能只直接调Rust helper。
    assert_eq!(value.length(), expected.len());
    for (index, expected_unit) in expected.iter().copied().enumerate() {
        // 代理对内部位置也必须精确返回单元，而不是替换字符或Unicode标量。
        assert_eq!(value.characterAtIndex(index), expected_unit);
    }
    assert_eq!(read_units(value), expected);
}

fn expect_range_exception<T>(operation: impl FnOnce() -> T + UnwindSafe) {
    let exception = match objc2::exception::catch(operation) {
        Ok(_) => panic!("Foundation越界调用没有抛出NSRangeException"),
        Err(Some(exception)) => exception,
        Err(None) => panic!("Foundation返回nil异常，不能证明范围合同"),
    };
    // SAFETY: 此处只接收本类公开NSException工厂产生的异常，name是其真实公开NSString属性。
    let name: Retained<NSString> = unsafe { msg_send![&*exception, name] };
    assert!(name.isEqualToString(ns_string!("NSRangeException")));
    // 由官方ObjC catch消费真实外来异常；不以Rust panic/catch_unwind冒充此边界。
}

fn primitives_copy_and_true_subranges() {
    autoreleasepool(|_| {
        let (_directory, document) = fixture("a中😀e\u{301}z");
        let snapshot = capture_source(&document);
        let value = FrozenMarkdownString::new(snapshot.clone(), 0..snapshot.utf16_len())
            .expect("构造完整不可变值");
        let foundation: &NSString = &value;
        let expected: Vec<u16> = "a中😀e\u{301}z".encode_utf16().collect();
        assert_units(foundation, &expected);

        let mut partial = [0xbeef; 3];
        // SAFETY: 目标恰有3单元，范围从低代理项开始但仍位于真实NSString长度内。
        unsafe {
            foundation.getCharacters_range(
                NonNull::new(partial.as_mut_ptr()).unwrap(),
                NSRange::new(3, 3),
            )
        };
        assert_eq!(partial, expected[3..6]);
        // SAFETY: EOF空范围不写任何内存；dangling指针非空且对齐，仅用于零长度调用。
        unsafe {
            foundation.getCharacters_range(NonNull::dangling(), NSRange::new(expected.len(), 0))
        };

        // SAFETY: NSObject公开copy选择子属于copy家族，返回一个实际+1 NSString claim。
        let copied: Retained<NSString> = unsafe { msg_send![foundation, copy] };
        assert!(std::ptr::eq(foundation, &*copied));
        assert_units(&copied, &expected);
        let emoji = foundation.substringWithRange(NSRange::new(2, 2));
        let low = foundation.substringWithRange(NSRange::new(3, 1));
        let middle = foundation.substringWithRange(NSRange::new(1, 5));
        let nested_low = middle.substringWithRange(NSRange::new(2, 1));
        let from_low = foundation.substringFromIndex(3);
        let to_high = foundation.substringToIndex(3);
        let empty = foundation.substringFromIndex(expected.len());
        assert_units(&emoji, &[0xd83d, 0xde00]);
        assert_units(&low, &[0xde00]);
        assert_units(&nested_low, &[0xde00]);
        assert_units(&from_low, &expected[3..]);
        assert_units(&to_high, &expected[..3]);
        assert_units(&empty, &[]);

        // 外部构造真实全局子范围同样允许代理项内部端点，不虚构scalar或段落边界。
        let global_low = FrozenMarkdownString::new(snapshot.clone(), 3..4).unwrap();
        assert_units(&global_low, &[0xde00]);
        assert!(matches!(
            FrozenMarkdownString::new(snapshot.clone(), 0..expected.len() + 1),
            Err(FrozenMarkdownStringError::RangeOutsideSnapshot)
        ));
        assert!(matches!(
            FrozenMarkdownString::new(snapshot.clone(), std::ops::Range { start: 4, end: 3 }),
            Err(FrozenMarkdownStringError::RangeOutsideSnapshot)
        ));

        expect_range_exception(AssertUnwindSafe(|| {
            foundation.characterAtIndex(expected.len())
        }));
        expect_range_exception(AssertUnwindSafe(|| {
            foundation.substringFromIndex(expected.len() + 1)
        }));
        expect_range_exception(AssertUnwindSafe(|| {
            foundation.substringToIndex(expected.len() + 1)
        }));
        expect_range_exception(AssertUnwindSafe(|| {
            foundation.substringWithRange(NSRange::new(usize::MAX, 2))
        }));
        expect_range_exception(AssertUnwindSafe(|| {
            middle.substringWithRange(NSRange::new(4, 2))
        }));

        let mut untouched = [0xbeef; 4];
        let before = value.read_observation();
        expect_range_exception(AssertUnwindSafe(|| {
            // SAFETY: 目标真实可写4单元；此故意越界范围须先抛异常，不能写入或构造超长slice。
            unsafe {
                foundation.getCharacters_range(
                    NonNull::new(untouched.as_mut_ptr()).unwrap(),
                    NSRange::new(expected.len() - 1, untouched.len()),
                )
            }
        }));
        assert_eq!(untouched, [0xbeef; 4]);
        assert_eq!(value.read_observation(), before);
        let mut overflow_untouched = [0xbeef; 2];
        expect_range_exception(AssertUnwindSafe(|| {
            // SAFETY: 此故意溢出仅用于验证范围检查先于destination处理；目标仍是有效独占数组。
            unsafe {
                foundation.getCharacters_range(
                    NonNull::new(overflow_untouched.as_mut_ptr()).unwrap(),
                    NSRange::new(usize::MAX, overflow_untouched.len()),
                )
            }
        }));
        assert_eq!(overflow_untouched, [0xbeef; 2]);
    });
    println!(
        "frozen_native_string stage=primitives_copy_substrings result=pass synthetic_only=true"
    );
}

fn values_survive_edit_history_document_and_pool_release() {
    let expected_text = format!("编辑前缀{}", "原😀文\r\n".repeat(800));
    let expected: Vec<u16> = expected_text.encode_utf16().collect();
    let (value, copied, low, weak_source, weak_value, weak_copy, weak_low) =
        autoreleasepool(|_| {
            let (directory, mut document) = fixture(&"原😀文\r\n".repeat(800));
            document
                .replace_utf16_range(0..0, "编辑前缀", MarkdownTextSelection::caret(0))
                .unwrap();
            let document = Rc::new(RefCell::new(document));
            let weak_document = Rc::downgrade(&document);
            let source = capture_source(&document.borrow());
            let weak_source = source.downgrade();
            let value = FrozenMarkdownString::new(source.clone(), 0..source.utf16_len()).unwrap();
            let foundation: &NSString = &value;
            // SAFETY: 公开copy家族返回这一个+1；复制的不可变值没有live document边。
            let copied: Retained<NSString> = unsafe { msg_send![foundation, copy] };
            let low = foundation.substringWithRange(NSRange::new(6, 1));
            let weak_value = Weak::from_retained(&value);
            let weak_copy = Weak::from_retained(&copied);
            let weak_low = Weak::from_retained(&low);

            document
                .borrow_mut()
                .replace_utf16_range(0..4, "未来", MarkdownTextSelection::caret(0))
                .unwrap();
            document.borrow_mut().undo_text_edit().unwrap();
            document.borrow_mut().redo_text_edit().unwrap();
            assert_units(foundation, &expected);
            assert_units(&copied, &expected);
            assert_units(&low, &[0xde00]);
            drop(document);
            drop(directory);
            drop(source);
            assert!(weak_document.upgrade().is_none());
            assert!(weak_source.upgrade().is_some());
            // 文档与路径已真实销毁；池排空后仍返回真实Retained值供下一阶段继续读取。
            (
                value,
                copied,
                low,
                weak_source,
                weak_value,
                weak_copy,
                weak_low,
            )
        });
    autoreleasepool(|_| {
        assert_units(&value, &expected);
        assert_units(&copied, &expected);
        assert_units(&low, &[0xde00]);
        drop(value);
        drop(copied);
        drop(low);
    });

    // 实际Native weak与唯一Opaque源weak都须在池排空后为空；Core单测另核相同值最后owner
    // 释放真实mmap/编辑段。这是独立值证据，不替代将来生产session的ledger/close收据。
    assert!(weak_value.load().is_none());
    assert!(weak_copy.load().is_none());
    assert!(weak_low.load().is_none());
    assert!(weak_source.upgrade().is_none());
    println!(
        "frozen_native_string stage=edit_history_document_drop_weak_release result=pass synthetic_only=true"
    );
}

fn empty_value_contract() {
    autoreleasepool(|_| {
        let (_directory, document) = fixture("");
        let source = capture_source(&document);
        let value = FrozenMarkdownString::new(source, 0..0).unwrap();
        let foundation: &NSString = &value;
        assert_units(foundation, &[]);
        assert_units(&foundation.substringWithRange(NSRange::new(0, 0)), &[]);
        // SAFETY: 接收者为空，真实0..0请求不写内存，且指针非空对齐。
        unsafe { foundation.getCharacters_range(NonNull::dangling(), NSRange::new(0, 0)) };
        expect_range_exception(AssertUnwindSafe(|| foundation.characterAtIndex(0)));
    });
    println!("frozen_native_string stage=empty result=pass synthetic_only=true");
}

fn print_read_phase(
    phase: &'static str,
    length: usize,
    before: FrozenMarkdownStringReadObservation,
    after: FrozenMarkdownStringReadObservation,
) {
    // max是共享计数的累计最大值；delta只对可加计数求差，不冒充阶段独立最大值或分配量。
    println!(
        "frozen_native_string stage={phase} fixture_utf16={length} character_delta={} bulk_delta={} units_delta={} full_snapshot_bulk_delta={} copy_delta={} substring_delta={} cumulative_max_bulk_units={} synthetic_only=true allocation_claim=false",
        after.character_calls - before.character_calls,
        after.bulk_calls - before.bulk_calls,
        after.requested_units - before.requested_units,
        after.full_snapshot_bulk_calls - before.full_snapshot_bulk_calls,
        after.copy_calls - before.copy_calls,
        after.substring_calls - before.substring_calls,
        after.maximum_bulk_units,
    );
}

fn numeric_attributes(
    key: &NSAttributedStringKey,
    value: usize,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let number = NSNumber::new_usize(value);
    let object: &AnyObject = &number;
    // 真实字典键为NSString、值为NSNumber；小型合成属性不包含正文、路径或原生编辑对象。
    NSDictionary::from_slices(&[key], &[object])
}

fn assert_numeric_attribute(
    value: &NSAttributedString,
    key: &NSAttributedStringKey,
    index: usize,
    expected: Option<usize>,
) {
    // SAFETY: 测试index均在有限真实字符串内，null表示本次不请求effectiveRange输出。
    let attribute =
        unsafe { value.attribute_atIndex_effectiveRange(key, index, std::ptr::null_mut()) };
    match expected {
        Some(expected) => {
            // 同时核实际NSNumber类型和值，不以对象地址相同或字典存在代替属性内容。
            let attribute = attribute.expect("指定范围应含真实数值属性");
            let number = attribute
                .downcast_ref::<NSNumber>()
                .expect("属性值应为NSNumber");
            assert_eq!(number.as_usize(), expected);
        }
        None => {
            // 不相邻段和间隙必须保留独立属性，不能把一次写入扩大为全篇样式。
            assert!(attribute.is_none());
        }
    }
}

fn observe_standard_attributed_containers() {
    let (weak_source, native_weaks) = autoreleasepool(|_| {
        let source_text = "中😀Arabic العربية עבריתe\u{301} ".repeat(192);
        let expected: Vec<u16> = source_text.encode_utf16().collect();
        let (directory, mut document) = fixture(&source_text);
        let source = capture_source(&document);
        let weak_source = source.downgrade();
        let value = FrozenMarkdownString::new(source.clone(), 0..source.utf16_len()).unwrap();
        let foundation: &NSString = &value;
        let before = value.read_observation();
        let attributed =
            NSAttributedString::initWithString(NSAttributedString::alloc(), foundation);
        print_read_phase(
            "attributed_init",
            expected.len(),
            before,
            value.read_observation(),
        );
        assert_eq!(attributed.length(), expected.len());

        let before = value.read_observation();
        let substring = attributed.attributedSubstringFromRange(NSRange::new(1, 128));
        print_read_phase(
            "attributed_substring",
            expected.len(),
            before,
            value.read_observation(),
        );
        let before = value.read_observation();
        // SAFETY: NSAttributedString公开copy家族返回一个真实+1，不假设其是否共享字符。
        let copied: Retained<NSAttributedString> = unsafe { msg_send![&*attributed, copy] };
        print_read_phase(
            "attributed_copy",
            expected.len(),
            before,
            value.read_observation(),
        );

        // 与前面不可变容器使用同一Frozen实例及累计计数；不重置计数或换新源隐藏whole请求。
        let before = value.read_observation();
        let mutable = NSMutableAttributedString::initWithString(
            NSMutableAttributedString::alloc(),
            foundation,
        );
        print_read_phase(
            "mutable_attributed_init",
            expected.len(),
            before,
            value.read_observation(),
        );
        let first_key = ns_string!("OmniFrozenFirstAttribute");
        let second_key = ns_string!("OmniFrozenSecondAttribute");
        let first_attributes = numeric_attributes(first_key, 17);
        let second_attributes = numeric_attributes(second_key, 29);
        let before = value.read_observation();
        // SAFETY: 字典真实为NSString键/NSObject值；3..20在接收者真实范围内，只写属性不改字符。
        unsafe { mutable.setAttributes_range(Some(&first_attributes), NSRange::new(3, 17)) };
        print_read_phase(
            "mutable_set_attributes",
            expected.len(),
            before,
            value.read_observation(),
        );
        let before = value.read_observation();
        // SAFETY: 第二真实字典与第一段不相邻，137..160有效；add只改变这段属性。
        unsafe { mutable.addAttributes_range(&second_attributes, NSRange::new(137, 23)) };
        print_read_phase(
            "mutable_add_attributes",
            expected.len(),
            before,
            value.read_observation(),
        );

        let before = value.read_observation();
        // SAFETY: 公开copy家族把mutable属性容器转为不可变值；实际读取完整保留在共享计数中。
        let immutable: Retained<NSAttributedString> = unsafe { msg_send![&*mutable, copy] };
        print_read_phase(
            "mutable_to_immutable_copy",
            expected.len(),
            before,
            value.read_observation(),
        );
        let before = value.read_observation();
        let entire = immutable.attributedSubstringFromRange(NSRange::new(0, expected.len()));
        print_read_phase(
            "styled_full_attributed_substring",
            expected.len(),
            before,
            value.read_observation(),
        );
        let before = value.read_observation();
        // SAFETY: 同一全范围属性值的公开copy仍返回一个+1；whole请求若出现只记事实，不当失败掩盖。
        let entire_copy: Retained<NSAttributedString> = unsafe { msg_send![&*entire, copy] };
        print_read_phase(
            "styled_full_attributed_copy",
            expected.len(),
            before,
            value.read_observation(),
        );

        document
            .replace_utf16_range(0..1, "未来😀", MarkdownTextSelection::caret(0))
            .unwrap();
        drop(document);
        drop(directory);
        // 阶段观测完成后再验证正文，避免测试oracle请求混入包装/子串/copy的增量归因。
        assert_units(foundation, &expected);
        assert_units(&attributed.string(), &expected);
        assert_units(&copied.string(), &expected);
        assert_units(&substring.string(), &expected[1..129]);
        let styled_values: [&NSAttributedString; 4] = [&mutable, &immutable, &entire, &entire_copy];
        for styled in styled_values {
            // 原文档编辑并销毁后，四个属性值仍保存原字符和两段不相邻的真实字典。
            assert_units(&styled.string(), &expected);
            assert_numeric_attribute(styled, first_key, 3, Some(17));
            assert_numeric_attribute(styled, first_key, 19, Some(17));
            assert_numeric_attribute(styled, first_key, 20, None);
            assert_numeric_attribute(styled, first_key, 137, None);
            assert_numeric_attribute(styled, second_key, 137, Some(29));
            assert_numeric_attribute(styled, second_key, 159, Some(29));
            assert_numeric_attribute(styled, second_key, 160, None);
            assert_numeric_attribute(styled, second_key, 3, None);
            assert_numeric_attribute(styled, first_key, 80, None);
            assert_numeric_attribute(styled, second_key, 80, None);
        }

        // weak只指向实际正文容器；不将小型tagged NSNumber/静态键的存活误作正文资源泄漏。
        let native_weaks = vec![
            Weak::<AnyObject>::new(&value),
            Weak::<AnyObject>::new(&attributed),
            Weak::<AnyObject>::new(&substring),
            Weak::<AnyObject>::new(&copied),
            Weak::<AnyObject>::new(&mutable),
            Weak::<AnyObject>::new(&immutable),
            Weak::<AnyObject>::new(&entire),
            Weak::<AnyObject>::new(&entire_copy),
        ];
        drop(attributed);
        drop(substring);
        drop(copied);
        drop(mutable);
        drop(immutable);
        drop(entire);
        drop(entire_copy);
        drop(value);
        drop(source);
        (weak_source, native_weaks)
    });
    for weak in native_weaks {
        // 同步操作完成且池已排空后核真实对象释放，不能只看局部变量已经drop。
        assert!(weak.load().is_none());
    }
    assert!(weak_source.upgrade().is_none());
    println!(
        "frozen_native_string stage=attributed_container_weak_release result=pass synthetic_only=true"
    );
}

fn observe_single_dictionary_attributed_constructor() {
    let (weak_source, native_weaks) = autoreleasepool(|_| {
        let source_text = "中😀Arabic العربية עבריתe\u{301} ".repeat(192);
        let expected: Vec<u16> = source_text.encode_utf16().collect();
        let (directory, mut document) = fixture(&source_text);
        let source = capture_source(&document);
        let weak_source = source.downgrade();
        let value = FrozenMarkdownString::new(source.clone(), 0..source.utf16_len()).unwrap();
        let foundation: &NSString = &value;
        let key = ns_string!("OmniFrozenUniformAttribute");
        let attributes = numeric_attributes(key, 41);
        let before = value.read_observation();
        // SAFETY: 真实NSString键/NSNumber值满足公开泛型合同；整个输入来自已冻结的不可变值。
        let attributed = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                foundation,
                Some(&attributes),
            )
        };
        print_read_phase(
            "single_dictionary_attributed_init",
            expected.len(),
            before,
            value.read_observation(),
        );
        let before = value.read_observation();
        // SAFETY: 单字典不可变属性值的公开copy返回实际+1；不假设字符是否复制或共享。
        let copied: Retained<NSAttributedString> = unsafe { msg_send![&*attributed, copy] };
        print_read_phase(
            "single_dictionary_attributed_copy",
            expected.len(),
            before,
            value.read_observation(),
        );
        document
            .replace_utf16_range(0..1, "新版本😀", MarkdownTextSelection::caret(0))
            .unwrap();
        drop(document);
        drop(directory);

        // 所有归因采样结束后才运行完整oracle；编辑不应改变原Frozen与属性值的文本/字典。
        assert_units(foundation, &expected);
        for attributed in [&*attributed, &*copied] {
            assert_units(&attributed.string(), &expected);
            for index in [0, expected.len() / 2, expected.len() - 1] {
                // 同一真实字典覆盖全范围，无需穷举每个UTF-16单元来验证样式。
                assert_numeric_attribute(attributed, key, index, Some(41));
            }
        }
        let native_weaks = vec![
            Weak::<AnyObject>::new(&value),
            Weak::<AnyObject>::new(&attributed),
            Weak::<AnyObject>::new(&copied),
        ];
        drop(attributed);
        drop(copied);
        drop(value);
        drop(source);
        (weak_source, native_weaks)
    });
    for weak in native_weaks {
        // 单字典快捷路径也必须真实释放，不按没有bulk请求推断资源生命周期。
        assert!(weak.load().is_none());
    }
    assert!(weak_source.upgrade().is_none());
    println!(
        "frozen_native_string stage=single_dictionary_weak_release result=pass synthetic_only=true"
    );
}

struct ReleaseCount(AtomicUsize);
impl FrozenValueReleaseObserver for ReleaseCount {
    fn released(&self) {
        // 只累计原子释放信号，不捕获正文、Native值或后继业务动作。
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn is_cancelled(&self) -> bool {
        // 此独立控制不取消观察；不获取family锁或触发任何回调。
        false
    }
}

static READ_REQUESTS: Mutex<[Option<FrozenReadRequest>; 16]> = Mutex::new([None; 16]);

fn read_hook(request: FrozenReadRequest) {
    // 固定上限的纯标量观测，不持业务owner、不执行Native，也不在poison时panic跨外来ABI。
    let mut records = READ_REQUESTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(slot) = records.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(request);
    }
}

fn sealed_family_and_background_foundation_values() {
    let family = FrozenValueFamily::new();
    let released = Arc::new(ReleaseCount(AtomicUsize::new(0)));
    for role in [
        FrozenValueObserverRole::Editor,
        FrozenValueObserverRole::FullGraph,
        FrozenValueObserverRole::Resources,
    ] {
        // 三个既有角色独立登记同一纯原子观察器，各应只消费一次最终事实。
        family.observe(role, released.clone()).unwrap();
    }
    let (weak_value, weak_source, held_source) = autoreleasepool(|_| {
        let (directory, document) = fixture("前😀后文");
        let source = FrozenNativeSource::capture(&document, &family, Some(read_hook)).unwrap();
        assert_eq!(source.text_checking_paragraph_range(0..0).unwrap(), 0..5);
        let weak_source = source.downgrade();
        let value = FrozenMarkdownString::new(source.clone(), 1..4).unwrap();
        let foundation: &NSString = &value;
        assert_eq!(foundation.characterAtIndex(1), 0xde00);
        let mut actual = [0; 2];
        // SAFETY: 目标覆盖两个真实单元，从receiver的低代理项开始仍是合法UTF-16读取。
        unsafe {
            foundation.getCharacters_range(
                NonNull::new(actual.as_mut_ptr()).unwrap(),
                NSRange::new(1, 2),
            )
        };
        assert_eq!(actual, [0xde00, 0x540e]);
        let mut unchanged = [0xbeef];
        expect_range_exception(AssertUnwindSafe(|| {
            // SAFETY: 目标真实可写，非法receiver范围必须在形成Rust slice前通知hook并抛异常。
            unsafe {
                foundation.getCharacters_range(
                    NonNull::new(unchanged.as_mut_ptr()).unwrap(),
                    NSRange::new(3, 1),
                )
            }
        }));
        assert_eq!(unchanged, [0xbeef]);
        let before_copy = family.snapshot();
        // SAFETY: 不可变copy返回同一对象+1，不应增加实际StringValue构造数。
        let copied: Retained<NSString> = unsafe { msg_send![foundation, copy] };
        assert_eq!(family.snapshot().constructed, before_copy.constructed);
        drop(copied);

        family.seal_sources();
        assert!(matches!(
            FrozenNativeSource::capture(&document, &family, None),
            Err(FrozenValueFamilyError::SourcesSealed)
        ));
        assert!(!family.snapshot().payload_value_destructors_complete);
        let worker_value = value.clone();
        std::thread::spawn(move || {
            let weak = autoreleasepool(|_| {
                // 具体FrozenMarkdownString确实Send+Sync；此线程拥有自己的pool与真实Retained。
                let partial = worker_value.substringWithRange(NSRange::new(0, 2));
                // SAFETY: 公开copy家族返回同一不可变child的+1，不新建资源或撤销旧值。
                let copied: Retained<NSString> = unsafe { msg_send![&*partial, copy] };
                assert_eq!(read_units(&copied), [0xd83d, 0xde00]);
                let weak = Weak::from_retained(&partial);
                drop(copied);
                drop(partial);
                drop(worker_value);
                weak
            });
            assert!(weak.load().is_none());
        })
        .join()
        .expect("后台Foundation字符串控制");
        assert_eq!(family.snapshot().active[2], 1);
        let held_source = weak_source.upgrade().unwrap();
        let weak_value = Weak::from_retained(&value);
        drop(value);
        drop(source);
        drop(document);
        drop(directory);
        (weak_value, weak_source, held_source)
    });
    assert!(weak_value.load().is_none());
    let held = family.snapshot();
    assert_eq!(held.active, [1, 0, 0, 0]);
    assert!(!held.payload_value_destructors_complete);
    assert_eq!(released.0.load(Ordering::Relaxed), 0);
    drop(held_source);
    assert!(weak_source.upgrade().is_none());
    let completed = family.snapshot();
    assert_eq!(completed.active, [0; 4]);
    assert_eq!(completed.constructed, completed.destructed);
    assert!(completed.payload_value_destructors_complete);
    assert!(!completed.sticky_failure);
    assert_eq!(released.0.load(Ordering::Relaxed), 3);

    let records = READ_REQUESTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        records
            .iter()
            .flatten()
            .any(|record| record.kind == FrozenReadKind::Character
                && record.snapshot_utf16_len == 5
                && record.receiver_start == 1
                && record.receiver_length == 3
                && record.local_start == 1
                && record.requested_length == 1
                && record.checked_global_start == Some(2)
                && record.checked_global_end == Some(3)
                && record.receiver_valid
                && record.request_valid)
    );
    assert!(
        records
            .iter()
            .flatten()
            .any(|record| record.kind == FrozenReadKind::Bulk
                && record.receiver_start == 1
                && record.receiver_length == 3
                && record.local_start == 3
                && record.requested_length == 1
                && record.checked_global_start == Some(4)
                && record.checked_global_end == Some(5)
                && record.receiver_valid
                && !record.request_valid)
    );
    println!(
        "frozen_native_string stage=sealed_family_background_hook_release result=pass synthetic_only=true"
    );
}

fn main() {
    fn assert_send_sync<T: Send + Sync>() {}
    // 独立进程从真实main线程开始，但仅操作Foundation不可变值；类型边界还必须实际Send+Sync。
    let _main_thread = MainThreadMarker::new().expect("不可变字符串控制须在独立进程主线程启动");
    assert_send_sync::<FrozenNativeSource>();
    assert_send_sync::<FrozenMarkdownString>();
    primitives_copy_and_true_subranges();
    values_survive_edit_history_document_and_pool_release();
    empty_value_contract();
    observe_standard_attributed_containers();
    observe_single_dictionary_attributed_constructor();
    sealed_family_and_background_foundation_values();
    println!(
        "frozen_native_string stage=complete result=pass synthetic_only=true product_memory_claim=false"
    );
}
