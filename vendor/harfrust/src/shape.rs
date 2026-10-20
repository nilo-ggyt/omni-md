/// Shapes Unicode content in place using the configured font.
///
/// A supplied plan must match the buffer's direction and script. On success,
/// the buffer contains glyphs.
///
/// # Errors
///
/// Returns an error if the buffer cannot be shaped with the supplied plan or
/// lacks the properties needed to build one.
pub fn shape(
    font: &ShaperFont<'_, '_>,
    buffer: &mut Buffer,
    options: ShapeOptions<'_>,
) -> Result<(), ShapeError> {
    if buffer.content_type == Some(ContentType::Glyphs) {
        return Err(ShapeError::AlreadyShaped);
    }
    if let Some(plan) = options.plan {
        shape_with_font(plan, font, buffer, options.features, options.point_size)
    } else {
        if buffer.direction == Direction::Invalid {
            return Err(ShapeError::DirectionUnset);
        }
        let plan = ShapePlan::from_layout(
            font.layout(),
            buffer.direction,
            buffer.script,
            buffer.language.as_ref(),
            options.features,
        );
        shape_with_font(&plan, font, buffer, options.features, options.point_size)
    }
}

use crate::aat;
use crate::buffer::GlyphFlags;
use crate::buffer::*;
use crate::ot::gpos;
use crate::ot::layout::*;
use crate::ot::shaper::*;
use crate::ot::*;
use crate::plan::ShapePlan;
use crate::unicode::{CharExt, GeneralCategory};
use crate::{fallback, normalize};
use crate::{Advances, ShapeError, ShapeOptions, ShaperFont};
use crate::{BufferFlags, ContentType, Direction, Feature, Mask, Script};

/// Runs a compiled plan using prepared layout data and effective font queries.
pub(crate) fn shape_with_font(
    plan: &ShapePlan,
    font: &ShaperFont<'_, '_>,
    buffer: &mut Buffer,
    features: &[Feature],
    point_size: Option<f32>,
) -> Result<(), ShapeError> {
    if buffer.content_type == Some(ContentType::Glyphs) {
        return Err(ShapeError::AlreadyShaped);
    }
    if buffer.direction != plan.direction {
        return Err(ShapeError::DirectionMismatch {
            plan: plan.direction,
            buffer: buffer.direction,
        });
    }
    let plan_script = plan.script.unwrap_or(Script::UNKNOWN);
    let buffer_script = buffer.script.unwrap_or(Script::UNKNOWN);
    if buffer_script != plan_script {
        return Err(ShapeError::ScriptMismatch {
            plan: plan_script,
            buffer: buffer_script,
        });
    }

    buffer.enter();
    let result = if buffer.len > 0 {
        let target_direction = buffer.direction;
        OtShapeContext {
            plan,
            font,
            buffer,
            features,
            target_direction,
            point_size,
        }
        .shape_internal()
    } else {
        Ok(()) // 空输入仍需通过同scope最终权限门，不能绕过已经请求的取消。
    };
    buffer.leave(); // 即使算法撤权也归还原enter/leave状态，不把部分输出发布成Glyphs。
    result?;
    crate::cooperative::poll_boundary()?;
    buffer.content_type = Some(ContentType::Glyphs);
    Ok(())
}

// hb_ot_shape_context_t: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L450>
struct OtShapeContext<'a, 'c, 'd> {
    plan: &'a ShapePlan,
    font: &'a ShaperFont<'c, 'd>,
    buffer: &'a mut Buffer,
    features: &'a [Feature],
    // Transient stuff
    target_direction: Direction,
    point_size: Option<f32>,
}

impl OtShapeContext<'_, '_, '_> {
    fn glyph_h_advances(&mut self) {
        if self.buffer.len == 0 {
            return;
        }
        let batched_advances = Advances::new(self.buffer);
        self.font.glyph_h_advances(batched_advances);
        shape_cooperative_boundary!();
    }

    // hb_ot_shape_internal: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L1171>
    fn shape_internal(&mut self) -> Result<(), ShapeError> {
        use crate::cooperative::{phase, poll_boundary, CooperativePhase as Phase};
        self.buffer.allocate_unicode_vars();
        {
            let _phase = phase(Phase::Unicode);
            self.set_unicode_props(self.plan.ot_map.get_global_mask());
            poll_boundary()?;
            self.insert_dotted_circle();
            poll_boundary()?;
            form_clusters(self.buffer);
            poll_boundary()?;
            ensure_native_direction(self.buffer);
            poll_boundary()?;
        }
        if let Some(func) = self.plan.shaper.preprocess_text {
            let _phase = phase(Phase::Script);
            func(self.plan, self.font, self.buffer);
            poll_boundary()?; // 脚本桥的早退必须先传播，不能用部分重排继续进入GSUB。
        }
        {
            let _phase = phase(Phase::Substitute);
            self.substitute_pre();
            poll_boundary()?;
        }
        {
            let _phase = phase(Phase::Position);
            self.position();
            poll_boundary()?;
        }
        self.substitute_post();
        poll_boundary()?;
        propagate_flags(self.buffer);
        poll_boundary()?;
        self.buffer.deallocate_unicode_vars();
        self.buffer.direction = self.target_direction;
        Ok(())
    }

    // hb_ot_substitute_pre: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L936>
    fn substitute_pre(&mut self) {
        self.substitute_default();
        shape_cooperative_boundary!();

        self.buffer.allocate_gsubgpos_vars();

        self.substitute_plan();
        shape_cooperative_boundary!();

        if self.plan.apply_morx && self.plan.apply_gpos {
            aat::layout::remove_deleted_glyphs(self.buffer);
            shape_cooperative_boundary!();
        }
    }

    // hb_ot_substitute_post: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L951>
    fn substitute_post(&mut self) {
        if self.plan.apply_morx && !self.plan.apply_gpos {
            aat::layout::remove_deleted_glyphs(self.buffer);
            shape_cooperative_boundary!();
        }

        deal_with_variation_selectors(self.buffer);
        shape_cooperative_boundary!();
        hide_default_ignorables(self.buffer, self.font);
        shape_cooperative_boundary!();

        if let Some(func) = self.plan.shaper.postprocess_glyphs {
            {
                let _phase = crate::cooperative::phase(crate::CooperativePhase::Script);
                func(self.plan, self.font, self.buffer);
            }
            shape_cooperative_boundary!();
        }
    }

    // hb_ot_substitute_default: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L889>
    fn substitute_default(&mut self) {
        self.rotate_chars();
        shape_cooperative_boundary!();

        self.buffer
            .allocate_var(GlyphInfo::NORMALIZER_GLYPH_INDEX_VAR);

        {
            let _phase = crate::cooperative::phase(crate::CooperativePhase::Normalize);
            normalize::normalize(self.plan, self.buffer, self.font);
        }
        shape_cooperative_boundary!();

        self.setup_masks();
        shape_cooperative_boundary!();

        // This is unfortunate to go here, but necessary...
        if self.plan.fallback_mark_positioning {
            fallback::recategorize_marks(self.buffer);
            shape_cooperative_boundary!();
        }

        map_glyphs_fast(self.buffer);
        shape_cooperative_boundary!();

        self.buffer
            .deallocate_var(GlyphInfo::NORMALIZER_GLYPH_INDEX_VAR);
    }

    // hb_ot_substitute_plan: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L911>
    fn substitute_plan(&mut self) {
        if self.plan.apply_morx {
            substitute_start(self.font.layout().ot, self.buffer);
            shape_cooperative_boundary!();

            if self.plan.fallback_glyph_classes {
                synthesize_glyph_classes(self.buffer);
                shape_cooperative_boundary!();
            }

            {
                let _phase = crate::cooperative::phase(crate::CooperativePhase::Aat);
                aat::layout::substitute(self.plan, self.font.layout(), self.buffer, self.features);
            }
            shape_cooperative_boundary!();
            // The digest is only read by the OT lookup-apply loop; without
            // GPOS ahead, nothing consumes it.
            if self.plan.apply_gpos {
                self.buffer.update_digest();
            }
        } else {
            substitute_start_with_digest(self.font.layout().ot, self.buffer);
            shape_cooperative_boundary!();

            if self.plan.fallback_glyph_classes {
                synthesize_glyph_classes(self.buffer);
                shape_cooperative_boundary!();
            }

            gsub::substitute(self.plan, self.font, self.buffer);
            shape_cooperative_boundary!();
        }
    }

    // hb_ot_position: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L1094>
    fn position(&mut self) {
        self.buffer.clear_positions();
        shape_cooperative_boundary!(); // 有限页初始化撤权后，不再调用字体advance或其他后继阶段。

        self.position_default();
        shape_cooperative_boundary!();
        self.position_plan();
        shape_cooperative_boundary!();

        if self.buffer.direction.is_backward() {
            self.buffer.reverse();
        }

        self.buffer.deallocate_gsubgpos_vars();
    }

    // hb_ot_position_default: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L1002>
    fn position_default(&mut self) {
        let len = self.buffer.len;

        if self.buffer.direction.is_horizontal() {
            self.glyph_h_advances();
            shape_cooperative_boundary!();
        } else {
            // 配对读取保留两个数组的同一原下标，不建立完整数组迭代器。
            for mapped_index in 0..len {
                shape_cooperative_abort!();
                let info = &self.buffer.info[mapped_index];
                let pos = &mut self.buffer.pos[mapped_index];
                let glyph = info.as_glyph();
                pos.y_advance = self.font.glyph_v_advance(glyph);
                let (x, y) = self.font.glyph_v_origin(glyph);
                pos.x_offset = pos.x_offset.saturating_sub(x);
                pos.y_offset = pos.y_offset.saturating_sub(y);
            }
        }

        if self.buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_SPACE_FALLBACK != 0 {
            fallback::fallback_spaces(self.font, self.buffer);
            shape_cooperative_boundary!();
        }
    }

    // hb_ot_position_plan: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L1029>
    fn position_plan(&mut self) {
        // If the font has no GPOS and direction is forward, then when
        // zeroing mark widths, we shift the mark with it, such that the
        // mark is positioned hanging over the previous glyph.  When
        // direction is backward we don't shift and it will end up
        // hanging over the next glyph after the final reordering.
        //
        // Note: If fallback positioning happens, we don't care about
        // this as it will be overridden.
        let adjust_offsets_when_zeroing =
            self.plan.adjust_mark_positioning_when_zeroing && self.buffer.direction.is_forward();

        // We change glyph origin to what GPOS expects (horizontal), apply GPOS, change it back.

        gpos::position_start(self.buffer);
        shape_cooperative_boundary!();

        if self.plan.zero_marks && self.plan.shaper.zero_width_marks == ZeroWidthMarks::ByGdefEarly
        {
            zero_mark_widths_by_gdef(self.buffer, adjust_offsets_when_zeroing);
            shape_cooperative_boundary!();
        }

        self.position_by_plan();
        shape_cooperative_boundary!();

        if self.plan.zero_marks && self.plan.shaper.zero_width_marks == ZeroWidthMarks::ByGdefLate {
            zero_mark_widths_by_gdef(self.buffer, adjust_offsets_when_zeroing);
            shape_cooperative_boundary!();
        }

        // Finish off.  Has to follow a certain order.
        gpos::position_finish_advances(self.buffer);
        shape_cooperative_boundary!();
        zero_width_default_ignorables(self.buffer);
        shape_cooperative_boundary!();
        gpos::position_finish_offsets(self.buffer);
        shape_cooperative_boundary!();

        if self.plan.fallback_mark_positioning {
            fallback::position_marks(
                self.plan,
                self.font,
                self.buffer,
                adjust_offsets_when_zeroing,
            );
            shape_cooperative_boundary!();
        }
    }

    // ShapePlan::position <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L271>
    fn position_by_plan(&mut self) {
        let plan = self.plan;
        let layout = self.font.layout();
        let buffer = &mut *self.buffer;
        if plan.apply_gpos {
            gpos::position(plan, self.font, buffer);
            shape_cooperative_boundary!();
        } else if plan.apply_kerx {
            {
                let _phase = crate::cooperative::phase(crate::CooperativePhase::Aat);
                aat::layout::position(plan, layout, self.font.scale, buffer);
            }
            shape_cooperative_boundary!();
        }
        if plan.apply_kern {
            {
                let _phase = crate::cooperative::phase(crate::CooperativePhase::Aat);
                aat::kern::apply(plan, layout, self.font.scale, buffer);
            }
            shape_cooperative_boundary!();
        } else if plan.apply_fallback_kern {
            fallback::fallback_kern(plan, buffer);
            shape_cooperative_boundary!();
        }

        if plan.apply_trak {
            {
                let _phase = crate::cooperative::phase(crate::CooperativePhase::Aat);
                aat::layout::track(plan, layout, self.font.scale, self.point_size, buffer);
            }
            shape_cooperative_boundary!();
        }
    }

    // hb_ot_shape_setup_masks: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L757>
    fn setup_masks(&mut self) {
        self.setup_masks_fraction();
        shape_cooperative_boundary!();

        if let Some(func) = self.plan.shaper.setup_masks {
            {
                let _phase = crate::cooperative::phase(crate::CooperativePhase::Script);
                func(self.plan, self.font, self.buffer);
            }
            shape_cooperative_boundary!();
        }

        for feature in self.features {
            shape_cooperative_abort!();
            if !feature.is_global() {
                let (mask, shift) = self.plan.ot_map.get_mask(feature.tag);
                self.buffer
                    .set_masks(feature.value << shift, mask, feature.start, feature.end);
            }
        }
    }

    // hb_ot_shape_setup_masks_fraction: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L685>
    fn setup_masks_fraction(&mut self) {
        let buffer = &mut *self.buffer;
        if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_FRACTION_SLASH == 0
            || !self.plan.has_frac
        {
            return;
        }

        let (pre_mask, post_mask) = if buffer.direction.is_forward() {
            (
                self.plan.numr_mask | self.plan.frac_mask,
                self.plan.frac_mask | self.plan.dnom_mask,
            )
        } else {
            (
                self.plan.frac_mask | self.plan.dnom_mask,
                self.plan.numr_mask | self.plan.frac_mask,
            )
        };

        let len = buffer.len;
        let mut i = 0;
        while i < len {
            shape_cooperative_abort!();
            // FRACTION SLASH
            if buffer.info[i].glyph_id == 0x2044 {
                let mut start = i;
                while start > 0
                    && buffer.info[start - 1].general_category() == GeneralCategory::DECIMAL_NUMBER
                {
                    shape_cooperative_abort!();
                    start -= 1;
                }

                let mut end = i + 1;
                while end < len
                    && buffer.info[end].general_category() == GeneralCategory::DECIMAL_NUMBER
                {
                    shape_cooperative_abort!();
                    end += 1;
                }

                if start == i || end == i + 1 {
                    if start == i {
                        buffer.unsafe_to_concat(Some(start), Some(start + 1));
                    }

                    if end == i + 1 {
                        buffer.unsafe_to_concat(Some(end - 1), Some(end));
                    }

                    i += 1;
                    continue;
                }

                buffer.unsafe_to_break(Some(start), Some(end));

                // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。

                for mapped_index in start..i {
                    shape_cooperative_abort!();
                    let info = &mut buffer.info[mapped_index];
                    info.mask |= pre_mask;
                }

                buffer.info[i].mask |= self.plan.frac_mask;

                // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。

                for mapped_index in i + 1..end {
                    shape_cooperative_abort!();
                    let info = &mut buffer.info[mapped_index];
                    info.mask |= post_mask;
                }

                i = end;
            } else {
                i += 1;
            }
        }
    }

    // hb_set_unicode_props: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L471>
    fn set_unicode_props(&mut self, global_mask: Mask) {
        let buffer = &mut *self.buffer;
        // Implement enough of Unicode Graphemes here that shaping
        // in reverse-direction wouldn't break graphemes.  Namely,
        // we mark all marks and ZWJ and ZWJ,Extended_Pictographic
        // sequences as continuations.  The foreach_grapheme()
        // macro uses this bit.
        //
        // https://www.unicode.org/reports/tr29/#Regex_Definitions

        let len = buffer.len;

        let mut i = 0;
        while i < len {
            shape_cooperative_abort!();
            let info = &mut buffer.info[i];
            info.mask = global_mask;
            info.init_unicode_props(&mut buffer.scratch_flags);

            if info.glyph_id < 0x80 {
                i += 1;
                continue;
            }

            let gen_cat = info.general_category();

            if gen_cat.flag_unsafe()
                & (GeneralCategory::LOWERCASE_LETTER.flag()
                    | GeneralCategory::UPPERCASE_LETTER.flag()
                    | GeneralCategory::TITLECASE_LETTER.flag()
                    | GeneralCategory::OTHER_LETTER.flag()
                    | GeneralCategory::SPACE_SEPARATOR.flag())
                != 0
            {
                i += 1;
                continue;
            }

            // 前一项只需 POD 值副本；避免创建覆盖完整 scratch 的 split slice，
            // 两次有限索引仍读取原先同一前驱，且当前项保持唯一可变借用。
            let prior = i.checked_sub(1).map(|index| buffer.info[index]);
            let info = &mut buffer.info[i];

            // Marks are already set as continuation by the above line.
            // Handle Emoji_Modifier and ZWJ-continuation.
            if gen_cat == GeneralCategory::MODIFIER_SYMBOL
                && matches!(info.glyph_id, 0x1F3FB..=0x1F3FF)
            {
                info.set_continuation(&mut buffer.scratch_flags);
            } else if i != 0 && matches!(info.glyph_id, 0x1F1E6..=0x1F1FF) {
                // Should never fail because we checked for i > 0.
                // TODO: use let chains when they become stable
                let prev = prior.as_ref().unwrap();
                if matches!(prev.glyph_id, 0x1F1E6..=0x1F1FF) && !prev.is_continuation() {
                    info.set_continuation(&mut buffer.scratch_flags);
                }
            } else if info.is_zwj() {
                info.set_continuation(&mut buffer.scratch_flags);
                if i + 1 < len {
                    let next = &mut buffer.info[i + 1];
                    if next.as_codepoint().is_emoji_extended_pictographic() {
                        next.mask = global_mask;
                        next.init_unicode_props(&mut buffer.scratch_flags);
                        next.set_continuation(&mut buffer.scratch_flags);
                        i += 1;
                    }
                }
            } else if matches!(info.glyph_id, 0xFF9E..=0xFF9F | 0xE0020..=0xE007F) {
                // Or part of the Other_Grapheme_Extend that is not marks.
                // As of Unicode 15 that is just:
                //
                // 200C          ; Other_Grapheme_Extend # Cf       ZERO WIDTH NON-JOINER
                // FF9E..FF9F    ; Other_Grapheme_Extend # Lm   [2] HALFWIDTH KATAKANA VOICED SOUND MARK..HALFWIDTH KATAKANA
                // SEMI-VOICED SOUND MARK E0020..E007F  ; Other_Grapheme_Extend # Cf  [96] TAG SPACE..CANCEL TAG
                //
                // ZWNJ is special, we don't want to merge it as there's no need, and keeping
                // it separate results in more granular clusters.
                // Tags are used for Emoji sub-region flag sequences:
                // https://github.com/harfbuzz/harfbuzz/issues/1556
                // Katakana ones were requested:
                // https://github.com/harfbuzz/harfbuzz/issues/3844
                info.set_continuation(&mut buffer.scratch_flags);
            } else if info.glyph_id == 0x2044
            /* FRACTION SLASH */
            {
                buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_HAS_FRACTION_SLASH;
            }

            i += 1;
        }
    }

    // hb_insert_dotted_circle: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L549>
    fn insert_dotted_circle(&mut self) {
        let should_insert = {
            let buffer = &*self.buffer;
            !buffer
                .flags
                .contains(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE)
                && buffer.flags.contains(BufferFlags::BEGINNING_OF_TEXT)
                && buffer.context_len[0] == 0
                && buffer.info[0].is_unicode_mark()
        };

        if should_insert && self.font.nominal_glyph(0x25CC).is_some() {
            let mask = self.buffer.cur(0).mask;
            let cluster = self.buffer.cur(0).cluster;
            let buffer = &mut *self.buffer;
            let mut info = GlyphInfo {
                glyph_id: 0x25CC,
                mask,
                cluster,
                ..GlyphInfo::default()
            };

            info.init_unicode_props(&mut buffer.scratch_flags);
            buffer.clear_output();
            buffer.output_info(info);
            buffer.sync();
        }
    }

    // hb_ot_rotate_chars: <https://github.com/harfbuzz/harfbuzz/blob/22ea52f42fa4fc168be91ef4e56aee3affda6e28/src/hb-ot-shape.cc#L652>
    fn rotate_chars(&mut self) {
        let len = self.buffer.len;

        if self.target_direction.is_backward() {
            let rtlm_mask = self.plan.rtlm_mask;

            // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。

            for mapped_index in 0..len {
                shape_cooperative_abort!();
                let info = &mut self.buffer.info[mapped_index];
                if let Some(c) = info.as_codepoint().mirroring() {
                    if self.font.nominal_glyph(c).is_some() {
                        info.glyph_id = c;
                        continue;
                    }
                }
                info.mask |= rtlm_mask;
            }
        }

        if self.target_direction.is_vertical() && !self.plan.has_vert {
            // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。
            for mapped_index in 0..len {
                shape_cooperative_abort!();
                let info = &mut self.buffer.info[mapped_index];
                if let Some(c) = info.as_codepoint().vertical() {
                    if self.font.nominal_glyph(c).is_some() {
                        info.glyph_id = c;
                    }
                }
            }
        }
    }
}

fn form_clusters(buffer: &mut Buffer) {
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_CONTINUATIONS != 0 {
        foreach_grapheme!(buffer, start, end, {
            buffer.merge_grapheme_clusters(start, end);
            shape_cooperative_boundary!();
        });
    }
}

fn ensure_native_direction(buffer: &mut Buffer) {
    let dir = buffer.direction;
    let mut hor = buffer
        .script
        .and_then(Direction::from_script)
        .unwrap_or_default();

    // Numeric runs in natively-RTL scripts are actually native-LTR, so we reset
    // the horiz_dir if the run contains at least one decimal-number char, and no
    // letter chars (ideally we should be checking for chars with strong
    // directionality but hb-unicode currently lacks bidi categories).
    //
    // This allows digit sequences in Arabic etc to be shaped in "native"
    // direction, so that features like ligatures will work as intended.
    //
    // https://github.com/harfbuzz/harfbuzz/issues/501
    //
    // Similar thing about Regional_Indicators; They are bidi=L, but Script=Common.
    // If they are present in a run of natively-RTL text, they get assigned a script
    // with natively RTL direction, which would result in wrong shaping if we
    // assign such native RTL direction to them then. Detect that as well.
    //
    // https://github.com/harfbuzz/harfbuzz/issues/3314

    if hor == Direction::RightToLeft && dir == Direction::LeftToRight {
        let mut found_number = false;
        let mut found_letter = false;
        let mut found_ri = false;
        // 初始化 scratch 长度保持不变；标量索引让长扫描在实际访问处执行分页撤驻留。
        for mapped_index in 0..buffer.info.len() {
            shape_cooperative_abort!();
            let info = &buffer.info[mapped_index];
            let gc = info.general_category();
            if gc == GeneralCategory::DECIMAL_NUMBER {
                found_number = true;
            } else if gc.is_letter() {
                found_letter = true;
                break;
            } else if matches!(info.glyph_id, 0x1F1E6..=0x1F1FF) {
                found_ri = true;
            }
        }
        if (found_number || found_ri) && !found_letter {
            hor = Direction::LeftToRight;
        }
    }

    // TODO vertical:
    // The only BTT vertical script is Ogham, but it's not clear to me whether OpenType
    // Ogham fonts are supposed to be implemented BTT or not.  Need to research that
    // first.
    if (dir.is_horizontal() && dir != hor && hor != Direction::Invalid)
        || (dir.is_vertical() && dir != Direction::TopToBottom)
    {
        reverse_graphemes(buffer);
        buffer.direction = buffer.direction.reverse();
    }
}

fn map_glyphs_fast(buffer: &mut Buffer) {
    // Normalization process sets up normalizer_glyph_index(), we just copy it.
    let len = buffer.len;
    // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。
    for mapped_index in 0..len {
        shape_cooperative_abort!();
        let info = &mut buffer.info[mapped_index];
        info.glyph_id = info.normalizer_glyph_index();
    }

    // 输出 scratch 可能借用 pos 的同一映射；有限索引由 Buffer 保持原类型转换。
    for mapped_index in 0..len {
        shape_cooperative_abort!();
        let info = buffer.out_info_at_mut(mapped_index);
        info.glyph_id = info.normalizer_glyph_index();
    }
}

fn synthesize_glyph_classes(buffer: &mut Buffer) {
    let len = buffer.len;
    // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。
    for mapped_index in 0..len {
        shape_cooperative_abort!();
        let info = &mut buffer.info[mapped_index];
        // Never mark default-ignorables as marks.
        // They won't get in the way of lookups anyway,
        // but having them as mark will cause them to be skipped
        // over if the lookup-flag says so, but at least for the
        // Mongolian variation selectors, looks like Uniscribe
        // marks them as non-mark.  Some Mongolian fonts without
        // GDEF rely on this.  Another notable character that
        // this applies to is COMBINING GRAPHEME JOINER.
        let class = if info.general_category() != GeneralCategory::NON_SPACING_MARK
            || info.is_default_ignorable()
        {
            GlyphPropsFlags::BASE_GLYPH
        } else {
            GlyphPropsFlags::MARK
        };

        info.set_glyph_props(class.bits());
    }
}

fn zero_width_default_ignorables(buffer: &mut Buffer) {
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_DEFAULT_IGNORABLES != 0
        && !buffer
            .flags
            .contains(BufferFlags::PRESERVE_DEFAULT_IGNORABLES)
        && !buffer
            .flags
            .contains(BufferFlags::REMOVE_DEFAULT_IGNORABLES)
    {
        let len = buffer.len;
        // 配对读取保留两个数组的同一原下标，不建立完整数组迭代器。
        for mapped_index in 0..len {
            shape_cooperative_abort!();
            let info = &buffer.info[mapped_index];
            let pos = &mut buffer.pos[mapped_index];
            if info.is_default_ignorable() {
                pos.x_advance = 0;
                pos.y_advance = 0;
                if buffer.direction.is_horizontal() {
                    pos.x_offset = 0;
                } else {
                    pos.y_offset = 0;
                }
            }
        }
    }
}

fn deal_with_variation_selectors(buffer: &mut Buffer) {
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_VARIATION_SELECTOR_FALLBACK == 0 {
        return;
    }

    // Note: In harfbuzz, this is part of the condition above (with OR), so it needs to stay
    // in sync.
    let Some(nf) = buffer.not_found_variation_selector else {
        return;
    };

    let count = buffer.len;
    let info = &mut buffer.info;
    let pos = &mut buffer.pos;

    for i in 0..count {
        shape_cooperative_abort!();
        if info[i].is_variation_selector() {
            info[i].glyph_id = nf;
            pos[i].x_advance = 0;
            pos[i].y_advance = 0;
            pos[i].x_offset = 0;
            pos[i].y_offset = 0;
            info[0].set_variation_selector(false);
        }
    }
}

fn zero_mark_widths_by_gdef(buffer: &mut Buffer, adjust_offsets: bool) {
    let len = buffer.len;
    // 配对读取保留两个数组的同一原下标，不建立完整数组迭代器。
    for mapped_index in 0..len {
        shape_cooperative_abort!();
        let info = &buffer.info[mapped_index];
        let pos = &mut buffer.pos[mapped_index];
        if info.is_mark() {
            if adjust_offsets {
                pos.x_offset = pos.x_offset.saturating_sub(pos.x_advance);
                pos.y_offset = pos.y_offset.saturating_sub(pos.y_advance);
            }

            pos.x_advance = 0;
            pos.y_advance = 0;
        }
    }
}

fn hide_default_ignorables(buffer: &mut Buffer, font: &ShaperFont) {
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_DEFAULT_IGNORABLES != 0
        && !buffer
            .flags
            .contains(BufferFlags::PRESERVE_DEFAULT_IGNORABLES)
    {
        if !buffer
            .flags
            .contains(BufferFlags::REMOVE_DEFAULT_IGNORABLES)
        {
            if let Some(invisible) = buffer
                .invisible
                .or_else(|| font.nominal_glyph(u32::from(' ')))
            {
                let len = buffer.len;
                // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。
                for mapped_index in 0..len {
                    shape_cooperative_abort!();
                    let info = &mut buffer.info[mapped_index];
                    if info.is_default_ignorable() {
                        info.glyph_id = invisible.to_u32();
                    }
                }
                return;
            }
        }

        buffer.delete_glyphs_inplace(GlyphInfo::is_default_ignorable);
    }
}

fn propagate_flags(buffer: &mut Buffer) {
    // Propagate cluster-level glyph flags to be the same on all cluster glyphs.
    // Simplifies using them.

    let mut and_mask = GlyphFlags::DEFINED_BITS;
    if !buffer.flags.contains(BufferFlags::PRODUCE_UNSAFE_TO_CONCAT) {
        and_mask &= !GlyphFlags::UNSAFE_TO_CONCAT.0;
    }

    if !buffer
        .flags
        .contains(BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL)
    {
        foreach_cluster!(buffer, start, end, {
            if end - start == 1 {
                buffer.info[start].mask &= and_mask;
            } else {
                let mut mask = 0;
                // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。
                for mapped_index in start..end {
                    shape_cooperative_abort!();
                    let info = &buffer.info[mapped_index];
                    mask |= info.mask;
                }

                mask &= and_mask;

                // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。

                for mapped_index in start..end {
                    shape_cooperative_abort!();
                    let info = &mut buffer.info[mapped_index];
                    info.mask = mask;
                }
            }
        });
        return;
    }

    /* If we are producing SAFE_TO_INSERT_TATWEEL, then do two things:
     *
     * - If the places that the Arabic shaper marked as SAFE_TO_INSERT_TATWEEL,
     *   are UNSAFE_TO_BREAK, then clear the SAFE_TO_INSERT_TATWEEL,
     * - Any place that is SAFE_TO_INSERT_TATWEEL, is also now UNSAFE_TO_BREAK.
     *
     * We couldn't make this interaction earlier. It has to be done here.
     */
    foreach_cluster!(buffer, start, end, {
        // We cannot use `continue` in our `for_each_cluster!` macro.
        if end - start != 1 {
            let mut mask = 0;
            // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。
            for mapped_index in start..end {
                shape_cooperative_abort!();
                let info = &buffer.info[mapped_index];
                mask |= info.mask;
            }

            mask &= GlyphFlags::DEFINED_BITS;

            if mask & GlyphFlags::UNSAFE_TO_BREAK.0 != 0 {
                mask &= !GlyphFlags::SAFE_TO_INSERT_TATWEEL.0;
            }

            if mask & GlyphFlags::SAFE_TO_INSERT_TATWEEL.0 != 0 {
                mask |= GlyphFlags::UNSAFE_TO_BREAK.0 | GlyphFlags::UNSAFE_TO_CONCAT.0;
            }

            mask &= and_mask;

            // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。

            for mapped_index in start..end {
                shape_cooperative_abort!();
                let info = &mut buffer.info[mapped_index];
                info.mask = mask;
            }
        }
    });
}
