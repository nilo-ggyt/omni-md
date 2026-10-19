use super::*;

fn regions(ranges: &[Range<usize>]) -> Vec<MarkdownFoldRegion> {
    ranges
        .iter()
        .map(|range| MarkdownFoldRegion {
            header_line: 17, // 旧行号刻意固定；纯mapper不能假装知道新文档的行布局。
            hidden_utf16_range: range.clone(),
        })
        .collect()
}

fn one(range: Range<usize>) -> Vec<Range<usize>> {
    std::iter::once(range).collect()
}

fn change(start: usize, end: usize, inserted: usize) -> MarkdownFoldChange {
    MarkdownFoldChange {
        old_utf16_range: start..end,
        inserted_utf16_len: inserted,
    }
}

fn mapped(
    folds: &[Range<usize>],
    old_length: usize,
    changes: &[MarkdownFoldChange],
    delete: bool,
    head: Option<usize>,
) -> Vec<Range<usize>> {
    let mut mapper = MarkdownFoldAnchorMapping::new(old_length, &regions(folds), delete).unwrap();
    let mut new_length = old_length;
    for change in changes {
        new_length = new_length - change.old_utf16_range.len() + change.inserted_utf16_len;
        mapper.push(change.clone()).unwrap();
    }
    mapper.finish(new_length, head).unwrap()
}

#[test]
fn insertions_at_both_edges_are_noninclusive_and_same_position_order_is_preserved() {
    let folds = std::iter::once(5..10).collect::<Vec<_>>();
    assert_eq!(
        mapped(
            &folds,
            20,
            &[
                change(5, 5, 2),
                change(5, 5, 3),
                change(10, 10, 4),
                change(10, 10, 1),
            ],
            false,
            None,
        ),
        one(10..15),
    );
}

#[test]
fn adjacent_replacements_keep_their_true_boundary_instead_of_becoming_an_envelope() {
    // 原位置5同时是前一个replacement终点和后一个起点，start(+1)仍落后一个新起点。
    assert_eq!(
        mapped(
            &one(5..9),
            12,
            &[change(0, 5, 2), change(5, 7, 4)],
            false,
            None
        ),
        one(2..8),
    );
    assert_eq!(
        mapped(
            &one(1..5),
            12,
            &[change(0, 5, 2), change(5, 7, 4)],
            false,
            None
        ),
        Vec::<Range<usize>>::new(),
    );
}

#[test]
fn separated_replace_all_changes_preserve_untouched_fold_between_them() {
    // 用首尾envelope代替真实两项会把这个中间fold误判为delete触碰。
    assert_eq!(
        mapped(
            &one(5..15),
            30,
            &[change(0, 1, 2), change(20, 21, 3)],
            true,
            None
        ),
        one(6..16),
    );
}

#[test]
fn delete_user_event_uses_strict_old_coordinate_intersection_even_for_zero_width() {
    for edit in [
        change(5, 5, 1),
        change(15, 15, 1),
        change(0, 5, 1),
        change(15, 18, 1),
    ] {
        assert_eq!(mapped(&one(5..15), 20, &[edit], true, None).len(), 1);
    }
    for edit in [
        change(10, 10, 1),
        change(10, 10, 0),
        change(4, 6, 0),
        change(14, 16, 0),
    ] {
        assert!(mapped(&one(5..15), 20, &[edit], true, None).is_empty());
    }
    // 普通输入事件只映射；不能把任意内部编辑都当作delete用户事件主动展开。
    assert_eq!(
        mapped(&one(5..15), 20, &[change(10, 10, 2)], false, None),
        one(5..17)
    );
}

#[test]
fn explicit_selection_uses_only_main_head_in_new_coordinates() {
    for (anchor, head) in [(0, 10), (19, 10)] {
        assert!(anchor != head);
        assert!(mapped(&one(5..15), 20, &[], false, Some(head)).is_empty());
    }
    for head in [0, 5, 15, 20] {
        // 选区anchor即使位于内部，调用方也只能传实际main.head；方向不改变这个接口。
        let _selection_anchor = 10;
        assert_eq!(mapped(&one(5..15), 20, &[], false, Some(head)), one(5..15));
    }
    assert_eq!(mapped(&one(5..15), 20, &[], false, None), one(5..15));
    // 先映射到7..17，再判断显式新head；不能拿新head对旧范围做展开判断。
    assert_eq!(
        mapped(&one(5..15), 20, &[change(0, 0, 2)], false, Some(6)),
        one(7..17)
    );
}

#[test]
fn collapsed_or_reversed_mapped_ranges_are_removed() {
    assert!(mapped(&one(5..15), 20, &[change(4, 16, 0)], false, None).is_empty());
    assert!(mapped(&one(5..15), 20, &[change(4, 16, 3)], false, None).is_empty());
}

#[test]
fn utf16_lengths_do_not_depend_on_rust_bytes_or_grapheme_count() {
    let inserted = "中😀".encode_utf16().count();
    assert_eq!(inserted, 3);
    assert_eq!(
        mapped(&one(2..7), 10, &[change(0, 2, inserted)], false, None),
        one(3..8)
    );
}

#[test]
fn maximum_is_exact_and_invalid_requests_never_modify_original_folds() {
    let folds = regions(&(0..256).map(|at| at * 2..at * 2 + 1).collect::<Vec<_>>());
    assert_eq!(
        MarkdownFoldAnchorMapping::new(512, &folds, false)
            .unwrap()
            .finish(512, None)
            .unwrap()
            .len(),
        256,
    );
    let mut too_many = folds.clone();
    too_many.push(folds[0].clone());
    assert!(matches!(
        MarkdownFoldAnchorMapping::new(512, &too_many, false),
        Err(MarkdownFoldMappingError::TooManyAnchors)
    ));
    let original = folds.clone();
    let mut mapper = MarkdownFoldAnchorMapping::new(512, &folds, false).unwrap();
    mapper.push(change(10, 12, 1)).unwrap();
    assert_eq!(
        mapper.push(change(11, 13, 0)),
        Err(MarkdownFoldMappingError::ChangesOutOfOrder)
    );
    assert_eq!(
        mapper.push(change(900, 901, 0)),
        Err(MarkdownFoldMappingError::ChangesOutOfOrder)
    );
    assert_eq!(
        mapper.finish(511, None),
        Err(MarkdownFoldMappingError::ChangesOutOfOrder)
    );
    assert_eq!(folds, original);
}

#[test]
fn invalid_lengths_ranges_heads_and_arithmetic_fail_closed() {
    for range in [5..5, Range { start: 8, end: 5 }, 1..21] {
        assert!(matches!(
            MarkdownFoldAnchorMapping::new(20, &regions(&[range]), false),
            Err(MarkdownFoldMappingError::InvalidAnchor)
        ));
    }
    let mapper = MarkdownFoldAnchorMapping::new(20, &regions(&one(5..15)), false).unwrap();
    assert_eq!(
        mapper.finish(21, None),
        Err(MarkdownFoldMappingError::NewLengthMismatch)
    );
    let mapper = MarkdownFoldAnchorMapping::new(20, &regions(&one(5..15)), false).unwrap();
    assert_eq!(
        mapper.finish(20, Some(21)),
        Err(MarkdownFoldMappingError::MainHeadOutsideDocument)
    );
    let mut mapper = MarkdownFoldAnchorMapping::new(20, &[], false).unwrap();
    assert_eq!(
        mapper.push(change(0, 21, 0)),
        Err(MarkdownFoldMappingError::InvalidChange)
    );
    let mut mapper = MarkdownFoldAnchorMapping::new(20, &[], false).unwrap();
    mapper.push(change(0, 0, usize::MAX)).unwrap();
    assert_eq!(
        mapper.finish(20, None),
        Err(MarkdownFoldMappingError::CoordinateOverflow)
    );
}

#[test]
fn large_true_change_stream_keeps_constant_anchor_storage_and_middle_gap_fold() {
    let folds = regions(&one(100_001..100_002));
    let mut mapper = MarkdownFoldAnchorMapping::new(200_000, &folds, true).unwrap();
    let capacities = (mapper.anchors.capacity(), mapper.endpoints.capacity());
    for at in 0..100_000 {
        // 两个被改字符之间保留一个真实unit间隙；流在任何时点都没有Vec<edit/match>。
        mapper.push(change(at * 2, at * 2 + 1, 2)).unwrap();
    }
    assert_eq!(
        (mapper.anchors.capacity(), mapper.endpoints.capacity()),
        capacities
    );
    assert_eq!(mapper.finish(300_000, None).unwrap(), one(150_002..150_003));
}

// 独立逐位置oracle忠实采用旧CM state6.7.1 mapPos的section规则；它故意不共用生产端点sweep。
fn reference_position(position: usize, assoc: isize, changes: &[MarkdownFoldChange]) -> usize {
    let (mut old, mut new) = (0, 0);
    for change in changes {
        let range = &change.old_utf16_range;
        if position < range.start {
            return new + position - old;
        }
        new += range.start - old;
        if range.end > position || range.is_empty() && range.end == position && assoc < 0 {
            return if position == range.start || assoc < 0 {
                new
            } else {
                new + change.inserted_utf16_len
            };
        }
        new += change.inserted_utf16_len;
        old = range.end;
    }
    new + position - old
}

#[test]
fn endpoint_sweep_matches_independent_position_oracle_for_adjacent_and_zero_width_changes() {
    let old_length = 9;
    let folds = (0..old_length)
        .flat_map(|from| (from + 1..=old_length).map(move |to| from..to))
        .collect::<Vec<_>>();
    for first_start in 0..=old_length {
        for first_end in first_start..=old_length {
            for second_start in first_end..=old_length {
                for second_end in second_start..=old_length {
                    for first_inserted in 0..=2 {
                        let changes = [
                            change(first_start, first_end, first_inserted),
                            change(second_start, second_end, 2),
                        ];
                        for delete in [false, true] {
                            let expected = folds
                                .iter()
                                .filter_map(|fold| {
                                    let from = reference_position(fold.start, 1, &changes);
                                    let to = reference_position(fold.end, -1, &changes);
                                    let touched = delete
                                        && changes.iter().any(|change| {
                                            fold.start < change.old_utf16_range.end
                                                && fold.end > change.old_utf16_range.start
                                        });
                                    (!touched && from < to).then_some(from..to)
                                })
                                .collect::<Vec<_>>();
                            assert_eq!(
                                mapped(&folds, old_length, &changes, delete, None),
                                expected,
                                "changes={changes:?},delete={delete}"
                            );
                        }
                    }
                }
            }
        }
    }
}
