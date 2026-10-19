//! 标准TextKit真实行元数据与旧位置反查的读取对照；不改变产品、不创建窗口。

#[cfg(target_os = "macos")]
mod control {
    use std::cell::Cell;
    use std::ops::Range;
    use std::ptr::NonNull;
    use std::rc::Rc;

    use block2::RcBlock;
    use objc2::rc::{Retained, Weak, autoreleasepool};
    use objc2::runtime::{AnyClass, AnyObject, Bool, NSZone};
    use objc2::{
        AnyThread, ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send,
    };
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSFont, NSFontAttributeName,
        NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSTextContainer,
        NSTextContentManager, NSTextContentStorage, NSTextElementProvider, NSTextLayoutFragment,
        NSTextLayoutFragmentEnumerationOptions, NSTextLayoutFragmentState, NSTextLayoutManager,
        NSTextLineFragment, NSTextLocation, NSTextRange, NSTextStorage, NSTextStorageObserving,
        NSWritingDirection,
    };
    use objc2_foundation::{
        MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSComparisonResult,
        NSDictionary, NSRange, NSRect, NSSize, NSString, NSUInteger, ns_string, unichar,
    };

    use omni_md_plugin::native_surface::{
        NativeMarkdownLineMetadataError, NativeMarkdownLineMetadataRequest,
        verify_native_markdown_line_metadata,
    };

    const MAXIMUM_SOURCE_UNITS: usize = 165_042;
    // 与离线归因使用同一固定模式；正文由坐标求值，不分配源String或全源UTF16数组。
    const PATTERN: &[u16] = &[
        0x4e2d, 0x6587, 0x8f6f, 0x6362, 0x884c, 0x20, 0x61, 0x6e, 0x63, 0x68, 0x6f, 0x72, 0x20,
        0x0627, 0x0644, 0x0639, 0x0631, 0x0628, 0x064a, 0x0629, 0x20, 0x31, 0x32, 0x33, 0x20,
        0x05e9, 0x05dc, 0x05d5, 0x05dd, 0x20, 0xd83d, 0xde42, 0x20, 0xd83d, 0xdc69, 0xd83c, 0xdffd,
        0x200d, 0xd83d, 0xdcbb, 0x20, 0x65, 0x0301, 0x20, 0x7ee7, 0x7eed, 0x6d4b, 0x8bd5, 0x3002,
        0x20,
    ];

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct Reads {
        characters: usize,
        bulk_calls: usize,
        bulk_units: usize,
        maximum_bulk: usize,
        full_bulk_calls: usize,
        attributed_substrings: usize,
        full_attributed_substrings: usize,
    }

    impl Reads {
        fn source_unchanged(self, after: Self) -> bool {
            // 只比真实primitive累计数；不重置或漏记属性容器触发的读取。
            self.characters == after.characters
                && self.bulk_calls == after.bulk_calls
                && self.bulk_units == after.bulk_units
                && self.attributed_substrings == after.attributed_substrings
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Case {
        Mixed,
        Rtl,
        Prefixed,
        Empty,
        Eof,
        Long,
        OneLine,
    }

    impl Case {
        fn parse(value: Option<String>) -> Self {
            match value.as_deref().unwrap_or("mixed") {
                "mixed" => Self::Mixed,
                "rtl" => Self::Rtl,
                "prefixed" => Self::Prefixed,
                "empty" => Self::Empty,
                "eof" => Self::Eof,
                "long" => Self::Long,
                "one-line" => Self::OneLine,
                _ => panic!("未知行元数据控制case"),
            }
        }

        fn length(self) -> usize {
            match self {
                Self::Empty => 0,
                Self::Eof => 201,
                Self::Prefixed => 206,
                Self::Long | Self::OneLine => MAXIMUM_SOURCE_UNITS,
                Self::Mixed | Self::Rtl => 200,
            }
        }

        fn unit(self, index: usize) -> u16 {
            if self == Self::Prefixed && index < 6 {
                return [0x70, 0x72, 0x65, 0x66, 0x78, 0x0a][index];
            }
            if self == Self::Eof && index + 1 == self.length() {
                return 0x0a;
            }
            let value = PATTERN[index % PATTERN.len()];
            if (index + 1 == self.length() || (self == Self::Eof && index + 2 == self.length()))
                && (0xd800..=0xdbff).contains(&value)
            {
                0x2e // 夹具本身不得在EOF或已声明换行之前留下无配对高代理项。
            } else {
                value
            }
        }
    }

    type Attributes = NSDictionary<NSAttributedStringKey, AnyObject>;

    struct StringIvars {
        case: Case,
        reads: Rc<Cell<Reads>>,
    }

    define_class!(
        // SAFETY: 只在AppKit主线程构造和读取合成不可变NSString；实现真实长度及完整UTF16原语。
        // 没有借用正文或延迟回调，copy明确沿标准NSString super以复现原live适配器路径。
        #[unsafe(super = NSString)]
        #[thread_kind = MainThreadOnly]
        #[ivars = StringIvars]
        struct AlgorithmicString;

        impl AlgorithmicString {
            #[unsafe(method(length))]
            fn source_length(&self) -> NSUInteger { self.ivars().case.length() }

            #[unsafe(method(characterAtIndex:))]
            fn source_character(&self, index: NSUInteger) -> unichar {
                if index >= self.ivars().case.length() { throw_control(ns_string!("NSRangeException")); }
                let mut reads = self.ivars().reads.get();
                reads.characters += 1;
                self.ivars().reads.set(reads);
                self.ivars().case.unit(index)
            }

            #[unsafe(method(getCharacters:range:))]
            /// # Safety
            /// destination须能写入原range.length个对齐UTF16单元；本控制完整初始化原请求。
            unsafe fn source_characters(&self, destination: NonNull<unichar>, range: NSRange) {
                // SAFETY: Foundation传入原目标buffer；helper先检查真实范围才构造可写slice。
                unsafe { self.copy_units(destination, range) };
            }

            #[unsafe(method(getCharacters:))]
            /// # Safety
            /// destination须能写入接收者真实length个UTF16单元。
            unsafe fn source_all_characters(&self, destination: NonNull<unichar>) {
                // SAFETY: 全值请求照常执行并记全文计数，不为控制裁短Foundation请求。
                unsafe { self.copy_units(destination, NSRange::new(0, self.ivars().case.length())) };
            }

            #[unsafe(method_id(copyWithZone:))]
            #[unsafe(method_family = copy)]
            fn source_copy(&self, zone: *mut NSZone) -> Retained<NSString> {
                // SAFETY: 原NSString copy家族+1完整转发；不是共享self的Frozen实验路径。
                unsafe { msg_send![super(self), copyWithZone: zone] }
            }
        }
    );

    impl AlgorithmicString {
        fn new(case: Case, reads: Rc<Cell<Reads>>, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(StringIvars { case, reads });
            // SAFETY: NSString子类按公开NSObject init初始化原分配；ivar已完整建立，返回精确Self。
            let value: Retained<Self> = unsafe { msg_send![super(this), init] };
            assert_eq!(value.class(), Self::class());
            value
        }

        unsafe fn copy_units(&self, destination: NonNull<unichar>, range: NSRange) {
            let length = self.ivars().case.length();
            if range.location > length || range.length > length - range.location {
                throw_control(ns_string!("NSRangeException")); // 非法范围不能先写caller buffer。
            }
            let mut reads = self.ivars().reads.get();
            reads.bulk_calls += 1;
            reads.bulk_units += range.length;
            reads.maximum_bulk = reads.maximum_bulk.max(range.length);
            reads.full_bulk_calls += usize::from(length != 0 && range == NSRange::new(0, length));
            self.ivars().reads.set(reads);
            // SAFETY: 外层原语提供真实长度buffer，范围已校验；空slice不解引用destination。
            let output =
                unsafe { std::slice::from_raw_parts_mut(destination.as_ptr(), range.length) };
            for (offset, unit) in output.iter_mut().enumerate() {
                *unit = self.ivars().case.unit(range.location + offset);
            }
        }
    }

    struct StorageIvars {
        source: Retained<AlgorithmicString>,
        attributes: Retained<Attributes>,
        reads: Rc<Cell<Reads>>,
    }

    define_class!(
        // SAFETY: 标准NSTextStorage四个primitive使用同一个真实不可变合成源和常量属性。
        // 控制不执行正文编辑；任何意外字符/属性写入明确抛出，不能静默丢掉Native行为。
        #[unsafe(super = NSTextStorage)]
        #[thread_kind = MainThreadOnly]
        #[ivars = StorageIvars]
        struct AlgorithmicStorage;

        impl AlgorithmicStorage {
            #[unsafe(method_id(string))]
            fn storage_string(&self) -> Retained<NSString> { self.ivars().source.clone().into_super() }

            #[unsafe(method(length))]
            fn storage_length(&self) -> NSUInteger { self.ivars().source.length() }

            #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
            /// # Safety
            /// effective须为null或同步可写的单个NSRange；非法index先抛出而不写输出。
            unsafe fn storage_attributes(&self, index: NSUInteger, effective: *mut NSRange) -> Retained<Attributes> {
                let length = self.ivars().source.length();
                if index >= length { throw_control(ns_string!("NSRangeException")); }
                self.ensureAttributesAreFixedInRange(NSRange::new(index, 1));
                if !effective.is_null() {
                    // SAFETY: caller提供一个可写输出；整源属性在本不可编辑fixture中确实完全相同。
                    unsafe { effective.write(NSRange::new(0, length)) };
                }
                self.ivars().attributes.clone()
            }

            #[unsafe(method(replaceCharactersInRange:withString:))]
            fn storage_replace(&self, _range: NSRange, _replacement: &NSString) {
                throw_control(ns_string!("NSInternalInconsistencyException"));
            }

            #[unsafe(method(setAttributes:range:))]
            fn storage_set_attributes(&self, _attributes: Option<&Attributes>, _range: NSRange) {
                throw_control(ns_string!("NSInternalInconsistencyException"));
            }

            #[unsafe(method(fixesAttributesLazily))]
            fn storage_fixes_lazily(&self) -> bool { true }

            #[unsafe(method(fixAttributesInRange:))]
            fn storage_fix_attributes(&self, range: NSRange) {
                let length = self.ivars().source.length();
                if range.location > length || range.length > length - range.location { throw_control(ns_string!("NSRangeException")); }
                // 原常量属性已完整有效；不遍历、复制或为fixing重新解析字符。
            }

            #[unsafe(method_id(attributedSubstringFromRange:))]
            fn storage_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
                let mut reads = self.ivars().reads.get();
                reads.attributed_substrings += 1;
                reads.full_attributed_substrings += usize::from(self.ivars().source.length() != 0 && range == NSRange::new(0, self.ivars().source.length()));
                self.ivars().reads.set(reads);
                // SAFETY: 原公开noncopy selector只转发一次；所有原请求完整执行并可计量。
                unsafe { msg_send![super(self), attributedSubstringFromRange: range] }
            }
        }
    );

    impl AlgorithmicStorage {
        fn new(
            source: Retained<AlgorithmicString>,
            attributes: Retained<Attributes>,
            reads: Rc<Cell<Reads>>,
            mtm: MainThreadMarker,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(StorageIvars {
                source,
                attributes,
                reads,
            });
            // SAFETY: 精确NSTextStorage子类按公开init合同初始化，ivar已完整，不绕过PartialInit。
            let value: Retained<Self> = unsafe { msg_send![super(this), init] };
            assert_eq!(value.class(), Self::class());
            value
        }
    }

    fn throw_control(name: &NSString) -> ! {
        let class = AnyClass::get(c"NSException").expect("Foundation异常类");
        // SAFETY: 公开NSException工厂返回真实异常+0，msg_send取得一个Retained Exception；没有正文借用。
        let exception: Retained<objc2::exception::Exception> = unsafe {
            msg_send![class, exceptionWithName: name, reason: ns_string!("Synthetic line metadata contract violation"), userInfo: std::ptr::null::<AnyObject>()]
        };
        objc2::exception::throw(exception)
    }

    fn source_range(
        content: &NSTextContentStorage,
        range: &NSTextRange,
        length: usize,
    ) -> Range<usize> {
        let origin = content.documentRange().location();
        let start = content.offsetFromLocation_toLocation(&origin, &range.location());
        let end = content.offsetFromLocation_toLocation(&origin, &range.endLocation());
        assert!(start >= 0 && end >= start && end as usize <= length);
        start as usize..end as usize
    }

    fn round_trip(content: &NSTextContentStorage, offset: usize) {
        let origin = content.documentRange().location();
        let location = content
            .locationFromLocation_withOffset(&origin, isize::try_from(offset).unwrap())
            .unwrap();
        let actual = content.offsetFromLocation_toLocation(&origin, &location);
        assert_eq!(actual, offset as isize);
        let again = content
            .locationFromLocation_withOffset(&origin, actual)
            .unwrap();
        assert_eq!(location.compare(&again), NSComparisonResult::Same);
        assert_eq!(content.offsetFromLocation_toLocation(&location, &again), 0);
    }

    fn finite_rect(rect: NSRect) -> bool {
        rect.origin.x.is_finite()
            && rect.origin.y.is_finite()
            && rect.size.width.is_finite()
            && rect.size.height.is_finite()
            && rect.size.width >= 0.0
            && rect.size.height >= 0.0
    }

    fn line_for_offset(
        lines: &objc2_foundation::NSArray<NSTextLineFragment>,
        offset: usize,
    ) -> Option<usize> {
        let (mut left, mut right) = (0, lines.count());
        // 固定64次metadata二分，不读取正文或遍历全部5282line；未找到原样Pending。
        for _ in 0..64 {
            if left >= right {
                return None;
            }
            let middle = left + (right - left) / 2;
            let range = lines.objectAtIndex(middle).characterRange();
            let end = range.location.checked_add(range.length).unwrap();
            if offset < range.location {
                right = middle;
            } else if offset > end || (offset == end && range.length != 0) {
                left = middle + 1;
            } else {
                return Some(middle);
            }
        }
        panic!("固定metadata二分预算耗尽");
    }

    fn verify_line(
        case: Case,
        manager: &NSTextLayoutManager,
        content: &NSTextContentStorage,
        fragment: &NSTextLayoutFragment,
        line: &NSTextLineFragment,
        reads: &Cell<Reads>,
        query: usize,
    ) {
        let element = fragment.textElement().expect("实际parent element");
        let element_range = source_range(content, &element.elementRange().unwrap(), case.length());
        let fragment_range = source_range(content, &fragment.rangeInElement(), case.length());
        let range = line.characterRange();
        let end = range.location.checked_add(range.length).unwrap();
        assert!(end <= element_range.len());
        let global = element_range.start + range.location..element_range.start + end;
        assert!(global.start >= fragment_range.start && global.end <= fragment_range.end);
        let frame = fragment.layoutFragmentFrame();
        let bounds = line.typographicBounds();
        assert!(finite_rect(frame) && finite_rect(bounds) && bounds.size.height > 0.0);
        assert!(
            range.length == 0 || bounds.size.width > 0.0,
            "非空line不能以可疑内容placeholder的零宽冒充真实geometry"
        );
        assert_eq!(fragment.state(), NSTextLayoutFragmentState::LayoutAvailable);

        // 旧getter作为真实正对照放在metadata验证之前，避免先触字形点映射预热系统缓存。
        let before = reads.get();
        let start = content
            .locationFromLocation_withOffset(
                &content.documentRange().location(),
                global.start as isize,
            )
            .unwrap();
        let inverse = fragment.textLineFragmentForTextLocation_isUpstreamAffinity(&start, false);
        if range.length != 0 {
            let inverse = inverse.as_ref().expect("原非空line位置反查");
            assert_eq!(inverse.characterRange(), range);
            assert_eq!(inverse.typographicBounds(), bounds);
        } else {
            assert_eq!(global.start, case.length());
            round_trip(content, case.length()); // 排他EOF只验证真实provider end，不要求返回上一行。
        }
        let after_inverse = reads.get();
        println!(
            "native_line_metadata case={case:?} stage=old_inverse query={query} source_start={} source_end={} bulk={} units={} full_bulk={} canonical_substrings={} formal=false",
            global.start,
            global.end,
            after_inverse.bulk_calls - before.bulk_calls,
            after_inverse.bulk_units - before.bulk_units,
            after_inverse.full_bulk_calls - before.full_bulk_calls,
            after_inverse.attributed_substrings - before.attributed_substrings
        );
        if case == Case::Long {
            assert_eq!(
                after_inverse.full_bulk_calls - before.full_bulk_calls,
                1,
                "巨段旧getter真实全文读正对照没有进入，不得把未覆盖当通过"
            );
            assert_eq!(
                after_inverse.full_attributed_substrings - before.full_attributed_substrings,
                1
            );
        }
        drop(inverse);
        drop(start);

        let before_metadata = reads.get();
        let document_start = content.documentRange().location();
        let request = || NativeMarkdownLineMetadataRequest {
            fragment,
            line,
            content,
            document_start: &document_start,
            document_length: case.length(),
            element_source: &element_range,
            source: &global,
            local_range: range,
            bounds,
        };
        if range.length == 0 {
            assert!(matches!(
                verify_native_markdown_line_metadata(request(), &|| true),
                Err(NativeMarkdownLineMetadataError::Source)
            ));
            round_trip(content, case.length()); // 真EOF仍独立认证，普通line helper必须拒绝零源单元。
        } else {
            verify_native_markdown_line_metadata(request(), &|| true)
                .expect("真实生产line metadata helper");
            assert!(matches!(
                verify_native_markdown_line_metadata(request(), &|| false),
                Err(NativeMarkdownLineMetadataError::Revoked)
            ));
            let checks = Cell::new(0usize);
            assert!(
                matches!(
                    verify_native_markdown_line_metadata(request(), &|| {
                        checks.set(checks.get() + 1);
                        checks.get() < 3
                    }),
                    Err(NativeMarkdownLineMetadataError::Revoked)
                ),
                "Native getter之间撤权必须沿原票拒绝"
            );
            let changed_source = global.start + 1..global.end;
            let mut changed = request();
            changed.source = &changed_source;
            assert!(matches!(
                verify_native_markdown_line_metadata(changed, &|| true),
                Err(NativeMarkdownLineMetadataError::Source)
            ));
            let mut overflow = request();
            overflow.local_range = NSRange::new(usize::MAX, 1);
            assert!(matches!(
                verify_native_markdown_line_metadata(overflow, &|| true),
                Err(NativeMarkdownLineMetadataError::Source)
            ));
            let mut changed_geometry = request();
            changed_geometry.bounds.size.height = 0.0;
            assert!(matches!(
                verify_native_markdown_line_metadata(changed_geometry, &|| true),
                Err(NativeMarkdownLineMetadataError::Geometry)
            ));
        }
        let attributed = line.attributedString();
        assert_eq!(
            attributed.length(),
            element_range.len(),
            "characterRange必须相对同一个完整element属性值"
        );
        let middle_y = bounds.origin.y + bounds.size.height / 2.0;
        assert!(middle_y.is_finite());
        let vertical = fragment
            .textLineFragmentForVerticalOffset_requiresExactMatch(middle_y, true)
            .expect("原真实line中点");
        assert!(std::ptr::eq(&*vertical, line));
        assert_eq!(vertical.characterRange(), range);
        assert_eq!(vertical.typographicBounds(), bounds);
        round_trip(content, global.start);
        round_trip(content, global.end);
        let parent_manager = fragment.textLayoutManager().unwrap();
        let parent_content = element.textContentManager().unwrap();
        assert!(std::ptr::eq(&*parent_manager, manager));
        let content_manager: &NSTextContentManager = content;
        assert!(std::ptr::eq(&*parent_content, content_manager));
        assert_eq!(
            source_range(content, &element.elementRange().unwrap(), case.length()),
            element_range
        );
        assert_eq!(
            source_range(content, &fragment.rangeInElement(), case.length()),
            fragment_range
        );
        assert_eq!(fragment.state(), NSTextLayoutFragmentState::LayoutAvailable);
        assert_eq!(fragment.layoutFragmentFrame(), frame);
        assert_eq!(line.characterRange(), range);
        assert_eq!(line.typographicBounds(), bounds);
        assert!(
            before_metadata.source_unchanged(reads.get()),
            "纯范围/几何认证读取了源字符或构造了canonical substring"
        );
        println!(
            "native_line_metadata case={case:?} stage=metadata query={query} element_start={} element_units={} line_start={} line_units={} exact_original_line=true source_reads=0 formal=false",
            element_range.start,
            element_range.len(),
            range.location,
            range.length
        );
    }

    fn run_case(case: Case, mtm: MainThreadMarker) {
        let reads = Rc::new(Cell::new(Reads::default()));
        let weak = autoreleasepool(|_| {
            let source = AlgorithmicString::new(case, Rc::clone(&reads), mtm);
            let source_weak = Weak::from_retained(&source);
            let style = NSMutableParagraphStyle::new();
            style.setBaseWritingDirection(if case == Case::Rtl {
                NSWritingDirection::RightToLeft
            } else {
                NSWritingDirection::Natural
            });
            let font = NSFont::systemFontOfSize(15.0);
            // SAFETY: 字典key均为公开属性常量，值的实际类型是对应NSFont及NSParagraphStyle。
            let attributes: Retained<Attributes> = unsafe {
                NSDictionary::from_retained_objects(
                    &[NSFontAttributeName, NSParagraphStyleAttributeName],
                    &[
                        Retained::cast_unchecked::<AnyObject>(font),
                        Retained::cast_unchecked::<AnyObject>(style),
                    ],
                )
            };
            let storage =
                AlgorithmicStorage::new(source.clone(), attributes, Rc::clone(&reads), mtm);
            let storage_weak = Weak::from_retained(&storage);
            let content = NSTextContentStorage::new();
            let content_weak = Weak::from_retained(&content);
            let manager = NSTextLayoutManager::new();
            let manager_weak = Weak::from_retained(&manager);
            let width = if case == Case::OneLine {
                1_000_000_000.0
            } else {
                260.0
            };
            let container = NSTextContainer::initWithContainerSize(
                NSTextContainer::alloc(),
                NSSize::new(width, f64::MAX),
            );
            let container_weak = Weak::from_retained(&container);
            content.addTextLayoutManager(&manager);
            content.setPrimaryTextLayoutManager(Some(&manager));
            manager.setTextContainer(Some(&container));
            manager.setLimitsLayoutForSuspiciousContents(false); // 控制要求真实非零字形，不能以系统零宽placeholder替代giant行。
            // 本主线程建立标准observer关系，存活storage与content均保留到对称detach之后。
            content.setTextStorage(Some(&storage));
            let before_layout = reads.get();
            manager.ensureLayoutForRange(&content.documentRange());
            let block = RcBlock::new(|_: NonNull<NSTextLayoutFragment>| Bool::YES);
            let enumerated = manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                None,
                NSTextLayoutFragmentEnumerationOptions::EnsuresLayout
                    | NSTextLayoutFragmentEnumerationOptions::EnsuresExtraLineFragment,
                &block,
            );
            drop(enumerated);
            drop(block);
            println!(
                "native_line_metadata case={case:?} stage=initial_layout source_units={} bulk={} full_bulk={} initial_full_layout_intentional=true giant_lazy_pass=false formal=false",
                case.length(),
                reads.get().bulk_calls - before_layout.bulk_calls,
                reads.get().full_bulk_calls - before_layout.full_bulk_calls
            );
            let origin = content.documentRange().location();
            let mut fragment_weak = None;
            let mut line_weak = None;
            let first = usize::from(case == Case::Prefixed) * 6;
            let probes = if matches!(case, Case::Empty | Case::Eof) {
                [case.length(); 5]
            } else {
                [
                    first,
                    first + (case.length() - first) / 2,
                    case.length() - 1,
                    30,
                    31,
                ]
            };
            for (ordinal, query) in probes.into_iter().enumerate() {
                if matches!(case, Case::Empty | Case::Eof) && ordinal != 0 {
                    break;
                }
                let location = content
                    .locationFromLocation_withOffset(&origin, query as isize)
                    .unwrap();
                let mut fragment = manager.textLayoutFragmentForLocation(&location);
                if fragment.is_none() && query == case.length() && case != Case::Empty {
                    // 一次原已布局枚举仅为有限尾随换行fixture找到真实extra line；没有ensure或伪造上一行。
                    let last = Cell::new(None::<Retained<NSTextLayoutFragment>>);
                    let callback = RcBlock::new(|candidate: NonNull<NSTextLayoutFragment>| {
                        // SAFETY: 同步枚举block提供本次调用期间存活的真实fragment。
                        let candidate = unsafe { candidate.as_ref() };
                        if candidate.state() == NSTextLayoutFragmentState::LayoutAvailable
                            && source_range(&content, &candidate.rangeInElement(), case.length())
                                .end
                                == case.length()
                        {
                            last.set(Some(candidate.retain()));
                        }
                        Bool::YES
                    });
                    drop(
                        manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                            None,
                            NSTextLayoutFragmentEnumerationOptions::None,
                            &callback,
                        ),
                    );
                    drop(callback);
                    fragment = last.take();
                }
                if case == Case::Empty {
                    assert!(
                        fragment.as_ref().is_none_or(|fragment| fragment.state()
                            != NSTextLayoutFragmentState::LayoutAvailable)
                    );
                    println!(
                        "native_line_metadata case=Empty stage=pending ready=false source_units=0 fake_line=false formal=false"
                    );
                    break;
                }
                let fragment = fragment.expect("实际ready fragment");
                assert_eq!(fragment.state(), NSTextLayoutFragmentState::LayoutAvailable);
                let element = source_range(
                    &content,
                    &fragment.textElement().unwrap().elementRange().unwrap(),
                    case.length(),
                );
                let lines = fragment.textLineFragments();
                let index = line_for_offset(&lines, query - element.start)
                    .or_else(|| {
                        (query == case.length() && lines.count() != 0).then(|| lines.count() - 1)
                    })
                    .expect("原实际line数组中存在query");
                let line = lines.objectAtIndex(index);
                verify_line(case, &manager, &content, &fragment, &line, &reads, query);
                if ordinal == 0 {
                    fragment_weak = Some(Weak::from_retained(&fragment));
                    line_weak = Some(Weak::from_retained(&line));
                }
            }
            manager.setTextContainer(None);
            content.removeTextLayoutManager(&manager);
            content.setPrimaryTextLayoutManager(None);
            // 同一标准observer在所有同步line读取结束后对称断开；不改正文、selection或资源门禁。
            content.setTextStorage(None);
            (
                source_weak,
                storage_weak,
                content_weak,
                manager_weak,
                container_weak,
                fragment_weak,
                line_weak,
            )
        });
        assert!(weak.0.load().is_none(), "合成原NSString未真实释放");
        assert!(weak.1.load().is_none(), "合成storage未真实释放");
        assert!(weak.2.load().is_none(), "标准content未真实释放");
        assert!(weak.3.load().is_none(), "标准manager未真实释放");
        assert!(weak.4.load().is_none(), "标准container未真实释放");
        assert!(
            weak.5.is_none_or(|value| value.load().is_none()),
            "标准fragment未真实释放"
        );
        assert!(
            weak.6.is_none_or(|value| value.load().is_none()),
            "标准line未真实释放"
        );
        println!(
            "native_line_metadata case={case:?} stage=released strict_weak_nil=true all_source_reads_counted=true formal=false"
        );
    }

    pub(super) fn run() {
        let mut args = std::env::args().skip(1);
        let case = Case::parse(args.next());
        assert!(args.next().is_none(), "行元数据控制不接受额外参数");
        let mtm = MainThreadMarker::new().expect("行元数据控制必须在真实AppKit主线程执行");
        let application = NSApplication::sharedApplication(mtm);
        application.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        run_case(case, mtm);
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    control::run();
    #[cfg(not(target_os = "macos"))]
    panic!("真实Native行元数据控制只支持macOS，不能在其他平台声明通过");
}
