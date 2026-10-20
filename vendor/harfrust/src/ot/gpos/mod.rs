//! OpenType GPOS lookups.

mod cursive;
mod mark;
mod pair;
mod single;
mod value;

use crate::buffer::*;
use crate::mapped_pod_vec::MappedPodVec;
use crate::ot::layout::*;
use crate::plan::ShapePlan;
use crate::Direction;
use crate::ShaperFont;

pub fn position(plan: &ShapePlan, font: &ShaperFont<'_, '_>, buffer: &mut Buffer) {
    let table = font.layout().ot.gpos.clone();
    apply_layout_table(plan, font, buffer, table.as_ref());
}

pub mod attach_type {
    pub const MARK: u8 = 1;
    pub const CURSIVE: u8 = 2;
}

/// 精确和只累积真实 advance；last_* 保存最后一个非零符号的下标加一，0 表示尚未出现。
/// i128 足以容纳 aarch64 地址空间内任意可分配 GlyphPosition 数乘完整 i32 域。
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct AdvancePrefix {
    sums: [i128; 2],
    last_positive: [usize; 2],
    last_negative: [usize; 2],
}

struct AdvancePrefixes {
    rows: MappedPodVec<AdvancePrefix>,
}

impl AdvancePrefixes {
    fn build(pos: &MappedPodVec<GlyphPosition>, len: usize) -> Option<Self> {
        let count = len.checked_add(1)?;
        let mut rows = MappedPodVec::new_in(pos.paging_scope());
        if !rows.prepare_resize(count, AdvancePrefix::default()) {
            return None;
        }
        rows.commit_len(count);
        let mut row = AdvancePrefix::default();
        rows[0] = row;
        // 每项访问均经过原映射窗口；新增文件只存固定宽度元数据，不建立常驻 heap N 数组。
        for index in 0..len {
            shape_cooperative_abort!();
            let value = pos[index];
            for (axis, advance) in [value.x_advance, value.y_advance].into_iter().enumerate() {
                shape_cooperative_abort!();
                row.sums[axis] += i128::from(advance);
                if advance > 0 {
                    row.last_positive[axis] = index + 1;
                } else if advance < 0 {
                    row.last_negative[axis] = index + 1;
                }
            }
            rows[index + 1] = row;
        }
        if !rows.healthy() {
            return None;
        }
        Some(Self { rows })
    }

    fn apply(
        &self,
        offset: i32,
        start: usize,
        end: usize,
        axis: usize,
        subtract: bool,
    ) -> Option<i32> {
        if start > end || end >= self.rows.len() || axis >= 2 {
            return None;
        }
        let after = self.rows[end];
        if after.last_positive[axis] > start && after.last_negative[axis] > start {
            // 正负交替时饱和运算不具结合律；保留原逐项路径，而不是返回有损近似值。
            return None;
        }
        let before = self.rows[start];
        let sum = after.sums[axis] - before.sums[axis];
        // 所有步进同号时中途最多触达一侧边界，最终一次 clamp 与原每步 saturation 等价。
        let result = if subtract {
            i128::from(offset) - sum
        } else {
            i128::from(offset) + sum
        };
        Some(result.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32)
    }
}

/// See <https://github.com/harfbuzz/harfbuzz/blob/7d936359a27abb2d7cb14ecc102463bb15c11843/src/OT/Layout/GPOS/GPOS.hh#L75>
fn propagate_attachment_offsets(
    pos: &mut MappedPodVec<GlyphPosition>,
    len: usize,
    i: usize,
    direction: Direction,
    nesting_level: usize,
    advances: Option<&AdvancePrefixes>,
) {
    // Adjusts offsets of attached glyphs (both cursive and mark) to accumulate
    // offset of glyph they are attached to.
    let chain = pos[i].attach_chain();
    let kind = pos[i].attach_type();

    pos[i].set_attach_chain(0);

    let j = (i as isize + isize::from(chain)) as usize;
    if j >= len {
        return;
    }

    if nesting_level == 0 {
        return;
    }

    if pos[j].attach_chain() != 0 {
        propagate_attachment_offsets(pos, len, j, direction, nesting_level - 1, advances);
    }

    match kind {
        attach_type::MARK => {
            if direction.is_horizontal() {
                pos[i].x_offset = pos[i].x_offset.saturating_add(pos[j].x_offset);
            } else {
                pos[i].y_offset = pos[i].y_offset.saturating_add(pos[j].y_offset);
            }

            // 端点和加减方向逐项对应原来的四个分支；先前的 parent offset 饱和加法
            // 与递归次序均保持原位。此阶段只修改 offset/chain，从不修改前缀引用的 advance。
            let (start, end, subtract) = if j < i {
                if direction.is_forward() {
                    (j, i, true)
                } else {
                    (j + 1, i + 1, false)
                }
            } else if direction.is_forward() {
                (i, j, false)
            } else {
                (i + 1, j + 1, true)
            };
            let x =
                advances.and_then(|prefix| prefix.apply(pos[i].x_offset, start, end, 0, subtract));
            let y =
                advances.and_then(|prefix| prefix.apply(pos[i].y_offset, start, end, 1, subtract));
            if let Some(value) = x {
                pos[i].x_offset = value;
            }
            if let Some(value) = y {
                pos[i].y_offset = value;
            }
            if x.is_none() || y.is_none() {
                // 混合正负增量可能中途饱和后恢复，普通 prefix difference 不等价。
                // 只对不能证明单调的轴保留上游逐项饱和次序，不能为性能丢失该语义。
                for k in start..end {
                    shape_cooperative_abort!();
                    if x.is_none() {
                        pos[i].x_offset = if subtract {
                            pos[i].x_offset.saturating_sub(pos[k].x_advance)
                        } else {
                            pos[i].x_offset.saturating_add(pos[k].x_advance)
                        };
                    }
                    if y.is_none() {
                        pos[i].y_offset = if subtract {
                            pos[i].y_offset.saturating_sub(pos[k].y_advance)
                        } else {
                            pos[i].y_offset.saturating_add(pos[k].y_advance)
                        };
                    }
                }
            }
        }
        attach_type::CURSIVE => {
            if direction.is_horizontal() {
                pos[i].y_offset = pos[i].y_offset.saturating_add(pos[j].y_offset);
            } else {
                pos[i].x_offset = pos[i].x_offset.saturating_add(pos[j].x_offset);
            }
        }
        _ => {}
    }
}

pub fn position_start(buffer: &mut Buffer) {
    let len = buffer.len;
    // 分页容器逐项触达，保留原顺序与同一整形 run，避免整片 slice 绕过驻留检查点。
    for mapped_index in 0..len {
        shape_cooperative_abort!();
        let pos = &mut buffer.pos[mapped_index];
        pos.set_attach_chain(0);
        pos.set_attach_type(0);
    }
}

pub fn position_finish_advances(_: &mut Buffer) {
    //buffer.assert_gsubgpos_vars();
}

pub fn position_finish_offsets(buffer: &mut Buffer) {
    buffer.assert_gsubgpos_vars();

    let len = buffer.len;
    let direction = buffer.direction;

    // Handle attachments
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_GPOS_ATTACHMENT != 0 {
        // GPOS/kerx 已经完成 advance 修改，而本函数接下来仅调整 offset 与 chain。
        // 一份短生命周期映射记录精确整数前缀和及正负下标；不会复制字符或字形数组。
        let Some(advances) = AdvancePrefixes::build(&buffer.pos, len) else {
            buffer.successful = false;
            return;
        };
        // https://github.com/harfbuzz/harfbuzz/issues/5514
        if buffer.direction.is_forward() {
            for i in 0..len {
                shape_cooperative_abort!();
                if buffer.pos[i].attach_chain() != 0 {
                    propagate_attachment_offsets(
                        &mut buffer.pos,
                        len,
                        i,
                        direction,
                        MAX_NESTING_LEVEL,
                        Some(&advances),
                    );
                }
            }
        } else {
            for i in (0..len).rev() {
                shape_cooperative_abort!();
                if buffer.pos[i].attach_chain() != 0 {
                    propagate_attachment_offsets(
                        &mut buffer.pos,
                        len,
                        i,
                        direction,
                        MAX_NESTING_LEVEL,
                        Some(&advances),
                    );
                }
            }
        }
        if !advances.rows.healthy() {
            // 元数据 IO 错误不能随临时 map 析构而丢失，否则 runner 会误签原结果。
            buffer.successful = false;
        }
    }
}

/// 仅离线 contracts 入口：有限合成记录对照已发布版逐项饱和算法，不接受正文或用户文件。
#[doc(hidden)]
#[cfg(test)]
fn paged_attachment_contract() -> bool {
    use alloc::vec;
    for len in [0usize, 1, 2, 7, 17, MAX_NESTING_LEVEL + 2, 129] {
        for case in 0..6 {
            let mut source = vec![GlyphPosition::default(); len];
            for (index, value) in source.iter_mut().enumerate() {
                let (x, y) = match case {
                    0 => (if index == 0 { 1200 } else { 0 }, 0),
                    1 => (i32::MAX, i32::MIN),
                    2 => (
                        if index % 2 == 0 { i32::MAX } else { -i32::MAX },
                        if index % 3 == 0 { i32::MIN } else { i32::MAX },
                    ),
                    3 => (-37, 29),
                    4 => (if index < len / 2 { -100 } else { 300 }, 0),
                    _ => (index as i32 * 3, -(index as i32) * 7),
                };
                value.x_advance = x;
                value.y_advance = y;
                value.x_offset = [i32::MIN, i32::MAX, 40_000_000, -40_000_000, 0][index % 5];
                value.y_offset = [i32::MAX, i32::MIN, -40_000_000, 40_000_000, 0][index % 5];
                value.set_attach_type(if case == 5 && index % 3 == 0 {
                    attach_type::CURSIVE
                } else {
                    attach_type::MARK
                });
                let chain = match case {
                    0 | 2 => -(index as i16),
                    1 | 3 => (len - 1 - index) as i16,
                    4 => {
                        if index > 0 {
                            -1
                        } else {
                            0
                        }
                    }
                    _ => {
                        if index % 4 == 0 {
                            i16::MAX
                        } else if index % 4 == 1 {
                            i16::MIN
                        } else if index > 0 {
                            -1
                        } else {
                            0
                        }
                    }
                };
                value.set_attach_chain(chain);
            }
            for direction in [
                Direction::LeftToRight,
                Direction::RightToLeft,
                Direction::TopToBottom,
                Direction::BottomToTop,
            ] {
                let mut expected = source.clone();
                let mut actual = MappedPodVec::new();
                if !actual.prepare_resize(len, GlyphPosition::default()) {
                    return false;
                }
                actual.commit_len(len);
                for (index, value) in source.iter().copied().enumerate() {
                    actual[index] = value;
                }
                let Some(prefix) = AdvancePrefixes::build(&actual, len) else {
                    return false;
                };
                for step in 0..len {
                    let index = if direction.is_forward() {
                        step
                    } else {
                        len - 1 - step
                    };
                    if expected[index].attach_chain() != 0 {
                        reference_attachment_offsets(
                            &mut expected,
                            len,
                            index,
                            direction,
                            MAX_NESTING_LEVEL,
                        );
                    }
                    if actual[index].attach_chain() != 0 {
                        propagate_attachment_offsets(
                            &mut actual,
                            len,
                            index,
                            direction,
                            MAX_NESTING_LEVEL,
                            Some(&prefix),
                        );
                    }
                }
                for (index, expected) in expected.iter().enumerate() {
                    let observed = actual[index];
                    if bytemuck::bytes_of(&observed) != bytemuck::bytes_of(expected) {
                        eprintln!(
                            "paged_attachment_contract mismatch case={case} len={len} direction={direction:?} index={index}"
                        );
                        return false;
                    }
                }
                if !actual.healthy() || !prefix.rows.healthy() {
                    return false;
                }
                // 小实例额外穷举所有区间。Some 必须等于原步进；混合符号允许 None 明示回退。
                if len <= 17 {
                    for start in 0..=len {
                        for end in start..=len {
                            for axis in 0..2 {
                                for subtract in [false, true] {
                                    for offset in [i32::MIN, -1, 0, 1, i32::MAX] {
                                        let mut sequential = offset;
                                        for value in &source[start..end] {
                                            let advance = if axis == 0 {
                                                value.x_advance
                                            } else {
                                                value.y_advance
                                            };
                                            sequential = if subtract {
                                                sequential.saturating_sub(advance)
                                            } else {
                                                sequential.saturating_add(advance)
                                            };
                                        }
                                        let mut positive = false;
                                        let mut negative = false;
                                        for value in &source[start..end] {
                                            let advance = if axis == 0 {
                                                value.x_advance
                                            } else {
                                                value.y_advance
                                            };
                                            positive |= advance > 0;
                                            negative |= advance < 0;
                                        }
                                        let candidate =
                                            prefix.apply(offset, start, end, axis, subtract);
                                        if candidate.is_some() != !(positive && negative)
                                            || candidate.is_some_and(|value| value != sequential)
                                        {
                                            eprintln!(
                                                "paged_attachment_contract prefix mismatch case={case} len={len} start={start} end={end} axis={axis} subtract={subtract}"
                                            );
                                            return false;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    true
}

// 此有限控制参考函数逐字保留发布版的方向、递归截断和饱和步进，仅改函数名。
#[cfg(test)]
fn reference_attachment_offsets(
    pos: &mut [GlyphPosition],
    len: usize,
    i: usize,
    direction: Direction,
    nesting_level: usize,
) {
    // Adjusts offsets of attached glyphs (both cursive and mark) to accumulate
    // offset of glyph they are attached to.
    let chain = pos[i].attach_chain();
    let kind = pos[i].attach_type();

    pos[i].set_attach_chain(0);

    let j = (i as isize + isize::from(chain)) as usize;
    if j >= len {
        return;
    }

    if nesting_level == 0 {
        return;
    }

    if pos[j].attach_chain() != 0 {
        reference_attachment_offsets(pos, len, j, direction, nesting_level - 1);
    }

    match kind {
        attach_type::MARK => {
            if direction.is_horizontal() {
                pos[i].x_offset = pos[i].x_offset.saturating_add(pos[j].x_offset);
            } else {
                pos[i].y_offset = pos[i].y_offset.saturating_add(pos[j].y_offset);
            }

            // i is the position of the mark; j is the base.
            if j < i {
                // This is the common case: mark follows base.
                // And currently the only way in OpenType.
                if direction.is_forward() {
                    for k in j..i {
                        pos[i].x_offset = pos[i].x_offset.saturating_sub(pos[k].x_advance);
                        pos[i].y_offset = pos[i].y_offset.saturating_sub(pos[k].y_advance);
                    }
                } else {
                    for k in j + 1..i + 1 {
                        pos[i].x_offset = pos[i].x_offset.saturating_add(pos[k].x_advance);
                        pos[i].y_offset = pos[i].y_offset.saturating_add(pos[k].y_advance);
                    }
                }
            } else {
                // This can happen with `kerx`: a mark attaching
                // to a base after it in the logical order.
                if direction.is_forward() {
                    for k in i..j {
                        pos[i].x_offset = pos[i].x_offset.saturating_add(pos[k].x_advance);
                        pos[i].y_offset = pos[i].y_offset.saturating_add(pos[k].y_advance);
                    }
                } else {
                    for k in i + 1..j + 1 {
                        pos[i].x_offset = pos[i].x_offset.saturating_sub(pos[k].x_advance);
                        pos[i].y_offset = pos[i].y_offset.saturating_sub(pos[k].y_advance);
                    }
                }
            }
        }
        attach_type::CURSIVE => {
            if direction.is_horizontal() {
                pos[i].y_offset = pos[i].y_offset.saturating_add(pos[j].y_offset);
            } else {
                pos[i].x_offset = pos[i].x_offset.saturating_add(pos[j].x_offset);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_advance_prefix_uses_original_job_scope_and_releases_real_resources() {
        let mut positions = MappedPodVec::new();
        assert!(positions.prepare_resize(40, GlyphPosition::default()));
        positions.commit_len(40);
        let observer = positions.resource_observer();
        let active = observer.snapshot();
        // 小任务只拥有一个真实64 KiB工作块，不应为了沿用旧断言创建匿名文件。
        assert_eq!(
            (
                active.work_bytes,
                active.peak_work_bytes,
                active.files_open,
                active.mappings_open,
                active.mapped_bytes
            ),
            (64 * 1024, 64 * 1024, 0, 0, 0)
        );
        let prefix = AdvancePrefixes::build(&positions, 40).unwrap();
        // 前缀实际分配第二个工作块，必须计入positions的原scope，而非另建任务漏计。
        let with_prefix = observer.snapshot();
        assert_eq!(
            (
                with_prefix.work_bytes,
                with_prefix.peak_work_bytes,
                with_prefix.files_open,
                with_prefix.mappings_open,
                with_prefix.mapped_bytes
            ),
            (128 * 1024, 128 * 1024, 0, 0, 0)
        );
        drop(prefix);
        let without_prefix = observer.snapshot();
        // 只归还临时前缀；原positions仍有效，历史峰值不能随当前用量一起归零。
        assert_eq!(
            (
                without_prefix.work_bytes,
                without_prefix.peak_work_bytes,
                without_prefix.files_open,
                without_prefix.mappings_open,
                without_prefix.mapped_bytes
            ),
            (64 * 1024, 128 * 1024, 0, 0, 0)
        );
        drop(positions);
        let ended = observer.snapshot();
        assert_eq!(
            (
                ended.work_bytes,
                ended.peak_work_bytes,
                ended.files_open,
                ended.mappings_open,
                ended.mapped_bytes,
                ended.peak_mapped_bytes
            ),
            (0, 128 * 1024, 0, 0, 0, 0)
        );
        assert_eq!((ended.files_created, ended.mappings_created), (0, 0));
        assert_eq!(ended.files_created, ended.files_closed);
        assert_eq!(ended.mappings_created, ended.mappings_released);
        assert!(!ended.cleanup_failed && ended.first_error.is_none());
    }

    #[test]
    fn mapped_prefix_matches_original_saturating_attachment_contract() {
        assert!(paged_attachment_contract());
    }

    #[test]
    fn propagate_attachment_offsets_saturates_deep_mark_chains() {
        let len = MAX_NESTING_LEVEL + 2;
        let mut pos = MappedPodVec::new();
        assert!(pos.prepare_resize(len, GlyphPosition::default()));
        pos.commit_len(len);

        for i in 1..len {
            pos[i].x_offset = 40_000_000;
            pos[i].y_offset = 40_000_000;
            pos[i].set_attach_type(attach_type::MARK);
            pos[i].set_attach_chain(-1);
        }

        propagate_attachment_offsets(
            &mut pos,
            len,
            len - 1,
            Direction::LeftToRight,
            MAX_NESTING_LEVEL,
            None,
        );

        assert_eq!(pos[len - 1].x_offset, i32::MAX);
        assert_eq!(pos[len - 1].y_offset, 40_000_000);
    }
}
