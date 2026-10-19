//! NSString公开primitive的真实UTF16子范围视图；每次bulk完整转发原字符串并保留原计量。

use super::*;

pub(super) struct VirtualStringSliceIvars {
    binding: Rc<VirtualBinding>,
    source: Retained<NSString>,
    slice: Utf16Slice,
}

define_class!(
    // SAFETY: 实现NSString要求的length和characterAtIndex:，bulk与substring仍遵守原UTF16单位契约。
    // 自定义对象不保存正文，全部字符通过原NSString的真实global索引读取。
    #[unsafe(super = NSString)]
    #[thread_kind = MainThreadOnly]
    #[ivars = VirtualStringSliceIvars]
    pub(super) struct SyntheticVirtualStringSlice;

    // SAFETY: NSObject协议无额外义务；所有源访问及释放沿原主线程图。
    unsafe impl NSObjectProtocol for SyntheticVirtualStringSlice {}

    impl SyntheticVirtualStringSlice {
        #[unsafe(method(length))]
        fn slice_length(&self) -> NSUInteger {
            let _ticket = self.ivars().binding.ticket();
            self.ivars().slice.length // 真实slice长度；不按viewport缩短，也不伪装为原全篇长度。
        }

        #[unsafe(method(characterAtIndex:))]
        fn slice_character(&self, index: NSUInteger) -> unichar {
            let global = self.ivars().slice.index(index).unwrap_or_else(|| throw_virtual_range());
            self.ivars().binding.checked("virtual.string_character", || {
                self.ivars().source.characterAtIndex(global) // 单位原样保留，包括单独surrogate；不转char。
            })
        }

        #[unsafe(method(getCharacters:range:))]
        /// # Safety
        /// destination须在本同步调用期间可写range.length个UTF16单位。
        unsafe fn slice_characters_range(&self, destination: NonNull<unichar>, range: NSRange) {
            let global = self.ivars().slice.subrange(range.location, range.length)
                .unwrap_or_else(|| throw_virtual_range());
            self.ivars().binding.checked("virtual.string_bulk", || {
                // SAFETY: 已证明local/global范围长度相同且在原source内；原primitive完整写入caller缓冲。
                // 直接转发原字符串，原string_read_diagnostic仍计真实global请求，wholebulk不被隐藏/截断。
                unsafe { self.ivars().source.getCharacters_range(destination, NSRange::new(global.start, global.length)) };
            });
        }

        #[unsafe(method(getCharacters:))]
        /// # Safety
        /// destination须在本同步调用期间可写本slice真实length个UTF16单位。
        unsafe fn slice_all_characters(&self, destination: NonNull<unichar>) {
            let slice = self.ivars().slice;
            self.ivars().binding.checked("virtual.string_bulk", || {
                // SAFETY: caller请求整个真实slice；原global范围与该slice长度完全相同，仍完整执行原bulk。
                unsafe { self.ivars().source.getCharacters_range(destination, NSRange::new(slice.start, slice.length)) };
            });
        }

        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn immutable_copy(&self, _zone: *mut NSZone) -> Retained<Self> {
            let count = self.ivars().binding.counts.string_copy_entries.get();
            if count < 4 && super::super::receiver_observation_requested() {
                self.ivars().binding.counts.string_copy_entries.set(count + 1);
                // 首ticket前仅观察真实copy入口；同票全部slice共用四条预算，不添加正文或Native getter。
                let slice = self.ivars().slice;
                eprintln!(
                    "content_storage_probe stage=virtual_string_copy_entry call={} global_location={} global_length={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                    count + 1, slice.start, slice.length,
                );
            }
            self.ivars().binding.checked("virtual.string_copy", || self.retain())
            // copy家族移交这一个+1；只读值不创建完整字符副本，也没有第二次retain。
        }

        #[unsafe(method_id(substringWithRange:))]
        #[unsafe(method_family = none)]
        fn slice_substring(&self, range: NSRange) -> Retained<NSString> {
            self.requested_substring(range)
        }

        #[unsafe(method_id(substringFromIndex:))]
        #[unsafe(method_family = none)]
        fn slice_substring_from(&self, index: NSUInteger) -> Retained<NSString> {
            let length = self.ivars().slice.length.checked_sub(index).unwrap_or_else(|| throw_virtual_range());
            self.requested_substring(NSRange::new(index, length)) // 不借super先复制整个值再裁切。
        }

        #[unsafe(method_id(substringToIndex:))]
        #[unsafe(method_family = none)]
        fn slice_substring_to(&self, index: NSUInteger) -> Retained<NSString> {
            self.requested_substring(NSRange::new(0, index))
        }
    }
);

impl SyntheticVirtualStringSlice {
    pub(super) fn create(
        binding: Rc<VirtualBinding>,
        source: Retained<NSString>,
        slice: Utf16Slice,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSString>, ProviderFailure> {
        let ticket = binding.ticket();
        if Utf16Slice::new(slice.start, slice.length, ticket.key.length) != Some(slice) {
            return Err(ProviderFailure::Range); // 不发布溢出或超原doc范围的虚拟字符串。
        }
        let allocated = ticket.native("virtual.string_alloc", || {
            Self::alloc(mtm).set_ivars(VirtualStringSliceIvars {
                binding: Rc::clone(&binding),
                source,
                slice,
            })
        })?;
        // SAFETY: 必需primitive和ivars已初始化；公开NSString子类只调用父类init，不传完整正文。
        let value: Retained<Self> = ticket.native("virtual.string_init", || unsafe {
            msg_send![super(allocated), init]
        })?;
        if !std::ptr::eq(value.class(), Self::class()) {
            return Err(ProviderFailure::Graph); // 不访问class-cluster意外替代对象的Rust ivars。
        }
        binding.include_owned(&ticket, "editor.virtual_paragraph.string_slice", &value)?;
        Ok(value.into_super())
    }

    fn requested_substring(&self, range: NSRange) -> Retained<NSString> {
        let ticket = self.ivars().binding.ticket();
        let slice = self
            .ivars()
            .slice
            .subrange(range.location, range.length)
            .unwrap_or_else(|| throw_virtual_range());
        self.ivars().binding.log_substring(false, range, slice);
        if slice == self.ivars().slice {
            self.ivars()
                .binding
                .checked("virtual.string_full_substring", || {
                    self.retain().into_super()
                })
        } else {
            let mtm = MainThreadMarker::new().unwrap_or_else(|| throw_virtual_contract());
            let source = ticket
                .native("virtual.string_source_retain", || {
                    self.ivars().source.clone()
                })
                .unwrap_or_else(|_| throw_virtual_contract());
            Self::create(Rc::clone(&self.ivars().binding), source, slice, mtm)
                .unwrap_or_else(|_| throw_virtual_contract()) // 新对象仍指向原source，不串接一串substring owners。
        }
    }
}
