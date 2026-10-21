//! UTF-16 range arithmetic for the synthetic virtual-value experiment. Translate coordinates
//! only, without reading, copying, or reencoding Source characters.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Utf16Slice {
    pub(super) start: usize,
    pub(super) length: usize,
}

impl Utf16Slice {
    pub(super) fn new(start: usize, length: usize, source_length: usize) -> Option<Self> {
        // Reject overflow and source-bound violations before issuing an actual slice coordinate.
        let end = start.checked_add(length)?;
        (end <= source_length).then_some(Self { start, length })
    }

    pub(super) fn end(self) -> usize {
        self.start + self.length // Actual callers establish this nonoverflowing sum through new/subrange; field visibility alone does not enforce it.
    }

    pub(super) fn subrange(self, location: usize, length: usize) -> Option<Self> {
        let local_end = location.checked_add(length)?;
        if local_end > self.length {
            return None; // Empty ranges beyond EOF are invalid too; only the actual slice endpoint admits an empty substring.
        }
        let start = self.start.checked_add(location)?;
        Self::new(start, length, self.end())
    }

    pub(super) fn index(self, local_index: usize) -> Option<usize> {
        if local_index >= self.length {
            return None; // A readable index differs from a range; an empty EOF substring contains no readable code unit.
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
        // The original global attribute run must contain this exact receiver-local character.
        // Intersect only after coverage is known, then return receiver-local coordinates.
        let global_index = self.index(local_index)?;
        let global_end = global_location.checked_add(global_length)?;
        if global_end > source_length
            || global_location > global_index
            || global_index >= global_end
        {
            return None; // Unknown, empty, or noncovering original runs cannot become valid attribute coverage.
        }
        let start = global_location.max(self.start);
        let end = global_end.min(self.end());
        if start >= end {
            return None; // A zero-length effective range cannot make Foundation stall or impersonate successful coverage.
        }
        Some((start - self.start, end - start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_subranges_preserve_global_offsets_and_real_empty_eof() {
        // Exercise actual production coordinate arithmetic, including nested global offsets and
        // the distinction between an empty EOF range and an invalid EOF character index.
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
        // Invalid arithmetic or missing run coverage must remain None, rather than clamping
        // into a valid-looking coordinate that could authorize another native request.
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
        // NSString slicing permits individual surrogate units. This pure coordinate control
        // reads only its finite fixture, not a native string or a live document.
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
