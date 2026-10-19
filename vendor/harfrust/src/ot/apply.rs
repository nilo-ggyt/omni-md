//! Matching of glyph patterns.

use super::Mask;
use super::OtData;
use super::buffer::GlyphInfo;
use super::buffer::{Buffer, GlyphPropsFlags};
use super::cache::Cache;
use super::layout::*;
use super::lookup_flags;
use super::set_digest::SetDigest;
use crate::Scale;
use crate::mapped_pod_vec::ReadSpanWitness;
use crate::ot::{ClassDefInfo, CoverageInfo};
use crate::unicode::GeneralCategory;
use alloc::boxed::Box;
use core::{
    cell::{Cell, RefCell},
    ops::Range,
};
use read_fonts::tables::layout::SequenceLookupRecord;
use read_fonts::types::GlyphId;
use std::rc::Rc;

pub(crate) type MatchPositions = smallvec::SmallVec<[u32; 8]>;

// 每个ApplyContext只有一个纯Rust shape身份；同一上下文中的font/GDEF固定。
// 固定槽只按精确lookup_props复用MaySkipYes，绝不缓存Maybe/coverage/predicate结果。
struct IgnoreProofEntry {
    props: u32,
    witness: ReadSpanWitness,
}
struct IgnoreProofs {
    scope: Rc<()>,
    entries: RefCell<[Option<IgnoreProofEntry>; 8]>,
    next: Cell<usize>,
}
impl IgnoreProofs {
    fn new() -> Self {
        Self {
            scope: Rc::new(()),
            entries: RefCell::new(core::array::from_fn(|_| None)),
            next: Cell::new(0),
        }
    }
    fn range(&self, buffer: &Buffer, props: u32) -> Option<Range<usize>> {
        let entries = self.entries.borrow();
        let entry = entries
            .iter()
            .flatten()
            .find(|entry| entry.props == props)?;
        buffer.out_info_witness_range(&self.scope, &entry.witness)
    }
    fn record(&self, buffer: &Buffer, props: u32, range: Range<usize>) -> bool {
        if range.is_empty() {
            return true;
        }
        let mut entries = self.entries.borrow_mut();
        let old = entries
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.props == props));
        if let Some(index) = old {
            let entry = entries[index].as_ref().unwrap();
            if buffer.extend_out_info_witness(&self.scope, &entry.witness, range.clone()) {
                return true;
            }
        }
        let Some(witness) = buffer.out_info_read_witness(&self.scope, range) else {
            return false;
        };
        let slot = old
            .or_else(|| entries.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                let slot = self.next.get();
                self.next.set((slot + 1) % entries.len());
                slot
            });
        entries[slot] = Some(IgnoreProofEntry { props, witness });
        true
    }
}

/// Value represents glyph id.
pub fn match_glyph(info: &mut GlyphInfo, value: u32) -> bool {
    info.glyph_id == value
}

pub fn match_always(_info: &mut GlyphInfo, _value: u32) -> bool {
    true
}

pub fn match_input(
    ctx: &mut ApplyContext,
    input_len: u16,
    match_func: impl Fn(&mut GlyphInfo, u32) -> bool,
    end_position: &mut usize,
    p_total_component_count: Option<&mut u8>,
) -> bool {
    // This is perhaps the trickiest part of OpenType...  Remarks:
    //
    // - If all components of the ligature were marks, we call this a mark ligature.
    //
    // - If there is no GDEF, and the ligature is NOT a mark ligature, we categorize
    //   it as a ligature glyph.
    //
    // - Ligatures cannot be formed across glyphs attached to different components
    //   of previous ligatures.  Eg. the sequence is LAM,SHADDA,LAM,FATHA,HEH, and
    //   LAM,LAM,HEH form a ligature, leaving SHADDA,FATHA next to eachother.
    //   However, it would be wrong to ligate that SHADDA,FATHA sequence.
    //   There are a couple of exceptions to this:
    //
    //   o If a ligature tries ligating with marks that belong to it itself, go ahead,
    //     assuming that the font designer knows what they are doing (otherwise it can
    //     break Indic stuff when a matra wants to ligate with a conjunct,
    //
    //   o If two marks want to ligate and they belong to different components of the
    //     same ligature glyph, and said ligature glyph is to be ignored according to
    //     mark-filtering rules, then allow.
    //     https://github.com/harfbuzz/harfbuzz/issues/545

    #[derive(PartialEq)]
    enum Ligbase {
        NotChecked,
        MayNotSkip,
        MaySkip,
    }

    let count = usize::from(input_len) + 1;

    if count == 1 {
        *end_position = ctx.buffer.idx + 1;
        ctx.match_positions_len = 1;
        ctx.match_positions[0] = ctx.buffer.idx as u32;
        if let Some(p_total_component_count) = p_total_component_count {
            *p_total_component_count = ctx.buffer.cur(0).lig_num_comps();
        }
        return true;
    }

    if count > MAX_CONTEXT_LENGTH {
        return false;
    }
    ctx.match_positions_len = count;

    let mut iter = SkippingIterator::with_match_fn(ctx, false, Some(match_func));
    iter.reset(iter.buffer.idx);
    iter.set_glyph_data(0);

    let first = *iter.buffer.cur(0);
    let first_lig_id = first.lig_id();
    let first_lig_comp = first.lig_comp();
    let mut total_component_count = 0;
    let mut ligbase = Ligbase::NotChecked;

    for i in 1..count {
        shape_cooperative_abort!();
        let mut unsafe_to = 0;
        if !iter.next(Some(&mut unsafe_to)) {
            *end_position = unsafe_to;
            return false;
        }

        iter.set_match_position(i, iter.index());

        let this = iter.buffer.info[iter.index()];
        let this_lig_id = this.lig_id();
        let this_lig_comp = this.lig_comp();

        if first_lig_id != 0 && first_lig_comp != 0 {
            // If first component was attached to a previous ligature component,
            // all subsequent components should be attached to the same ligature
            // component, otherwise we shouldn't ligate them...
            if first_lig_id != this_lig_id || first_lig_comp != this_lig_comp {
                // ...unless, we are attached to a base ligature and that base
                // ligature is ignorable.
                if ligbase == Ligbase::NotChecked {
                    let mut j = iter.buffer.out_len;
                    let mut found = false;
                    // 后向输出扫描只读同一 scratch 单项，保留忽略基字的原查找次序。
                    while j > 0 && iter.buffer.out_info_at(j - 1).lig_id() == first_lig_id {
                        shape_cooperative_abort!();
                        if iter.buffer.out_info_at(j - 1).lig_comp() == 0 {
                            j -= 1;
                            found = true;
                            break;
                        }
                        j -= 1;
                    }

                    ligbase = if found && iter.may_skip(&iter.buffer.out_info_at(j)) == MaySkip::Yes
                    {
                        Ligbase::MaySkip
                    } else {
                        Ligbase::MayNotSkip
                    };
                }

                if ligbase == Ligbase::MayNotSkip {
                    return false;
                }
            }
        } else {
            // If first component was NOT attached to a previous ligature component,
            // all subsequent components should also NOT be attached to any ligature
            // component, unless they are attached to the first component itself!
            if this_lig_id != 0 && this_lig_comp != 0 && (this_lig_id != first_lig_id) {
                return false;
            }
        }

        total_component_count += this.lig_num_comps_in_ligation();
    }

    *end_position = iter.index() + 1;

    if let Some(p_total_component_count) = p_total_component_count {
        total_component_count += first.lig_num_comps();
        *p_total_component_count = total_component_count;
    }

    ctx.match_positions[0] = iter.buffer.idx as u32;

    true
}

pub fn match_backtrack(
    ctx: &mut ApplyContext,
    backtrack_len: u16,
    match_func: impl Fn(&mut GlyphInfo, u32) -> bool,
    match_start: &mut usize,
) -> bool {
    if backtrack_len == 0 {
        *match_start = ctx.buffer.backtrack_len();
        return true;
    }

    let mut iter = SkippingIterator::with_match_fn(ctx, true, Some(match_func));
    iter.reset_back(iter.buffer.backtrack_len());
    iter.set_glyph_data(0);

    for _ in 0..backtrack_len {
        shape_cooperative_abort!();
        let mut unsafe_from = 0;
        if !iter.prev_witness(Some(&mut unsafe_from)) {
            *match_start = unsafe_from;
            return false;
        }
    }

    *match_start = iter.index();
    true
}

pub fn match_lookahead(
    ctx: &mut ApplyContext,
    lookahead_len: u16,
    match_func: impl Fn(&mut GlyphInfo, u32) -> bool,
    start_index: usize,
    end_index: &mut usize,
) -> bool {
    if lookahead_len == 0 {
        *end_index = start_index;
        return true;
    }

    // Function should always be called with a non-zero starting index
    // c.f. https://github.com/harfbuzz/rustybuzz/issues/142
    debug_assert!(start_index >= 1);
    let mut iter = SkippingIterator::with_match_fn(ctx, true, Some(match_func));
    iter.reset(start_index - 1);
    iter.set_glyph_data(0);

    for _ in 0..lookahead_len {
        shape_cooperative_abort!();
        let mut unsafe_to = 0;
        if !iter.next(Some(&mut unsafe_to)) {
            *end_index = unsafe_to;
            return false;
        }
    }

    *end_index = iter.index() + 1;
    true
}

#[derive(PartialEq, Eq, Copy, Clone)]
pub enum MatchResult {
    Match,
    NotMatch,
    Skip,
}

#[derive(PartialEq, Eq, Copy, Clone)]
enum MayMatch {
    No,
    Yes,
    Maybe,
}

#[derive(PartialEq, Eq, Copy, Clone)]
pub enum MaySkip {
    No,
    Yes,
    Maybe,
}

#[derive(Default)]
pub struct Matcher {
    lookup_props: u32,
    mask: Mask,
    ignore_zwnj: bool,
    ignore_zwj: bool,
    ignore_hidden: bool,
    per_syllable: bool,
}

impl Matcher {
    fn new(ctx: &ApplyContext, context_match: bool) -> Self {
        Matcher {
            lookup_props: ctx.lookup_props,
            // Ignore ZWNJ if we are matching GPOS, or matching GSUB context and asked to.
            ignore_zwnj: ctx.table_index == LayoutTableKind::Gpos
                || (context_match && ctx.auto_zwnj),
            // Ignore ZWJ if we are matching context, or asked to.
            ignore_zwj: context_match || ctx.auto_zwj,
            // Ignore hidden glyphs (like CGJ) during GPOS.
            ignore_hidden: ctx.table_index == LayoutTableKind::Gpos,
            mask: if context_match {
                u32::MAX
            } else {
                ctx.lookup_mask()
            },
            /* Per syllable matching is only for GSUB. */
            per_syllable: ctx.table_index == LayoutTableKind::Gsub && ctx.per_syllable,
        }
    }

    fn may_match(
        &self,
        info: &mut GlyphInfo,
        glyph_data: u32,
        match_func: Option<&impl Fn(&mut GlyphInfo, u32) -> bool>,
        syllable: u8,
    ) -> MayMatch {
        if (info.mask & self.mask) == 0
            || (self.per_syllable && syllable != 0 && syllable != info.syllable())
        {
            return MayMatch::No;
        }

        if let Some(match_func) = match_func {
            return if match_func(info, glyph_data) {
                MayMatch::Yes
            } else {
                MayMatch::No
            };
        }

        MayMatch::Maybe
    }

    #[inline(always)]
    fn may_skip(&self, info: &GlyphInfo, ot: &OtData, lookup_props: u32) -> MaySkip {
        if !check_glyph_property(ot, info, lookup_props) {
            return MaySkip::Yes;
        }

        if info.is_default_ignorable()
            && (self.ignore_zwnj || !info.is_zwnj())
            && (self.ignore_zwj || !info.is_zwj())
            && (self.ignore_hidden || !info.is_hidden())
        {
            return MaySkip::Maybe;
        }

        MaySkip::No
    }
}

#[inline(always)]
fn match_info<F: Fn(&mut GlyphInfo, u32) -> bool>(
    matcher: &Matcher,
    info: &mut GlyphInfo,
    ot: &OtData,
    lookup_props: u32,
    glyph_data: u32,
    match_func: Option<&F>,
    syllable: u8,
) -> MatchResult {
    let skip = matcher.may_skip(info, ot, lookup_props);

    if skip == MaySkip::Yes {
        return MatchResult::Skip;
    }

    let _match = matcher.may_match(info, glyph_data, match_func, syllable);

    if _match == MayMatch::Yes || (_match == MayMatch::Maybe && skip == MaySkip::No) {
        return MatchResult::Match;
    }

    if skip == MaySkip::No {
        return MatchResult::NotMatch;
    }

    MatchResult::Skip
}

// In harfbuzz, skipping iterator works quite differently than it works here. In harfbuzz,
// hb_ot_apply_context contains a skipping iterator that itself contains references to font
// and buffer, meaning that we multiple borrows issue. Due to ownership rules in Rust,
// we cannot copy this approach. Because of this, we basically create a new skipping iterator
// when needed, and we do not have `init` method that exist in harfbuzz. This has a performance
// cost, and makes backporting related changes very hard, but it seems unavoidable, unfortunately.
pub struct SkippingIterator<'f, 'c, F> {
    pub(crate) buffer: &'c mut Buffer,
    ignore_proofs: &'c IgnoreProofs,
    ot: &'c OtData<'f>,
    matcher: &'c Matcher,
    match_positions: &'c mut MatchPositions,
    buf_len: usize,
    glyph_data: u32,
    pub(crate) buf_idx: usize,
    match_func: Option<F>,
    lookup_props: u32,
    syllable: u8,
}

impl<'f, 'c> SkippingIterator<'f, 'c, fn(&mut GlyphInfo, u32) -> bool> {
    pub fn new(ctx: &'c mut ApplyContext<'f>, context_match: bool) -> Self {
        Self::with_match_fn(ctx, context_match, None)
    }
}

impl<'f, 'c, F> SkippingIterator<'f, 'c, F>
where
    F: Fn(&mut GlyphInfo, u32) -> bool,
{
    pub fn with_match_fn(
        ctx: &'c mut ApplyContext<'f>,
        context_match: bool,
        match_fn: Option<F>,
    ) -> Self {
        let matcher = if context_match {
            &ctx.context_matcher
        } else {
            &ctx.matcher
        };
        let buf_len = ctx.buffer.len;
        SkippingIterator {
            buffer: ctx.buffer,
            ignore_proofs: &ctx.ignore_proofs,
            ot: ctx.layout.ot,
            glyph_data: 0,
            buf_len,
            buf_idx: 0,
            matcher,
            match_func: match_fn,
            match_positions: &mut ctx.match_positions,
            lookup_props: matcher.lookup_props,
            syllable: 0,
        }
    }

    pub fn set_match_position(&mut self, idx: usize, position: usize) {
        let count = idx + 1;
        if count > self.match_positions.len() {
            self.match_positions.resize(count, 0);
        }

        self.match_positions[idx] = position as u32;
    }

    pub fn set_glyph_data(&mut self, glyph_data: u32) {
        self.glyph_data = glyph_data;
    }

    fn advance_glyph_data(&mut self) {
        self.glyph_data += 1;
    }

    pub fn set_lookup_props(&mut self, lookup_props: u32) {
        self.lookup_props = lookup_props;
    }

    pub fn index(&self) -> usize {
        self.buf_idx
    }

    #[inline]
    pub fn next(&mut self, unsafe_to: Option<&mut usize>) -> bool {
        let stop = self.buf_len.saturating_sub(1);

        while self.buf_idx < stop {
            shape_cooperative_abort!();
            self.buf_idx += 1;

            match self.match_at(self.buf_idx) {
                MatchResult::Match => {
                    self.advance_glyph_data();
                    return true;
                }
                MatchResult::NotMatch => {
                    if let Some(unsafe_to) = unsafe_to {
                        *unsafe_to = self.buf_idx + 1;
                    }

                    return false;
                }
                MatchResult::Skip => continue,
            }
        }

        if let Some(unsafe_to) = unsafe_to {
            *unsafe_to = self.buf_idx + 1;
        }

        false
    }

    #[inline]
    pub fn prev(&mut self, unsafe_from: Option<&mut usize>) -> bool {
        let stop: usize = 0;
        // 原输出前缀的范围由 buf_idx 递减严格限定；不保留覆盖整前缀的 slice。
        while self.buf_idx > stop {
            shape_cooperative_abort!();
            self.buf_idx -= 1;

            match match_info(
                self.matcher,
                self.buffer.out_info_at_mut(self.buf_idx),
                self.ot,
                self.lookup_props,
                self.glyph_data,
                self.match_func.as_ref(),
                self.syllable,
            ) {
                MatchResult::Match => {
                    self.advance_glyph_data();
                    return true;
                }
                MatchResult::NotMatch => {
                    if let Some(unsafe_from) = unsafe_from {
                        *unsafe_from = self.buf_idx.max(1) - 1;
                    }

                    return false;
                }
                MatchResult::Skip => {
                    continue;
                }
            }
        }

        if let Some(unsafe_from) = unsafe_from {
            *unsafe_from = 0;
        }

        false
    }

    fn prev_witness(&mut self, unsafe_from: Option<&mut usize>) -> bool {
        while self.buf_idx > 0 {
            shape_cooperative_abort!();
            let end = self.buf_idx;
            let known = self.ignore_proofs.range(self.buffer, self.lookup_props);
            if let Some(range) = &known {
                if range.start < end && end <= range.end {
                    // 原谓词在此范围必定不会执行；原owner/shape/props仍同票才允许跳过。
                    self.buf_idx = range.start;
                    continue;
                }
            }
            let mut start = end.saturating_sub(Buffer::out_info_finite_chunk_capacity());
            if let Some(range) = &known {
                // append在已证明范围之后时，只读新尾段，再拼接相邻的真实证明。
                if start < range.end && range.end < end {
                    start = range.end;
                }
            }
            let matcher = self.matcher;
            let ot = self.ot;
            let props = self.lookup_props;
            let Some(boundary) = self.buffer.with_out_info_finite_chunk(start..end, |chunk| {
                for offset in (0..chunk.len()).rev() {
                    shape_cooperative_abort!();
                    // 只缓存真正MaySkipYes；Maybe仍会进入原match_info及其有副作用谓词。
                    if matcher.may_skip(&chunk[offset], ot, props) != MaySkip::Yes {
                        return Some(start + offset);
                    }
                }
                None
            }) else {
                self.buffer.successful = false;
                if let Some(output) = unsafe_from {
                    *output = 0;
                }
                return false;
            };
            shape_cooperative_boundary!(); // closure取消与“完整扫描且无boundary”同为None，先拒绝再建证明。
            let proven_start = boundary.map_or(start, |index| index + 1);
            if proven_start < end
                && !self
                    .ignore_proofs
                    .record(self.buffer, props, proven_start..end)
            {
                self.buffer.successful = false;
                if let Some(output) = unsafe_from {
                    *output = 0;
                }
                return false;
            }
            let Some(index) = boundary else {
                self.buf_idx = start;
                continue;
            };
            self.buf_idx = index;
            // 真正非definite项只授权原单条记录；不得拿整个窗口的&mut撤销刚完成证明。
            match match_info(
                self.matcher,
                self.buffer.out_info_at_mut(index),
                self.ot,
                self.lookup_props,
                self.glyph_data,
                self.match_func.as_ref(),
                self.syllable,
            ) {
                MatchResult::Match => {
                    self.advance_glyph_data();
                    return true;
                }
                MatchResult::NotMatch => {
                    if let Some(output) = unsafe_from {
                        *output = self.buf_idx.max(1) - 1;
                    }
                    return false;
                }
                MatchResult::Skip => { /* Maybe未匹配仍逐项继续，绝不把这个位置并入证明。 */
                }
            }
        }
        if let Some(output) = unsafe_from {
            *output = 0;
        }
        false
    }

    pub fn reset(&mut self, start_index: usize) {
        // For GSUB forward iterator
        self.buf_idx = start_index;
        self.buf_len = self.buffer.len;
        self.syllable = self.buffer.cur(0).syllable();
    }

    pub fn reset_back(&mut self, start_index: usize) {
        // For GSUB backward iterator
        self.buf_idx = start_index;
        self.syllable = self.buffer.cur(0).syllable();
    }

    pub fn reset_fast(&mut self, start_index: usize) {
        // Doesn't set end or syllable. Used by GPOS which doesn't care / change.
        self.buf_idx = start_index;
    }

    pub fn may_skip(&self, info: &GlyphInfo) -> MaySkip {
        self.matcher.may_skip(info, self.ot, self.lookup_props)
    }

    #[inline]
    pub fn match_at(&mut self, idx: usize) -> MatchResult {
        let info = &mut self.buffer.info[idx];
        match_info(
            self.matcher,
            info,
            self.ot,
            self.lookup_props,
            self.glyph_data,
            self.match_func.as_ref(),
            self.syllable,
        )
    }
}

pub(crate) fn apply_lookup(
    ctx: &mut ApplyContext,
    input_len: usize,
    match_end: usize,
    lookups: &[SequenceLookupRecord],
) {
    let mut count = input_len + 1;

    debug_assert!(count <= ctx.match_positions.len(), "");

    // All positions are distance from beginning of *output* buffer.
    // Adjust.
    let mut end: isize = {
        let backtrack_len = ctx.buffer.backtrack_len();
        let delta = backtrack_len as isize - ctx.buffer.idx as isize;

        // Convert positions to new indexing.
        for j in 0..count {
            shape_cooperative_abort!();
            ctx.match_positions[j] = (ctx.match_positions[j] as isize + delta) as _;
        }

        backtrack_len as isize + match_end as isize - ctx.buffer.idx as isize
    };

    for record in lookups {
        shape_cooperative_abort!();
        if !ctx.buffer.successful {
            break;
        }

        let idx = usize::from(record.sequence_index.get());
        if idx >= count {
            continue;
        }

        let orig_len = ctx.buffer.backtrack_len() + ctx.buffer.lookahead_len();

        // This can happen if earlier recursed lookups deleted many entries.
        if ctx.match_positions[idx] as usize >= orig_len {
            continue;
        }

        if !ctx.buffer.move_to(ctx.match_positions[idx] as usize) {
            break;
        }

        if ctx.buffer.max_ops <= 0 {
            break;
        }

        if ctx.recurse(record.lookup_list_index.get()).is_none() {
            continue;
        }

        let new_len = ctx.buffer.backtrack_len() + ctx.buffer.lookahead_len();
        let mut delta = new_len as isize - orig_len as isize;
        if delta == 0 {
            continue;
        }

        // Recursed lookup changed buffer len.  Adjust.
        //
        // TODO:
        //
        // Right now, if buffer length increased by n, we assume n new glyphs
        // were added right after the current position, and if buffer length
        // was decreased by n, we assume n match positions after the current
        // one where removed.  The former (buffer length increased) case is
        // fine, but the decrease case can be improved in at least two ways,
        // both of which are significant:
        //
        //   - If recursed-to lookup is MultipleSubst and buffer length
        //     decreased, then it's current match position that was deleted,
        //     NOT the one after it.
        //
        //   - If buffer length was decreased by n, it does not necessarily
        //     mean that n match positions where removed, as there recursed-to
        //     lookup might had a different LookupFlag.  Here's a constructed
        //     case of that:
        //     https://github.com/harfbuzz/harfbuzz/discussions/3538
        //
        // It should be possible to construct tests for both of these cases.

        end += delta;
        if end < ctx.match_positions[idx] as isize {
            // End might end up being smaller than match_positions[idx] if the recursed
            // lookup ended up removing many items.
            // Just never rewind end beyond start of current position, since that is
            // not possible in the recursed lookup.  Also adjust delta as such.
            //
            // https://bugs.chromium.org/p/chromium/issues/detail?id=659496
            // https://github.com/harfbuzz/harfbuzz/issues/1611
            //
            delta += ctx.match_positions[idx] as isize - end;
            end = ctx.match_positions[idx] as isize;
        }

        // next now is the position after the recursed lookup.
        let mut next = idx + 1;

        if delta > 0 {
            if delta as usize + count > MAX_CONTEXT_LENGTH {
                break;
            }

            if delta as usize + count > ctx.match_positions.len() {
                ctx.match_positions.resize(delta as usize + count, 0);
            }
        } else {
            // NOTE: delta is non-positive.
            delta = delta.max(next as isize - count as isize);
            next = (next as isize - delta) as _;
        }

        // Shift!
        ctx.match_positions
            .copy_within(next..count, (next as isize + delta) as _);
        next = (next as isize + delta) as _;
        count = (count as isize + delta) as _;
        ctx.match_positions_len = count;

        // Fill in new entries.
        for j in idx + 1..next {
            shape_cooperative_abort!();
            ctx.match_positions[j] = ctx.match_positions[j - 1] + 1;
        }

        // And fixup the rest.
        while next < count {
            shape_cooperative_abort!();
            ctx.match_positions[next] = (ctx.match_positions[next] as isize + delta) as _;
            next += 1;
        }
    }

    ctx.buffer.move_to(end.try_into().unwrap());
}

/// Find out whether a lookup would be applied.
pub trait WouldApply {
    /// Whether the lookup would be applied.
    fn would_apply(&self, ctx: &WouldApplyContext) -> bool;
}

// HB uses a cache size of 128 here; we double it to reduce collisions
// since our lookup is slower.
pub(crate) type MappingCache = Cache<
    16,  // KEY_BITS
    8,   // VALUE_BITS
    256, // CACHE_SIZE
    16,  // STORAGE_BITS
>;

pub(crate) type BinaryCache = Cache<
    15,  // KEY_BITS
    1,   // VALUE_BITS
    256, // CACHE_SIZE
    8,   // STORAGE_BITS
>;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum SubtableExternalCacheMode {
    #[allow(unused)]
    None,
    Small,
    Full,
}

pub(crate) struct LigatureSubstFormat1Cache {
    pub seconds: SetDigest,
    pub coverage: MappingCache,
}

impl LigatureSubstFormat1Cache {
    pub fn new(seconds: SetDigest) -> Self {
        LigatureSubstFormat1Cache {
            coverage: MappingCache::new(),
            seconds,
        }
    }
}

pub(crate) struct LigatureSubstFormat1SmallCache {
    pub coverage: CoverageInfo,
    pub seconds: SetDigest,
}

pub(crate) struct PairPosFormat1Cache {
    pub coverage: MappingCache,
    pub pair_sets: Box<[SetDigest]>,
}

impl PairPosFormat1Cache {
    pub fn new(pair_sets: Box<[SetDigest]>) -> Self {
        PairPosFormat1Cache {
            coverage: MappingCache::new(),
            pair_sets,
        }
    }
}

pub(crate) struct PairPosFormat1SmallCache {
    pub coverage: CoverageInfo,
    pub pair_sets: Box<[SetDigest]>,
}

pub(crate) struct PairPosFormat2Cache {
    pub coverage: MappingCache,
    pub first: MappingCache,
    pub second: MappingCache,
}

impl PairPosFormat2Cache {
    pub fn new() -> Self {
        PairPosFormat2Cache {
            coverage: MappingCache::new(),
            first: MappingCache::new(),
            second: MappingCache::new(),
        }
    }
}

pub(crate) struct PairPosFormat2SmallCache {
    pub coverage: CoverageInfo,
    pub first: ClassDefInfo,
    pub second: ClassDefInfo,
}

pub(crate) struct ContextFormat2Cache {
    pub coverage: CoverageInfo,
    pub input: ClassDefInfo,
    pub coverage_cache: BinaryCache,
    pub rule_sets: Box<[RuleSetDigest]>,
}

pub(crate) struct ChainContextFormat2Cache {
    pub coverage: CoverageInfo,
    pub backtrack: ClassDefInfo,
    pub input: ClassDefInfo,
    pub lookahead: ClassDefInfo,
    pub coverage_cache: BinaryCache,
    pub class_caches: Option<Box<ChainContextClassCaches>>,
    pub rule_sets: Box<[RuleSetDigest]>,
}

pub(crate) struct ChainContextClassCaches {
    pub input: MappingCache,
    pub lookahead: MappingCache,
}

#[derive(Copy, Clone, Default)]
// One word per rule set; collisions only admit extra work.
pub(crate) struct RuleSetDigest(u64);

impl RuleSetDigest {
    pub fn full() -> Self {
        Self(u64::MAX)
    }

    pub fn add(&mut self, value: u16) {
        self.0 |= 1 << (value & 63);
    }

    pub fn is_full(self) -> bool {
        self.0 == u64::MAX
    }

    pub fn may_have(self, value: u16) -> bool {
        self.0 & (1 << (value & 63)) != 0
    }
}

pub(crate) enum SubtableExternalCache {
    None,
    LigatureSubstFormat1Cache(Box<LigatureSubstFormat1Cache>),
    LigatureSubstFormat1SmallCache(LigatureSubstFormat1SmallCache),
    PairPosFormat1Cache(Box<PairPosFormat1Cache>),
    PairPosFormat1SmallCache(PairPosFormat1SmallCache),
    PairPosFormat2Cache(Box<PairPosFormat2Cache>),
    PairPosFormat2SmallCache(PairPosFormat2SmallCache),
    ContextFormat2Cache(Box<ContextFormat2Cache>),
    ChainContextFormat2Cache(Box<ChainContextFormat2Cache>),
}

/// Apply a lookup.
pub trait Apply {
    fn apply(&self, ctx: &mut ApplyContext) -> Option<()> {
        // Default implementation just calls `apply_with_external_cache`.
        self.apply_with_external_cache(ctx, &SubtableExternalCache::None)
    }

    // The rest are relevant to subtables only

    fn apply_with_external_cache(
        &self,
        ctx: &mut ApplyContext,
        _external_cache: &SubtableExternalCache,
    ) -> Option<()> {
        // Default implementation just calls `apply`.
        self.apply(ctx)
    }

    fn apply_cached(
        &self,
        ctx: &mut ApplyContext,
        external_cache: &SubtableExternalCache,
    ) -> Option<()> {
        // Default implementation just calls `apply_with_external_cache`.
        // This is used to apply the lookup with glyph-info caching.
        self.apply_with_external_cache(ctx, external_cache)
    }

    fn cache_cost(&self) -> u32 {
        // Default implementation returns 0, meaning no cache cost.
        // This is used to determine the cost of caching the subtable.
        0
    }

    fn external_cache_create(&self, mode: SubtableExternalCacheMode) -> SubtableExternalCache {
        // Default implementation returns None, meaning no external cache.
        // This is used to create an external cache for the subtable.
        let _ = mode;
        SubtableExternalCache::None
    }
}

pub struct WouldApplyContext<'a> {
    pub glyphs: &'a [GlyphId],
    pub zero_context: bool,
}

fn match_properties_mark(
    ot: &OtData,
    info: &GlyphInfo,
    glyph_props: u16,
    match_props: u32,
) -> bool {
    // If using mark filtering sets, the high short of
    // match_props has the set index.
    if match_props as u16 & lookup_flags::USE_MARK_FILTERING_SET != 0 {
        let set_index = (match_props >> 16) as u16;
        return ot.is_mark_glyph(info.as_glyph().to_u32(), set_index);
    }

    // The second byte of match_props has the meaning
    // "ignore marks of attachment type different than
    // the attachment type specified."
    if match_props as u16 & lookup_flags::MARK_ATTACHMENT_TYPE_MASK != 0 {
        return (match_props as u16 & lookup_flags::MARK_ATTACHMENT_TYPE_MASK)
            == (glyph_props & lookup_flags::MARK_ATTACHMENT_TYPE_MASK);
    }

    true
}

#[inline(always)]
pub fn check_glyph_property(ot: &OtData, info: &GlyphInfo, match_props: u32) -> bool {
    let glyph_props = info.glyph_props();

    // Not covered, if, for example, glyph class is ligature and
    // match_props includes LookupFlags::IgnoreLigatures
    if glyph_props & match_props as u16 & lookup_flags::IGNORE_FLAGS != 0 {
        return false;
    }

    if glyph_props & GlyphPropsFlags::MARK.bits() != 0 {
        return match_properties_mark(ot, info, glyph_props, match_props);
    }

    true
}

pub struct ApplyContext<'a> {
    pub table_index: LayoutTableKind,
    pub layout: crate::LayoutData<'a>,
    pub scale: Scale,
    pub buffer: &'a mut Buffer,
    lookup_mask: Mask,
    pub per_syllable: bool,
    pub lookup_index: u16,
    pub lookup_props: u32,
    pub nesting_level_left: usize,
    pub auto_zwnj: bool,
    pub auto_zwj: bool,
    pub random: bool,
    pub random_state: u32,
    pub new_syllables: Option<u8>,
    pub last_base: i32,
    pub last_base_until: u32,
    pub(crate) matcher: Matcher,
    pub(crate) context_matcher: Matcher,
    ignore_proofs: IgnoreProofs,
    pub(crate) match_positions_len: usize,
    pub(crate) match_positions: MatchPositions,
}

impl<'a> ApplyContext<'a> {
    pub fn new(
        table_index: LayoutTableKind,
        layout: crate::LayoutData<'a>,
        scale: Scale,
        buffer: &'a mut Buffer,
    ) -> Self {
        Self {
            table_index,
            layout,
            scale,
            buffer,
            lookup_mask: 1,
            per_syllable: false,
            lookup_index: u16::MAX,
            lookup_props: u32::MAX,
            nesting_level_left: MAX_NESTING_LEVEL,
            auto_zwnj: true,
            auto_zwj: true,
            random: false,
            random_state: 1,
            new_syllables: None,
            last_base: -1,
            last_base_until: 0,
            matcher: Matcher::default(),
            context_matcher: Matcher::default(),
            ignore_proofs: IgnoreProofs::new(),
            match_positions_len: 0,
            match_positions: MatchPositions::from_elem(0, 1),
        }
    }

    #[inline(always)]
    pub fn scale_x(&self, value: f32) -> i32 {
        self.scale.scale_x_f(value)
    }

    #[inline(always)]
    pub fn scale_y(&self, value: f32) -> i32 {
        self.scale.scale_y_f(value)
    }

    pub fn random_number(&mut self) -> u32 {
        // http://www.cplusplus.com/reference/random/minstd_rand/
        self.random_state = self.random_state.wrapping_mul(48271) % (i32::MAX as u32);
        self.random_state
    }

    pub fn set_lookup_mask(&mut self, mask: Mask) {
        self.lookup_mask = mask;
        self.last_base = -1;
        self.last_base_until = 0;
    }

    pub fn lookup_mask(&self) -> Mask {
        self.lookup_mask
    }

    pub fn update_matchers(&mut self) {
        self.matcher = Matcher::new(self, false);
        self.context_matcher = Matcher::new(self, true);
    }

    pub fn recurse(&mut self, sub_lookup_index: u16) -> Option<()> {
        if self.nesting_level_left == 0 {
            self.buffer.successful = false;
            return None;
        }

        self.buffer.max_ops -= 1;
        if self.buffer.max_ops < 0 {
            self.buffer.successful = false;
            return None;
        }

        self.nesting_level_left -= 1;
        let saved_props = self.lookup_props;
        let saved_index = self.lookup_index;

        // The nested lookup never reads our match positions, so save by
        // moving them out rather than cloning. The replacement must keep
        // the len >= 1 invariant (match_input's count == 1 fast path
        // writes match_positions[0] without resizing); it stays inline in
        // the SmallVec, so no allocation happens here.
        let saved_match_positions =
            core::mem::replace(&mut self.match_positions, MatchPositions::from_elem(0, 1));
        let saved_match_positions_len = self.match_positions_len;

        self.lookup_index = sub_lookup_index;
        let applied = self
            .layout
            .ot
            .table_data_and_lookup(self.table_index, sub_lookup_index)
            .and_then(|(table_data, lookup)| {
                self.lookup_props = lookup.props();
                self.update_matchers();
                lookup.apply(self, table_data, false)
            });
        self.lookup_props = saved_props;
        self.lookup_index = saved_index;
        self.update_matchers();
        self.match_positions = saved_match_positions;
        self.match_positions_len = saved_match_positions_len;
        self.nesting_level_left += 1;
        shape_cooperative_boundary!(); // 完成原递归状态恢复后才传播，不能留下半层context。
        applied
    }

    fn set_glyph_class(
        &mut self,
        glyph_id: GlyphId,
        class_guess: GlyphPropsFlags,
        ligature: bool,
        component: bool,
    ) {
        self.buffer.digest.add(glyph_id.into());

        if let Some(syllable) = self.new_syllables {
            self.buffer.cur_mut(0).set_syllable(syllable);
        }

        let cur = self.buffer.cur_mut(0);
        let mut props = cur.glyph_props();

        props |= GlyphPropsFlags::SUBSTITUTED.bits();

        if ligature {
            props |= GlyphPropsFlags::LIGATED.bits();
            // In the only place that the MULTIPLIED bit is used, Uniscribe
            // seems to only care about the "last" transformation between
            // Ligature and Multiple substitutions.  Ie. if you ligate, expand,
            // and ligate again, it forgives the multiplication and acts as
            // if only ligation happened.  As such, clear MULTIPLIED bit.
            props &= !GlyphPropsFlags::MULTIPLIED.bits();
        }

        if component {
            props |= GlyphPropsFlags::MULTIPLIED.bits();
        }

        let has_glyph_classes = self.layout.ot.has_glyph_classes();

        if has_glyph_classes {
            props &= GlyphPropsFlags::PRESERVE.bits();
            cur.set_glyph_props(props | self.layout.ot.glyph_props(glyph_id));
        } else if !class_guess.is_empty() {
            props &= GlyphPropsFlags::PRESERVE.bits();
            cur.set_glyph_props(props | class_guess.bits());
        } else {
            cur.set_glyph_props(props);
        }
    }

    pub fn replace_glyph(&mut self, glyph_id: GlyphId) {
        self.set_glyph_class(glyph_id, GlyphPropsFlags::empty(), false, false);
        self.buffer.replace_glyph(u32::from(glyph_id));
    }

    pub fn replace_glyph_inplace(&mut self, glyph_id: GlyphId) {
        self.set_glyph_class(glyph_id, GlyphPropsFlags::empty(), false, false);
        self.buffer.cur_mut(0).glyph_id = u32::from(glyph_id);
    }

    pub fn replace_glyph_with_ligature(&mut self, glyph_id: GlyphId, class_guess: GlyphPropsFlags) {
        self.set_glyph_class(glyph_id, class_guess, true, false);
        self.buffer.replace_glyph(u32::from(glyph_id));
    }

    pub fn output_glyph_for_component(&mut self, glyph_id: GlyphId, class_guess: GlyphPropsFlags) {
        self.set_glyph_class(glyph_id, class_guess, false, true);
        self.buffer.output_glyph(u32::from(glyph_id));
    }
}

pub fn ligate_input(
    ctx: &mut ApplyContext,
    // Including the first glyph
    count: usize,
    // Including the first glyph
    match_end: usize,
    total_component_count: u8,
    lig_glyph: GlyphId,
) {
    // - If a base and one or more marks ligate, consider that as a base, NOT
    //   ligature, such that all following marks can still attach to it.
    //   https://github.com/harfbuzz/harfbuzz/issues/1109
    //
    // - If all components of the ligature were marks, we call this a mark ligature.
    //   If it *is* a mark ligature, we don't allocate a new ligature id, and leave
    //   the ligature to keep its old ligature id.  This will allow it to attach to
    //   a base ligature in GPOS.  Eg. if the sequence is: LAM,LAM,SHADDA,FATHA,HEH,
    //   and LAM,LAM,HEH for a ligature, they will leave SHADDA and FATHA with a
    //   ligature id and component value of 2.  Then if SHADDA,FATHA form a ligature
    //   later, we don't want them to lose their ligature id/component, otherwise
    //   GPOS will fail to correctly position the mark ligature on top of the
    //   LAM,LAM,HEH ligature.  See:
    //     https://bugzilla.gnome.org/show_bug.cgi?id=676343
    //
    // - If a ligature is formed of components that some of which are also ligatures
    //   themselves, and those ligature components had marks attached to *their*
    //   components, we have to attach the marks to the new ligature component
    //   positions!  Now *that*'s tricky!  And these marks may be following the
    //   last component of the whole sequence, so we should loop forward looking
    //   for them and update them.
    //
    //   Eg. the sequence is LAM,LAM,SHADDA,FATHA,HEH, and the font first forms a
    //   'calt' ligature of LAM,HEH, leaving the SHADDA and FATHA with a ligature
    //   id and component == 1.  Now, during 'liga', the LAM and the LAM-HEH ligature
    //   form a LAM-LAM-HEH ligature.  We need to reassign the SHADDA and FATHA to
    //   the new ligature with a component value of 2.
    //
    //   This in fact happened to a font...  See:
    //   https://bugzilla.gnome.org/show_bug.cgi?id=437633
    //

    ctx.buffer.merge_clusters(ctx.buffer.idx, match_end);

    let mut is_base_ligature = ctx.buffer.info[ctx.match_positions[0] as usize].is_base_glyph();
    let mut is_mark_ligature = ctx.buffer.info[ctx.match_positions[0] as usize].is_mark();
    for i in 1..count {
        shape_cooperative_abort!();
        if !ctx.buffer.info[ctx.match_positions[i] as usize].is_mark() {
            is_base_ligature = false;
            is_mark_ligature = false;
        }
    }

    let is_ligature = !is_base_ligature && !is_mark_ligature;
    let class = if is_ligature {
        GlyphPropsFlags::LIGATURE
    } else {
        GlyphPropsFlags::empty()
    };
    let lig_id = if is_ligature {
        ctx.buffer.allocate_lig_id()
    } else {
        0
    };
    let first = ctx.buffer.cur_mut(0);
    let mut last_lig_id = first.lig_id();
    let mut last_num_comps = first.lig_num_comps();
    let mut comps_so_far = last_num_comps;

    if is_ligature {
        first.set_lig_props_for_ligature(lig_id, total_component_count);
        if first.general_category() == GeneralCategory::NON_SPACING_MARK {
            first.set_general_category(GeneralCategory::OTHER_LETTER);
        }
    }

    ctx.replace_glyph_with_ligature(lig_glyph, class);

    for i in 1..count {
        shape_cooperative_abort!();
        while ctx.buffer.idx < ctx.match_positions[i] as usize && ctx.buffer.successful {
            shape_cooperative_abort!();
            if is_ligature {
                let cur = ctx.buffer.cur_mut(0);
                let mut this_comp = cur.lig_comp();
                if this_comp == 0 {
                    this_comp = last_num_comps;
                }
                // Avoid the potential for a wrap-around bug when subtracting from an unsigned integer
                // c.f. https://github.com/harfbuzz/rustybuzz/issues/142
                debug_assert!(comps_so_far >= last_num_comps);
                let new_lig_comp = comps_so_far - last_num_comps + this_comp.min(last_num_comps);
                cur.set_lig_props_for_mark(lig_id, new_lig_comp);
            }
            ctx.buffer.next_glyph();
        }

        let cur = ctx.buffer.cur(0);
        last_lig_id = cur.lig_id();
        last_num_comps = cur.lig_num_comps_in_ligation();
        comps_so_far += last_num_comps;

        // Skip the base glyph.
        ctx.buffer.idx += 1;
    }

    if !is_mark_ligature && last_lig_id != 0 {
        // Re-adjust components for any marks following.
        for i in ctx.buffer.idx..ctx.buffer.len {
            shape_cooperative_abort!();
            let info = &mut ctx.buffer.info[i];
            if last_lig_id != info.lig_id() {
                break;
            }

            let this_comp = info.lig_comp();
            if this_comp == 0 {
                break;
            }

            // Avoid the potential for a wrap-around bug when subtracting from an unsigned integer
            // c.f. https://github.com/harfbuzz/rustybuzz/issues/142
            debug_assert!(comps_so_far >= last_num_comps);
            let new_lig_comp = comps_so_far - last_num_comps + this_comp.min(last_num_comps);
            info.set_lig_props_for_mark(lig_id, new_lig_comp);
        }
    }
}

/// 仅离线 contracts 入口：对照原 prev，检查有限窗口的逐项顺序、停止位置和谓词写入。
#[doc(hidden)]
#[cfg(test)]
pub fn paged_backtrack_cursor_contract() -> bool {
    let capacity = Buffer::out_info_finite_chunk_capacity();
    for count in [
        0,
        1,
        2,
        capacity - 1,
        capacity,
        capacity + 1,
        capacity * 2 + 1,
    ] {
        for case in 0..5 {
            for separate in [false, true] {
                for per_syllable in [false, true] {
                    let original =
                        backtrack_cursor_contract_run(count, case, separate, per_syllable, false);
                    let finite =
                        backtrack_cursor_contract_run(count, case, separate, per_syllable, true);
                    if original.is_none() || original != finite {
                        eprintln!(
                            "paged_backtrack_cursor_contract mismatch count={count} case={case} separate={separate} per_syllable={per_syllable}"
                        );
                        return false;
                    }
                }
            }
        }
    }
    // 独立的跨调用序列保留同一个真实ApplyContext缓存；四方向都与未优化prev逐步比。
    // 这里重现recurse的保存/切换/还原协议，不把手工状态转换声称为字体表驱动递归。
    for direction in [
        crate::Direction::LeftToRight,
        crate::Direction::RightToLeft,
        crate::Direction::TopToBottom,
        crate::Direction::BottomToTop,
    ] {
        for separate in [false, true] {
            let original = backtrack_witness_sequence_run(separate, direction, false);
            let paged = backtrack_witness_sequence_run(separate, direction, true);
            if original.is_none() || original != paged {
                eprintln!(
                    "paged_backtrack_witness_sequence mismatch separate={separate} direction={direction:?}"
                );
                return false;
            }
        }
    }
    true
}

// 该控制仅生成有限非用户记录。outcomes 最多四项，hash 逐条计算，不构造结果字形镜像。
#[cfg(test)]
fn backtrack_cursor_contract_run(
    count: usize,
    case: usize,
    separate: bool,
    per_syllable: bool,
    finite: bool,
) -> Option<(Vec<(bool, usize, u32, usize)>, usize, u64, u64)> {
    use core::cell::Cell;
    let mut buffer = Buffer::new();
    for index in 0..=count {
        let codepoint = if index == count || index == 0 {
            0x41
        } else if case == 0 {
            0x64e
        } else {
            [0x64e, 0x200d, 0x200c, 0x34f, 0x41, 0x42, 0x301][index % 7]
        };
        buffer.push(codepoint, index as u32);
        if !buffer.allocation_successful() {
            return None;
        }
        let info = &mut buffer.info[index];
        info.mask = if case == 2 && index % 11 == 0 { 0 } else { 1 };
        info.init_unicode_props(&mut buffer.scratch_flags);
        info.set_glyph_props(if matches!(codepoint, 0x64e | 0x301) {
            GlyphPropsFlags::MARK.bits()
        } else {
            GlyphPropsFlags::BASE_GLYPH.bits()
        });
        info.set_syllable(if case == 3 && index % 5 == 0 { 7 } else { 3 });
    }
    if separate {
        if !buffer
            .pos
            .prepare_resize(count + 1, crate::GlyphPosition::default())
        {
            return None;
        }
        buffer.pos.commit_len(count + 1);
        for index in 0..=count {
            buffer.pos[index] = bytemuck::cast(buffer.info[index]);
        }
    }
    buffer.have_output = true;
    buffer.have_separate_output = separate;
    buffer.out_len = count;
    buffer.idx = count;
    let calls = Cell::new(0usize);
    let predicate_hash = Cell::new(0u64);
    let mut outcomes = Vec::new();
    {
        let layout = crate::LayoutData {
            ot: &crate::ot::EMPTY_OT_DATA,
            aat: &crate::aat::EMPTY_AAT_DATA,
            units_per_em: 1000,
            apply_trak: false,
        };
        let mut context =
            ApplyContext::new(LayoutTableKind::Gsub, layout, Scale::default(), &mut buffer);
        context.lookup_props = if case == 4 {
            0
        } else {
            u32::from(lookup_flags::IGNORE_MARKS)
        };
        context.auto_zwnj = case != 1;
        context.per_syllable = per_syllable;
        context.update_matchers();
        let predicate = |info: &mut GlyphInfo, data: u32| {
            calls.set(calls.get() + 1);
            predicate_hash.set(
                predicate_hash
                    .get()
                    .wrapping_mul(131)
                    .wrapping_add(u64::from(info.cluster))
                    .wrapping_add(u64::from(data) << 32),
            );
            // 实际更改原记录，以发现错误的重排、预执行或重复 predicate。
            info.set_syllable(info.syllable().wrapping_add(1));
            info.mask ^= 0x100;
            matches!(info.glyph_id, 0x41 | 0x200d) || data % 2 == 1
        };
        let mut cursor = SkippingIterator::with_match_fn(&mut context, true, Some(predicate));
        cursor.reset_back(count);
        cursor.set_glyph_data(0);
        for _ in 0..4 {
            let mut unsafe_from = usize::MAX;
            let found = if finite {
                cursor.prev_witness(Some(&mut unsafe_from))
            } else {
                cursor.prev(Some(&mut unsafe_from))
            };
            outcomes.push((found, cursor.index(), cursor.glyph_data, unsafe_from));
            if !found {
                break;
            }
        }
    }
    if !buffer.allocation_successful() {
        return None;
    }
    let mut glyph_hash = 0u64;
    for index in 0..count {
        let info = buffer.out_info_at(index);
        for byte in bytemuck::bytes_of(&info) {
            glyph_hash = glyph_hash.wrapping_mul(257).wrapping_add(u64::from(*byte));
        }
    }
    Some((outcomes, calls.get(), predicate_hash.get(), glyph_hash))
}

// 每个阶段只留固定大小的停止收据；没有用户文本或整段glyph结果镜像。
#[cfg(test)]
type BacktrackSequenceReceipt = (Vec<(bool, usize, u32, usize)>, usize, u64, u64);

#[cfg(test)]
fn sequence_cursor_visit(
    context: &mut ApplyContext,
    calls: &Cell<usize>,
    predicate_hash: &Cell<u64>,
    use_witness: bool,
    outcomes: &mut Vec<(bool, usize, u32, usize)>,
) {
    let end = context.buffer.out_len;
    let data = u32::from(context.lookup_index);
    let predicate = |info: &mut GlyphInfo, data: u32| {
        calls.set(calls.get() + 1);
        predicate_hash.set(
            predicate_hash
                .get()
                .wrapping_mul(131)
                .wrapping_add(u64::from(info.cluster))
                .wrapping_add(u64::from(data) << 32),
        );
        // Maybe/default-ignorable每次仍须运行这个真实写入，不能因上次为Skip就跳过。
        info.mask ^= 0x100;
        info.set_syllable(info.syllable().wrapping_add(1));
        info.glyph_id == 0x41
    };
    let mut cursor = SkippingIterator::with_match_fn(context, true, Some(predicate));
    cursor.reset_back(end);
    cursor.set_glyph_data(data);
    let mut unsafe_from = usize::MAX;
    let found = if use_witness {
        cursor.prev_witness(Some(&mut unsafe_from))
    } else {
        cursor.prev(Some(&mut unsafe_from))
    };
    outcomes.push((found, cursor.index(), cursor.glyph_data, unsafe_from));
}

#[cfg(test)]
fn sequence_glyph(buffer: &mut Buffer, index: usize, codepoint: u32) -> GlyphInfo {
    let mut value = GlyphInfo {
        glyph_id: codepoint,
        cluster: index as u32,
        mask: 1,
        ..GlyphInfo::default()
    };
    value.init_unicode_props(&mut buffer.scratch_flags);
    value.set_glyph_props(if codepoint == 0x64e {
        GlyphPropsFlags::MARK.bits()
    } else {
        GlyphPropsFlags::BASE_GLYPH.bits()
    });
    value.set_syllable(3);
    value
}

#[cfg(test)]
fn sequence_layout() -> crate::LayoutData<'static> {
    crate::LayoutData {
        ot: &crate::ot::EMPTY_OT_DATA,
        aat: &crate::aat::EMPTY_AAT_DATA,
        units_per_em: 1000,
        apply_trak: false,
    }
}

#[cfg(test)]
fn backtrack_witness_sequence_run(
    separate: bool,
    direction: crate::Direction,
    use_witness: bool,
) -> Option<BacktrackSequenceReceipt> {
    let count = Buffer::out_info_finite_chunk_capacity() * 2 + 1;
    let mut buffer = Buffer::new();
    // 先真实预留一点尾空间；后续append必须不经过remap，才能证实范围外写入可复用。
    if !buffer.ensure(count + 64) {
        return None;
    }
    for index in 0..=count {
        buffer.info[index] = sequence_glyph(
            &mut buffer,
            index,
            if index == 0 || index == count {
                0x41
            } else {
                0x64e
            },
        );
    }
    buffer.len = count + 1;
    buffer.direction = direction;
    buffer.have_output = true;
    buffer.out_len = count;
    buffer.idx = count;
    if separate {
        for index in 0..=count {
            buffer.pos[index] = bytemuck::cast(buffer.info[index]);
        }
    }
    buffer.have_separate_output = separate;
    let calls = Cell::new(0);
    let predicate_hash = Cell::new(0);
    let mut outcomes = Vec::with_capacity(24);
    let mut context = ApplyContext::new(
        LayoutTableKind::Gsub,
        sequence_layout(),
        Scale::default(),
        &mut buffer,
    );
    context.lookup_props = u32::from(lookup_flags::IGNORE_MARKS);
    context.lookup_index = 1;
    context.update_matchers();
    // 冷请求和同票热请求必须分别与原prev相同，第二次并未重建scope或backing。
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    if use_witness
        && context
            .ignore_proofs
            .range(context.buffer, context.lookup_props)
            != Some(1..count)
    {
        return None;
    }
    // MaySkipYes不依赖lookup编号或coverage，同props的新lookup仍可共用精确证明。
    context.lookup_index = 7;
    context.update_matchers();
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    // 重现嵌套lookup公开的props切换，再恢复父lookup；非Yes项仍运行原predicate。
    let saved = (context.lookup_props, context.lookup_index);
    context.lookup_props = 0;
    context.lookup_index = 9;
    context.update_matchers();
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    (context.lookup_props, context.lookup_index) = saved;
    context.update_matchers();
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    // 新mark写在旧证明的排他右端，不相交；完成真实append后才增长语义前缀。
    let old_range = context
        .ignore_proofs
        .range(context.buffer, context.lookup_props);
    let value = sequence_glyph(context.buffer, count, 0x64e);
    *context.buffer.out_info_at_mut(count) = value;
    context.buffer.out_len += 1;
    context.buffer.idx += 1;
    context.buffer.len += 1;
    let end = context.buffer.out_len;
    let value = sequence_glyph(context.buffer, end, 0x41);
    context.buffer.info[end] = value;
    if context.buffer.have_separate_output {
        context.buffer.pos[end] = bytemuck::cast(value);
    }
    if use_witness
        && context
            .ignore_proofs
            .range(context.buffer, context.lookup_props)
            != old_range
    {
        return None;
    }
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    // 对证明内部只授权一个真实范围就必须先撤权；新base应成为首个停止位置。
    let middle = count / 2;
    let value = sequence_glyph(context.buffer, middle, 0x41);
    context
        .buffer
        .with_out_info_finite_chunk_mut(middle..middle + 1, |part| part[0] = value)?;
    if use_witness
        && context
            .ignore_proofs
            .range(context.buffer, context.lookup_props)
            .is_some()
    {
        return None;
    }
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    // 改成Maybe项后两次独立请求都应调用有副作用predicate，不得误纳入纯忽略证明。
    let value = sequence_glyph(context.buffer, middle, 0x200d);
    *context.buffer.out_info_at_mut(middle) = value;
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    // 真实raw借用保持原地址和原数据，只声明有限可写范围；授权本身就撤旧证明。
    if context.buffer.have_separate_output {
        let (_, handle) = context.buffer.pos.as_mut_slice_with_checkpoint();
        if !handle.observe_raw_span(middle..middle + 1) || !handle.finish_callback() {
            return None;
        }
    } else {
        let (_, handle) = context.buffer.info.as_mut_slice_with_checkpoint();
        if !handle.observe_raw_span(middle..middle + 1) || !handle.finish_callback() {
            return None;
        }
    }
    if use_witness
        && context
            .ignore_proofs
            .range(context.buffer, context.lookup_props)
            .is_some()
    {
        return None;
    }
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    // 即使字节完全相同，切到另一个实际mapping也不得沿用原domain证明。
    for index in 0..context.buffer.len {
        let value = context.buffer.out_info_at(index);
        if context.buffer.have_separate_output {
            context.buffer.info[index] = value;
        } else {
            context.buffer.pos[index] = bytemuck::cast(value);
        }
    }
    context.buffer.have_separate_output = !context.buffer.have_separate_output;
    if use_witness
        && context
            .ignore_proofs
            .range(context.buffer, context.lookup_props)
            .is_some()
    {
        return None;
    }
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    // 实际容量增长会重建映射；所有旧地址域证明失效，随后只能重新读取原字形。
    if !context.buffer.ensure(count * 32) {
        return None;
    }
    if use_witness
        && context
            .ignore_proofs
            .range(context.buffer, context.lookup_props)
            .is_some()
    {
        return None;
    }
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    // 保留纯弱证明来观察scope结束；不能因缓存保活旧shape或把新context当作旧请求。
    let old_scope = Rc::downgrade(&context.ignore_proofs.scope);
    let old_witness = context
        .ignore_proofs
        .entries
        .borrow()
        .iter()
        .flatten()
        .next()
        .map(|e| e.witness.clone());
    drop(context);
    if old_scope.upgrade().is_some() {
        return None;
    }
    let mut context = ApplyContext::new(
        LayoutTableKind::Gsub,
        sequence_layout(),
        Scale::default(),
        &mut buffer,
    );
    context.lookup_props = u32::from(lookup_flags::IGNORE_MARKS);
    context.lookup_index = 1;
    context.update_matchers();
    if let Some(old) = old_witness {
        if context
            .buffer
            .out_info_witness_range(&context.ignore_proofs.scope, &old)
            .is_some()
        {
            return None;
        }
    }
    sequence_cursor_visit(
        &mut context,
        &calls,
        &predicate_hash,
        use_witness,
        &mut outcomes,
    );
    if !context.buffer.allocation_successful() {
        return None;
    }
    let mut glyph_hash = 0u64;
    for index in 0..context.buffer.out_len {
        for byte in bytemuck::bytes_of(&context.buffer.out_info_at(index)) {
            glyph_hash = glyph_hash.wrapping_mul(257).wrapping_add(u64::from(*byte));
        }
    }
    Some((outcomes, calls.get(), predicate_hash.get(), glyph_hash))
}

#[cfg(test)]
mod paged_backtrack_tests {
    use super::*;
    #[test]
    fn cancellation_inside_a_finite_chunk_never_publishes_an_ignore_witness() {
        use crate::{
            CancellationToken, CooperativeCheckpointKind, CooperativeControl,
            CooperativeSourceHealth,
        };
        use std::sync::Arc;
        let mut buffer = Buffer::new();
        for index in 0..=512 {
            buffer.push(0x301, index);
            let info = &mut buffer.info[index as usize];
            info.init_unicode_props(&mut buffer.scratch_flags);
            info.set_glyph_props(GlyphPropsFlags::MARK.bits());
            info.mask = 1;
        }
        buffer.have_output = true;
        buffer.out_len = 512;
        buffer.idx = 512;
        let token = CancellationToken::new();
        let trigger = token.clone();
        let health: CooperativeSourceHealth = Arc::new(move |point| {
            if point.kind == CooperativeCheckpointKind::Loop {
                trigger.request();
            }
            None
        });
        let control =
            CooperativeControl::new(token, buffer.paging_resource_observer(), Some(health));
        let mut context = ApplyContext::new(
            LayoutTableKind::Gsub,
            sequence_layout(),
            Scale::default(),
            &mut buffer,
        );
        context.lookup_props = u32::from(lookup_flags::IGNORE_MARKS);
        context.update_matchers();
        let result = crate::with_cooperative_control(control, || {
            let mut cursor = SkippingIterator::new(&mut context, true);
            cursor.reset_back(512);
            assert!(!cursor.prev_witness(None));
            crate::cooperative::poll_boundary()
        });
        assert_eq!(result, Err(crate::ShapeError::Cancelled));
        assert!(
            context
                .ignore_proofs
                .range(context.buffer, context.lookup_props)
                .is_none()
        );
    }

    #[test]
    fn finite_output_windows_preserve_original_cursor_and_predicate_contract() {
        assert!(super::paged_backtrack_cursor_contract());
    }
}
