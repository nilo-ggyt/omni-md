//! 虚拟值的UTF16范围运算；只转坐标，不读取、复制或重新编码源字符。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Utf16Slice {
    pub(super) start: usize,
    pub(super) length: usize,
}

impl Utf16Slice {
    pub(super) fn new(start: usize, length: usize, source_length: usize) -> Option<Self> {
        let end = start.checked_add(length)?;
        (end <= source_length).then_some(Self { start, length })
    }

    pub(super) fn end(self) -> usize {
        self.start + self.length // 只有new/subrange能构造本模块之外的值，已证明总和不溢出。
    }

    pub(super) fn subrange(self, location: usize, length: usize) -> Option<Self> {
        let local_end = location.checked_add(length)?;
        if local_end > self.length {
            return None; // 包括越界空串；只有真实slice末端的空范围允许。
        }
        let start = self.start.checked_add(location)?;
        Self::new(start, length, self.end())
    }

    pub(super) fn index(self, local_index: usize) -> Option<usize> {
        if local_index >= self.length {
            return None; // index与range不同，EOF空范围不能变成一个可读unit。
        }
        self.start.checked_add(local_index)
    }

    pub(super) fn local_effective_range(
        self,
        local_index: usize,
        global_location: usize,
        global_length: usize,
        source_length: usize,
    ) -> Option<(usize, usize)> {
        let global_index = self.index(local_index)?;
        let global_end = global_location.checked_add(global_length)?;
        if global_end > source_length
            || global_location > global_index
            || global_index >= global_end
        {
            return None; // 原属性范围必须真实覆盖本次unit，未知/空run不能变成有效覆盖。
        }
        let start = global_location.max(self.start);
        let end = global_end.min(self.end());
        if start >= end {
            return None; // 不能用零长度effectiveRange让Foundation不前进或假装成功。
        }
        Some((start - self.start, end - start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_subranges_preserve_global_offsets_and_real_empty_eof() {
        let source = Utf16Slice::new(100, 20, 200).expect("真实source范围");
        let nested = source.subrange(5, 8).expect("真实子范围");
        assert_eq!(nested, Utf16Slice::new(105, 8, 200).expect("同全局范围"));
        assert_eq!(nested.subrange(8, 0), Utf16Slice::new(113, 0, 200));
        assert_eq!(nested.subrange(9, 0), None);
        assert_eq!(nested.index(8), None);
        assert_eq!(source.subrange(19, 2), None);
    }

    #[test]
    fn overflow_and_invalid_effective_ranges_never_become_coordinates() {
        assert_eq!(Utf16Slice::new(usize::MAX, 1, usize::MAX), None);
        let source = Utf16Slice::new(10, 10, 30).expect("真实source范围");
        assert_eq!(source.subrange(usize::MAX, 2), None);
        assert_eq!(source.local_effective_range(3, 13, 0, 30), None);
        assert_eq!(source.local_effective_range(3, 14, 1, 30), None);
        assert_eq!(source.local_effective_range(3, 0, usize::MAX, 30), None);
        assert_eq!(source.local_effective_range(3, 0, 30, 30), Some((0, 10)));
        assert_eq!(source.local_effective_range(3, 12, 10, 30), Some((2, 8)));
    }

    #[test]
    fn surrogate_code_units_are_forwarded_without_scalar_reencoding() {
        let units: Vec<u16> = "甲😀乙".encode_utf16().collect();
        let emoji = Utf16Slice::new(1, 2, units.len()).expect("真实emoji源范围");
        let high = emoji
            .subrange(0, 1)
            .expect("允许NSString读取高surrogate unit");
        let low = emoji
            .subrange(1, 1)
            .expect("允许NSString读取低surrogate unit");
        assert_eq!(&units[high.start..high.end()], &[0xd83d]);
        assert_eq!(&units[low.start..low.end()], &[0xde00]);
        assert_eq!(emoji.index(1), Some(low.start));
        assert_eq!(emoji.subrange(2, 0).map(Utf16Slice::end), Some(3));
    }
}
