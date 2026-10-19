//! 有限合成Foundation控制：属性值共享冻结正文，oracle在无bulk门禁之后才读取字符。

#[path = "../src/native_surface/frozen_annotation.rs"]
mod frozen_annotation;
#[path = "../src/native_surface/frozen_native_string.rs"]
mod frozen_native_string;

#[path = "../src/native_surface/frozen_native_source.rs"]
mod frozen_native_source;
#[path = "../src/native_surface/frozen_value_family.rs"]
// 此独立main只验证Native值集成；Family纯测试由另一真实harness执行，不屏蔽生产源警告。
#[allow(dead_code, unused_imports)]
mod frozen_value_family;

use std::cell::RefCell;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use frozen_annotation::{
    FrozenAnnotation, FrozenAnnotationAttributes, FrozenAnnotationError, FrozenAnnotationRun,
};
use frozen_native_source::FrozenNativeSource;
use frozen_native_string::{FrozenMarkdownString, FrozenMarkdownStringReadObservation};
use frozen_value_family::{
    FrozenValueFamily, FrozenValueFamilyError, FrozenValueObserverRole, FrozenValueReleaseObserver,
};
use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{AnyThread, msg_send};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSDictionary, NSMutableAttributedString,
    NSMutableDictionary, NSMutableString, NSNull, NSNumber, NSObject, NSObjectProtocol, NSRange,
    NSString, ns_string,
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
    let directory = tempdir().expect("创建属性值合成夹具目录");
    let path = directory.path().join("frozen-annotation.md");
    std::fs::write(&path, source).expect("写入属性值合成夹具");
    let document = MarkdownDocumentBuffer::open(&path, 4 * 1024 * 1024).unwrap();
    (directory, document)
}

fn read_units(value: &NSString) -> Vec<u16> {
    // 完整读取仅用于门禁后有限oracle，不进入不可变值实现，也不混入构造/属性查询的计量。
    let mut units = vec![0; value.length()];
    if !units.is_empty() {
        // SAFETY: Vec提供真实length个对齐独占单元，公开范围恰好覆盖此有限值。
        unsafe {
            value.getCharacters_range(
                NonNull::new(units.as_mut_ptr()).unwrap(),
                NSRange::new(0, units.len()),
            )
        };
    }
    units
}

fn no_payload_request(
    phase: &'static str,
    source: &FrozenMarkdownString,
    before: FrozenMarkdownStringReadObservation,
) {
    let after = source.read_observation();
    println!(
        "frozen_annotation stage={phase} bulk_delta={} units_delta={} full_snapshot_bulk_delta={} cumulative_max_bulk_units={} synthetic_only=true allocation_claim=false",
        after.bulk_calls - before.bulk_calls,
        after.requested_units - before.requested_units,
        after.full_snapshot_bulk_calls - before.full_snapshot_bulk_calls,
        after.maximum_bulk_units,
    );
    // 这些阶段只应操作真实冻结值与run元数据；任何字符请求都保留为negative，不能截断或reset。
    assert_eq!(after.bulk_calls, before.bulk_calls);
    assert_eq!(after.character_calls, before.character_calls);
    assert_eq!(after.requested_units, before.requested_units);
    assert_eq!(
        after.full_snapshot_bulk_calls,
        before.full_snapshot_bulk_calls
    );
}

fn dictionary(kind: &NSString, weight: usize) -> Retained<FrozenAnnotationAttributes> {
    let number = NSNumber::new_usize(weight);
    let null = NSNull::null();
    let kind_object: &AnyObject = kind;
    let number_object: &AnyObject = &number;
    let null_object: &AnyObject = &null;
    NSDictionary::from_slices(
        &[
            ns_string!("kind"),
            ns_string!("weight"),
            ns_string!("nothing"),
        ],
        &[kind_object, number_object, null_object],
    )
}

type MutableAttributes = NSMutableDictionary<NSString, AnyObject>;

fn runs() -> (
    Vec<FrozenAnnotationRun>,
    Retained<MutableAttributes>,
    Retained<NSMutableString>,
) {
    let first = dictionary(ns_string!("标题"), 17);
    let same = dictionary(ns_string!("标题"), 17);
    let different = dictionary(ns_string!("标题"), 29);
    let shallow = NSMutableString::new();
    shallow.appendString(ns_string!("共享属性值"));
    let number = NSNumber::new_usize(41);
    let null = NSNull::null();
    let kind_object: &AnyObject = ns_string!("正文");
    let number_object: &AnyObject = &number;
    let null_object: &AnyObject = &null;
    let shallow_object: &AnyObject = &shallow;
    let mutable = MutableAttributes::from_slices(
        &[
            ns_string!("kind"),
            ns_string!("weight"),
            ns_string!("nothing"),
            ns_string!("shallow"),
        ],
        &[kind_object, number_object, null_object, shallow_object],
    );
    let other_object: &AnyObject = ns_string!("另一属性");
    let other = FrozenAnnotationAttributes::from_slices(&[ns_string!("other")], &[other_object]);
    let records = vec![
        FrozenAnnotationRun {
            range: 0..4,
            attributes: first,
        },
        FrozenAnnotationRun {
            range: 4..8,
            attributes: same,
        },
        FrozenAnnotationRun {
            range: 8..12,
            attributes: different,
        },
        FrozenAnnotationRun {
            range: 17..20,
            attributes: FrozenAnnotationAttributes::new(),
        },
        FrozenAnnotationRun {
            range: 25..35,
            attributes: mutable.clone().into_super(),
        },
        FrozenAnnotationRun {
            range: 35..44,
            attributes: other,
        },
        FrozenAnnotationRun {
            range: 60..70,
            attributes: dictionary(ns_string!("标题"), 17),
        },
    ];
    (records, mutable, shallow)
}

fn copied_runs(records: &[FrozenAnnotationRun]) -> Vec<FrozenAnnotationRun> {
    records
        .iter()
        .map(|record| {
            // SAFETY: oracle保留构造时同一字典映射；只copy容器，属性值保留相同浅引用。
            let attributes: Retained<FrozenAnnotationAttributes> =
                unsafe { msg_send![&*record.attributes, copy] };
            FrozenAnnotationRun {
                range: record.range.clone(),
                attributes,
            }
        })
        .collect()
}

fn oracle(source_text: &str, records: &[FrozenAnnotationRun]) -> Retained<NSAttributedString> {
    // oracle在无payload门禁后才构造；使用独立有限普通NSString，不接收被测Frozen源。
    let string = NSString::from_str(source_text);
    let mutable =
        NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &string);
    for record in records {
        // SAFETY: 这些真实有序范围已由被测构造入口校验，字典键/值为正确Foundation类型。
        unsafe {
            mutable.setAttributes_range(
                Some(&record.attributes),
                NSRange::new(record.range.start, record.range.len()),
            )
        };
    }
    // SAFETY: 公开copy家族返回一个实际不可变NSAttributedString的+1 claim。
    unsafe { msg_send![&*mutable, copy] }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RangeOutcome {
    Returned,
    RangeException,
}

fn range_outcome<T>(
    case: &'static str,
    backend: &'static str,
    operation: impl FnOnce() -> T + UnwindSafe,
) -> RangeOutcome {
    println!(
        "frozen_annotation stage=range_control case={case} backend={backend} phase=begin synthetic_only=true"
    );
    let exception = match objc2::exception::catch(operation) {
        Ok(value) => {
            // 标准默认API可能在NULL输出时忽略limit；记录真实返回，不把它误报成异常。
            drop(value);
            println!(
                "frozen_annotation stage=range_control case={case} backend={backend} outcome=returned synthetic_only=true"
            );
            return RangeOutcome::Returned;
        }
        Err(Some(exception)) => exception,
        Err(None) => panic!("{case}/{backend}: 收到nil异常，不能证明公开范围合同"),
    };
    // SAFETY: 被测实现及Foundation oracle只应返回公开NSException，name属性为NSString。
    let name: Retained<NSString> = unsafe { msg_send![&*exception, name] };
    assert!(
        name.isEqualToString(ns_string!("NSRangeException")),
        "{case}/{backend}: 不是预期的NSRangeException"
    );
    println!(
        "frozen_annotation stage=range_control case={case} backend={backend} outcome=range_exception synthetic_only=true"
    );
    RangeOutcome::RangeException
}

fn compared_range_outcomes<T, U>(
    case: &'static str,
    frozen: impl FnOnce() -> T + UnwindSafe,
    standard: impl FnOnce() -> U + UnwindSafe,
) -> (RangeOutcome, RangeOutcome) {
    // 分别捕获真正ObjC结果；不因第一项返回就提前panic，保证同参数标准控制也实际执行。
    let frozen = range_outcome(case, "frozen", frozen);
    let standard = range_outcome(case, "standard", standard);
    (frozen, standard)
}

fn compare_metadata(value: &NSAttributedString, oracle: &NSAttributedString) {
    let length = value.length();
    assert_eq!(length, oracle.length());
    assert_eq!(value.string().length(), length);
    if length == 0 {
        // 空substring只需保持真实长度；没有可查询的字符索引。
        return;
    }
    let mut indices = vec![0, length - 1, length / 2];
    indices.extend((1..length.min(75)).step_by(2));
    indices.sort_unstable();
    indices.dedup();
    for index in indices {
        let mut effective = NSRange::new(usize::MAX, usize::MAX);
        let mut oracle_maximum = NSRange::new(usize::MAX, usize::MAX);
        // SAFETY: index已在真实长度内，两个range输出都指向活着的可写栈值。
        let actual = unsafe { value.attributesAtIndex_effectiveRange(index, &mut effective) };
        // SAFETY: oracle索引与完整limit有效；输出是独占栈上NSRange。
        let expected = unsafe {
            oracle.attributesAtIndex_longestEffectiveRange_inRange(
                index,
                &mut oracle_maximum,
                NSRange::new(0, length),
            )
        };
        assert!(actual.isEqualToDictionary(&expected));
        assert!(effective.length > 0);
        assert!(effective.location <= index && index < effective.location + effective.length);
        assert!(effective.location >= oracle_maximum.location);
        assert!(
            effective.location + effective.length
                <= oracle_maximum.location + oracle_maximum.length
        );

        // 包括不包含index的合法limit；真实最长范围按公开Foundation裁切行为比较。
        for limit in [
            NSRange::new(0, length),
            NSRange::new(0, length.min(10)),
            NSRange::new(length / 3, length / 3),
            NSRange::new(0, 0),
            NSRange::new(length, 0),
        ] {
            let mut actual_range = NSRange::new(usize::MAX, usize::MAX);
            let mut expected_range = NSRange::new(usize::MAX, usize::MAX);
            // SAFETY: 合法index/limit与独占可写输出，不能以NULL优化跳过limit校验。
            let actual = unsafe {
                value.attributesAtIndex_longestEffectiveRange_inRange(
                    index,
                    &mut actual_range,
                    limit,
                )
            };
            // SAFETY: 同一真实范围原样交给标准oracle，取得实际最大同字典范围。
            let expected = unsafe {
                oracle.attributesAtIndex_longestEffectiveRange_inRange(
                    index,
                    &mut expected_range,
                    limit,
                )
            };
            assert!(actual.isEqualToDictionary(&expected));
            assert_eq!(actual_range, expected_range, "all-attributes最长范围不一致");
            for key in [
                ns_string!("kind"),
                ns_string!("weight"),
                ns_string!("missing"),
            ] {
                let mut actual_range = NSRange::new(usize::MAX, usize::MAX);
                let mut expected_range = NSRange::new(usize::MAX, usize::MAX);
                // SAFETY: 默认单属性API必须忽略其它属性变化，仍使用合法index/limit/输出。
                let actual = unsafe {
                    value.attribute_atIndex_longestEffectiveRange_inRange(
                        key,
                        index,
                        &mut actual_range,
                        limit,
                    )
                };
                // SAFETY: 标准oracle接收同样参数，Nil属性同样具有真实连续最大范围。
                let expected = unsafe {
                    oracle.attribute_atIndex_longestEffectiveRange_inRange(
                        key,
                        index,
                        &mut expected_range,
                        limit,
                    )
                };
                match (actual, expected) {
                    (Some(actual), Some(expected)) => {
                        // 这些值均由本夹具以Foundation类构造；仍先运行时核NSObject类型，
                        // 不把任意AnyObject直接假定为NSObject后调用协议方法。
                        let actual = actual
                            .downcast_ref::<NSObject>()
                            .expect("被测属性应为Foundation对象");
                        let expected = expected
                            .downcast_ref::<NSObject>()
                            .expect("oracle属性应为Foundation对象");
                        assert!(actual.isEqual(Some(expected.as_ref())));
                    }
                    (None, None) => {} // 属性不存在是合法结果，仍须比较其实际最大范围。
                    _ => panic!("单属性存在性与标准oracle不一致"),
                }
                assert_eq!(
                    actual_range, expected_range,
                    "named-attribute最长范围不一致"
                );
            }
        }
    }
}

fn typed_values(value: &NSAttributedString, shallow: &NSMutableString) {
    // SAFETY: 真实非空夹具在0与25均有属性；NULL明确表示本次不请求range输出。
    let first = unsafe { value.attributesAtIndex_effectiveRange(0, std::ptr::null_mut()) };
    let kind = first.objectForKey(ns_string!("kind")).unwrap();
    assert!(
        kind.downcast_ref::<NSString>()
            .unwrap()
            .isEqualToString(ns_string!("标题"))
    );
    let weight = first.objectForKey(ns_string!("weight")).unwrap();
    assert_eq!(weight.downcast_ref::<NSNumber>().unwrap().as_usize(), 17);
    let null = first.objectForKey(ns_string!("nothing")).unwrap();
    // NSNull值来自本夹具的公开工厂，动态下转仍核实际类型，不对未知AnyObject发NSObject消息。
    assert!(null.downcast_ref::<NSNull>().is_some());
    // SAFETY: 25是真实C段起点；只借容器，不修改其冻结映射。
    let later = unsafe { value.attributesAtIndex_effectiveRange(25, std::ptr::null_mut()) };
    let weight = later.objectForKey(ns_string!("weight")).unwrap();
    assert_eq!(weight.downcast_ref::<NSNumber>().unwrap().as_usize(), 41);
    assert!(later.objectForKey(ns_string!("changed")).is_none());
    let retained_value = later.objectForKey(ns_string!("shallow")).unwrap();
    let shallow_object: &AnyObject = shallow;
    assert!(std::ptr::eq(&*retained_value, shallow_object));
    assert!(
        retained_value
            .downcast_ref::<NSString>()
            .unwrap()
            .isEqualToString(ns_string!("共享属性值已改变"))
    );
}

fn frozen_annotations_keep_values_and_release_resources() {
    let source_text = "头😀中文 abc العربية עברית e\u{301} 尾|".repeat(128);
    let expected_units: Vec<u16> = source_text.encode_utf16().collect();
    let (weak_source, weak_document, native_weaks) = autoreleasepool(|_| {
        let (directory, document) = fixture(&source_text);
        let document = Rc::new(RefCell::new(document));
        let weak_document = Rc::downgrade(&document);
        let snapshot = capture_source(&document.borrow());
        let weak_source = snapshot.downgrade();
        let source = FrozenMarkdownString::new(snapshot.clone(), 0..snapshot.utf16_len()).unwrap();
        let before = source.read_observation();
        let (records, input_dictionary, shallow) = runs();
        let oracle_records = copied_runs(&records);
        let value = FrozenAnnotation::new(source.clone(), records).unwrap();
        no_payload_request("construct", &source, before);
        let foundation: &NSAttributedString = &value;
        let before = source.read_observation();
        // SAFETY: copy选择子属于公开copy家族，返回一个实际+1不可变属性值。
        let copied: Retained<NSAttributedString> = unsafe { msg_send![foundation, copy] };
        assert!(std::ptr::eq(foundation, &*copied));
        let full = foundation.attributedSubstringFromRange(NSRange::new(0, expected_units.len()));
        let partial = foundation.attributedSubstringFromRange(NSRange::new(3, 41));
        let nested = partial.attributedSubstringFromRange(NSRange::new(1, 29));
        let low = foundation.attributedSubstringFromRange(NSRange::new(2, 1));
        let empty = foundation.attributedSubstringFromRange(NSRange::new(expected_units.len(), 0));
        no_payload_request("copy_full_partial_nested_substrings", &source, before);

        // 字典结构冻结，但任意属性对象保留标准浅语义，不将它们重新编码或伪装为深不可变。
        input_dictionary.removeAllObjects();
        shallow.appendString(ns_string!("已改变"));
        let before = source.read_observation();
        typed_values(foundation, &shallow);
        // 构造/copy阶段已采样封存；此oracle仅用独立普通NSString，专门核同参数异常行为。
        let standard = oracle(&source_text, &oracle_records);
        let eof = compared_range_outcomes(
            "attributes_eof_null",
            AssertUnwindSafe(|| {
                // SAFETY: 故意非法index核公开异常；NULL是合法输出选项。
                unsafe {
                    foundation.attributesAtIndex_effectiveRange(
                        expected_units.len(),
                        std::ptr::null_mut(),
                    )
                }
            }),
            AssertUnwindSafe(|| {
                // SAFETY: 标准容器接收完全相同的非法index与NULL，官方catch接收实际异常。
                unsafe {
                    standard.attributesAtIndex_effectiveRange(
                        expected_units.len(),
                        std::ptr::null_mut(),
                    )
                }
            }),
        );
        let substring = compared_range_outcomes(
            "substring_overflow",
            AssertUnwindSafe(|| {
                foundation.attributedSubstringFromRange(NSRange::new(usize::MAX, 2))
            }),
            AssertUnwindSafe(|| standard.attributedSubstringFromRange(NSRange::new(usize::MAX, 2))),
        );
        let all_null = compared_range_outcomes(
            "all_longest_overflow_null",
            AssertUnwindSafe(|| {
                // SAFETY: 同一非法limit与NULL输出；被测显式override保留原范围校验合同。
                unsafe {
                    foundation.attributesAtIndex_longestEffectiveRange_inRange(
                        0,
                        std::ptr::null_mut(),
                        NSRange::new(usize::MAX, 1),
                    )
                }
            }),
            AssertUnwindSafe(|| {
                // SAFETY: 标准默认API是否忽略NULL的limit由真实结果记录，不预设一定抛异常。
                unsafe {
                    standard.attributesAtIndex_longestEffectiveRange_inRange(
                        0,
                        std::ptr::null_mut(),
                        NSRange::new(usize::MAX, 1),
                    )
                }
            }),
        );
        let named_null = compared_range_outcomes(
            "named_longest_overflow_null",
            AssertUnwindSafe(|| {
                // SAFETY: 默认named API的同参数行为交给标准控制比较，不人为要求更严格异常。
                unsafe {
                    foundation.attribute_atIndex_longestEffectiveRange_inRange(
                        ns_string!("kind"),
                        0,
                        std::ptr::null_mut(),
                        NSRange::new(usize::MAX, 1),
                    )
                }
            }),
            AssertUnwindSafe(|| {
                // SAFETY: 标准named API接收同一key/index/NULL/非法limit，捕获真实返回或NSRangeException。
                unsafe {
                    standard.attribute_atIndex_longestEffectiveRange_inRange(
                        ns_string!("kind"),
                        0,
                        std::ptr::null_mut(),
                        NSRange::new(usize::MAX, 1),
                    )
                }
            }),
        );
        let mut untouched = NSRange::new(987, 654);
        let mut standard_output = NSRange::new(987, 654);
        let all_nonnull = compared_range_outcomes(
            "all_longest_outside_nonnull",
            AssertUnwindSafe(|| {
                // SAFETY: 输出指向真实可写栈值；非法limit由官方catch核验，不先写假范围。
                unsafe {
                    foundation.attributesAtIndex_longestEffectiveRange_inRange(
                        0,
                        &mut untouched,
                        NSRange::new(0, expected_units.len() + 1),
                    )
                }
            }),
            AssertUnwindSafe(|| {
                // SAFETY: 标准控制使用独立可写栈值和完全相同的index/非法limit。
                unsafe {
                    standard.attributesAtIndex_longestEffectiveRange_inRange(
                        0,
                        &mut standard_output,
                        NSRange::new(0, expected_units.len() + 1),
                    )
                }
            }),
        );
        // 所有case都已实际执行后才验证，避免首项失败阻止其余标准控制的事实采集。
        for (case, outcomes) in [
            ("attributes_eof_null", eof),
            ("substring_overflow", substring),
            ("all_longest_overflow_null", all_null),
            ("all_longest_outside_nonnull", all_nonnull),
        ] {
            assert_eq!(
                outcomes.0,
                RangeOutcome::RangeException,
                "{case}: 被测显式范围校验未抛异常"
            );
            if outcomes.0 != outcomes.1 {
                // 标准NULL快捷路径的实测差异单独报告；不据此伪称所有API异常行为完全相同。
                println!(
                    "frozen_annotation stage=range_control case={case} explicit_override_differs_from_standard=true synthetic_only=true"
                );
            }
        }
        assert_eq!(
            named_null.0, named_null.1,
            "named_longest_overflow_null: 默认API行为应与标准一致"
        );
        assert_eq!(untouched, NSRange::new(987, 654));
        no_payload_request("typed_values_and_invalid_ranges", &source, before);

        // 复用已完成异常对照的独立标准值；没有重新capture或读取被测Frozen正文。
        let values: Vec<(Retained<NSAttributedString>, Range<usize>)> = vec![
            (value.clone().into_super(), 0..expected_units.len()),
            (copied, 0..expected_units.len()),
            (full, 0..expected_units.len()),
            (partial, 3..44),
            (nested, 4..33),
            (low, 2..3),
            (empty, expected_units.len()..expected_units.len()),
        ];
        let before = source.read_observation();
        for (actual, range) in &values {
            let expected =
                standard.attributedSubstringFromRange(NSRange::new(range.start, range.len()));
            compare_metadata(actual, &expected);
        }
        no_payload_request(
            "primitive_and_default_named_longest_oracle",
            &source,
            before,
        );
        let sealed = source.read_observation();
        assert_eq!(sealed.full_snapshot_bulk_calls, 0);
        assert_eq!(sealed.bulk_calls, 0);

        // 无bulk阶段已封存；下面的显式字符oracle必须完整执行并记量，不reset或隐藏这些请求。
        document
            .borrow_mut()
            .replace_utf16_range(0..1, "后续😀版本", MarkdownTextSelection::caret(0))
            .unwrap();
        document.borrow_mut().undo_text_edit().unwrap();
        document.borrow_mut().redo_text_edit().unwrap();
        drop(document);
        drop(directory);
        assert!(weak_document.upgrade().is_none());
        for (actual, range) in &values {
            assert_eq!(read_units(&actual.string()), expected_units[range.clone()]);
        }
        typed_values(foundation, &shallow);
        let observed = source.read_observation();
        println!(
            "frozen_annotation stage=explicit_payload_oracle bulk_delta={} full_snapshot_bulk_delta={} cumulative_max_bulk_units={} synthetic_only=true explicit_oracle=true",
            observed.bulk_calls - sealed.bulk_calls,
            observed.full_snapshot_bulk_calls - sealed.full_snapshot_bulk_calls,
            observed.maximum_bulk_units,
        );

        let mut native_weaks = vec![
            Weak::<AnyObject>::new(&value),
            Weak::<AnyObject>::new(&source),
        ];
        for (actual, _) in &values {
            // 包括partial/nested/EOF对象，每个weak都在真实pool排空后核验。
            native_weaks.push(Weak::<AnyObject>::new(actual));
        }
        drop(values);
        drop(value);
        drop(source);
        drop(snapshot);
        (weak_source, weak_document, native_weaks)
    });
    assert!(weak_document.upgrade().is_none());
    for weak in native_weaks {
        assert!(weak.load().is_none()); // Native对象释放不能用字段清空或强制失效替代。
    }
    assert!(weak_source.upgrade().is_none());
    println!("frozen_annotation stage=values_and_weak_release result=pass synthetic_only=true");
}

fn empty_and_constructor_rejections() {
    autoreleasepool(|_| {
        let (_directory, document) = fixture("");
        let snapshot = capture_source(&document);
        let source = FrozenMarkdownString::new(snapshot, 0..0).unwrap();
        let before = source.read_observation();
        let value = FrozenAnnotation::new(source.clone(), Vec::new()).unwrap();
        assert_eq!(value.length(), 0);
        assert_eq!(value.string().length(), 0);
        assert_eq!(
            value
                .attributedSubstringFromRange(NSRange::new(0, 0))
                .length(),
            0
        );
        let standard =
            NSAttributedString::initWithString(NSAttributedString::alloc(), ns_string!(""));
        let empty = compared_range_outcomes(
            "empty_attributes_eof_null",
            AssertUnwindSafe(|| {
                // SAFETY: 空值没有可读index，NULL允许；记录实际ObjC异常，不写输出。
                unsafe { value.attributesAtIndex_effectiveRange(0, std::ptr::null_mut()) }
            }),
            AssertUnwindSafe(|| {
                // SAFETY: 标准空属性值使用完全相同的index与NULL参数作独立控制。
                unsafe { standard.attributesAtIndex_effectiveRange(0, std::ptr::null_mut()) }
            }),
        );
        assert_eq!(
            empty.0,
            RangeOutcome::RangeException,
            "empty_attributes_eof_null: 被测范围校验未抛异常"
        );
        if empty.0 != empty.1 {
            println!(
                "frozen_annotation stage=range_control case=empty_attributes_eof_null explicit_override_differs_from_standard=true synthetic_only=true"
            );
        }
        no_payload_request("empty", &source, before);

        let (_directory, document) = fixture("abcdef😀");
        let snapshot = capture_source(&document);
        let source = FrozenMarkdownString::new(snapshot.clone(), 0..snapshot.utf16_len()).unwrap();
        let attributes = FrozenAnnotationAttributes::new();
        let malformed = [
            (
                Range { start: 3, end: 2 },
                FrozenAnnotationError::InvalidRunRange,
            ),
            (2..2, FrozenAnnotationError::InvalidRunRange),
            (
                0..source.length() + 1,
                FrozenAnnotationError::InvalidRunRange,
            ),
        ];
        for (range, expected) in malformed {
            let result = FrozenAnnotation::new(
                source.clone(),
                vec![FrozenAnnotationRun {
                    range,
                    attributes: attributes.clone(),
                }],
            );
            assert!(matches!(result, Err(error) if error == expected));
        }
        let result = FrozenAnnotation::new(
            source,
            vec![
                FrozenAnnotationRun {
                    range: 3..5,
                    attributes: attributes.clone(),
                },
                FrozenAnnotationRun {
                    range: 1..2,
                    attributes,
                },
            ],
        );
        assert!(matches!(result, Err(FrozenAnnotationError::RunsNotOrdered)));
    });
    println!(
        "frozen_annotation stage=empty_and_constructor_errors result=pass synthetic_only=true"
    );
}

struct ReleaseCount(AtomicUsize);
impl FrozenValueReleaseObserver for ReleaseCount {
    fn released(&self) {
        // 只接收纯原子完成信号，不能借观察器延长Native值或源载荷生命周期。
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn is_cancelled(&self) -> bool {
        // 固定控制不取消角色，也不重入family或接触任何线程绑定资源。
        false
    }
}

fn sealed_family_and_background_annotation_values() {
    let family = FrozenValueFamily::new();
    let released = Arc::new(ReleaseCount(AtomicUsize::new(0)));
    for role in [
        FrozenValueObserverRole::Editor,
        FrozenValueObserverRole::FullGraph,
        FrozenValueObserverRole::Resources,
    ] {
        // 每个角色有自己的授权槽，最后只接受一次真实payload/value析构事实。
        family.observe(role, released.clone()).unwrap();
    }
    let (weak_value, weak_string, weak_source, held_source) = autoreleasepool(|_| {
        let (directory, document) = fixture(&"前😀后文".repeat(32));
        let source = FrozenNativeSource::capture(&document, &family, None).unwrap();
        assert_eq!(
            source.text_checking_paragraph_range(0..0).unwrap(),
            0..source.utf16_len()
        );
        let weak_source = source.downgrade();
        let string = FrozenMarkdownString::new(source.clone(), 0..source.utf16_len()).unwrap();
        let value = FrozenAnnotation::new(
            string.clone(),
            vec![FrozenAnnotationRun {
                range: 0..source.utf16_len(),
                attributes: dictionary(ns_string!("后台只读"), 73),
            }],
        )
        .unwrap();
        family.seal_sources();
        assert!(matches!(
            FrozenNativeSource::capture(&document, &family, None),
            Err(FrozenValueFamilyError::SourcesSealed)
        ));
        let before_copy = family.snapshot();
        let foundation: &NSAttributedString = &value;
        // SAFETY: 公开copy家族只增加同一不可变对象的+1，不应签发第二个实际value claim。
        let copied: Retained<NSAttributedString> = unsafe { msg_send![foundation, copy] };
        assert_eq!(family.snapshot().constructed, before_copy.constructed);
        drop(copied);
        let address = std::ptr::from_ref(foundation).addr();
        std::thread::spawn(move || {
            let weaks = autoreleasepool(|_| {
                // SAFETY: 仅本合成控制跨线程使用公开Cocoa不可变值。main保留真实Retained并在join
                // 返回前不释放；属性值仅为不可变NSString/NSNumber/NSNull，没有并发修改或AppKit。
                // 此局部受控借用不为通用FrozenAnnotation/任意属性对象添加Rust Send/Sync。
                let value = unsafe { &*(address as *const NSAttributedString) };
                let partial = value.attributedSubstringFromRange(NSRange::new(0, 3));
                // SAFETY: 非copy创建的partial被本线程真实Retained保活，copy返回一个+1 claim。
                let copied: Retained<NSAttributedString> = unsafe { msg_send![&*partial, copy] };
                let text = copied.string();
                assert_eq!(read_units(&text), [0x524d, 0xd83d, 0xde00]);
                // SAFETY: 本线程的真实非空partial索引有效，NULL只免除范围输出。
                let attributes =
                    unsafe { copied.attributesAtIndex_effectiveRange(0, std::ptr::null_mut()) };
                let number = attributes.objectForKey(ns_string!("weight")).unwrap();
                assert_eq!(number.downcast_ref::<NSNumber>().unwrap().as_usize(), 73);
                let weaks = vec![
                    Weak::<AnyObject>::new(&partial),
                    Weak::<AnyObject>::new(&copied),
                    Weak::<AnyObject>::new(&text),
                ];
                drop(text);
                drop(copied);
                drop(partial);
                weaks
            });
            for weak in weaks {
                assert!(weak.load().is_none()); // 后台pool排空，不能只凭claim计数猜测Native对象已销毁。
            }
        })
        .join()
        .expect("后台Foundation属性值控制");

        let current = family.snapshot();
        assert_eq!(current.active, [1, 1, 1, 1]);
        assert!(current.constructed[2] > 1 && current.constructed[3] > 1);
        assert_eq!(current.constructed[1], 1); // partial只共享原Data，不能重复签发payload。
        assert!(!current.payload_value_destructors_complete);
        assert_eq!(string.read_observation().full_snapshot_bulk_calls, 0);
        let held_source = weak_source.upgrade().unwrap();
        let weak_value = Weak::from_retained(&value);
        let weak_string = Weak::from_retained(&string);
        drop(value);
        drop(string);
        drop(source);
        drop(document);
        drop(directory);
        (weak_value, weak_string, weak_source, held_source)
    });
    assert!(weak_value.load().is_none());
    assert!(weak_string.load().is_none());
    assert_eq!(family.snapshot().active, [1, 0, 0, 0]);
    assert!(!family.snapshot().payload_value_destructors_complete);
    assert_eq!(released.0.load(Ordering::Relaxed), 0);
    drop(held_source);
    assert!(weak_source.upgrade().is_none());
    let completed = family.snapshot();
    assert_eq!(completed.active, [0; 4]);
    assert_eq!(completed.constructed, completed.destructed);
    assert!(completed.constructed.iter().all(|count| *count > 0));
    assert!(completed.payload_value_destructors_complete);
    assert!(!completed.sticky_failure);
    assert_eq!(released.0.load(Ordering::Relaxed), 3);
    println!(
        "frozen_annotation stage=sealed_family_background_release result=pass synthetic_only=true"
    );
}

fn main() {
    // 这是Foundation-only独立main，不创建AppKit应用、窗口、定时器或产品生命周期。
    let _main_thread = MainThreadMarker::new().expect("独立属性值控制须从真实main线程启动");
    frozen_annotations_keep_values_and_release_resources();
    empty_and_constructor_rejections();
    sealed_family_and_background_annotation_values();
    println!(
        "frozen_annotation stage=complete result=pass synthetic_only=true product_memory_claim=false"
    );
}
