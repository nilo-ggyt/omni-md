// Copyright 2015 Google Inc. All rights reserved.
// 本地预算/持久页测试，MIT条款见LICENSE。
use super::*;

fn limits() -> StructuralSpineLimits {
    StructuralSpineLimits {
        max_depth: MAX_STRUCTURAL_DEPTH,
        max_live_page_bytes: MAX_SPINE_PAGE_BYTES,
        max_page_allocations: 1_000_000,
        max_role_work: 16_000_000,
    }
}
fn entry(index: usize) -> StructuralEntry {
    StructuralEntry {
        kind: StructuralKind::Item { indent: index % 9 },
        source_start: index,
    }
}

#[test]
fn whole_document_shared_checkpoints_do_not_copy_full_prefix_on_every_push() {
    let mut spine = StructuralSpineSnapshot::new(limits()).unwrap();
    let mut checkpoints = Vec::new();
    for index in 0..4096 {
        // 每次保存旧snapshot，强迫真实COW路径；不能靠唯一Arc快路掩盖depth²分配。
        checkpoints.push(spine.clone());
        spine.push(entry(index)).unwrap();
    }
    let (live, allocations, work) = spine.payload_counts();
    assert_eq!(allocations, 4096);
    assert_eq!(live, 4096 * PAGE_PAYLOAD_BYTES);
    assert!(work <= 4096 * PAGE_ENTRIES);
    for (index, checkpoint) in checkpoints.iter().enumerate().step_by(37) {
        assert_eq!(checkpoint.len(), index);
        if index > 0 {
            assert_eq!(checkpoint.entry(index - 1), Some(entry(index - 1)));
        }
    }
    // 释放历史checkpoint之后只有当前128页仍活，预算不把已nil历史计作永久增长。
    drop(checkpoints);
    assert_eq!(spine.payload_counts().0, 128 * PAGE_PAYLOAD_BYTES);
}

#[test]
fn pop_does_not_allocate_and_new_push_preserves_old_prefix() {
    let mut spine = StructuralSpineSnapshot::new(limits()).unwrap();
    for index in 0..100 {
        spine.push(entry(index)).unwrap();
    }
    let old = spine.clone();
    let allocations = spine.payload_counts().1;
    for index in (1..100).rev() {
        assert_eq!(spine.pop().unwrap(), Some(entry(index)));
    }
    assert_eq!(spine.payload_counts().1, allocations);
    spine.push(entry(999)).unwrap();
    assert_eq!(spine.entry(1), Some(entry(999)));
    assert_eq!(old.entry(1), Some(entry(1)));
    assert_eq!(old.entry(99), Some(entry(99)));
}

#[test]
fn maximum_depth_root_first_walk_uses_fixed_pointer_stack_and_stops_at_first_false() {
    let mut spine = StructuralSpineSnapshot::new(limits()).unwrap();
    for index in 0..MAX_STRUCTURAL_DEPTH {
        spine.push(entry(index)).unwrap();
    }
    spine.charge_role_visit().unwrap();
    let mut index = 0;
    spine.visit_roles(|role| {
        assert_eq!(role, entry(index).role());
        index += 1;
        true
    });
    assert_eq!(index, MAX_STRUCTURAL_DEPTH);
    let mut visited = 0;
    spine.visit_roles(|_| {
        visited += 1;
        visited != 17
    });
    assert_eq!(visited, 17);
    assert_eq!(
        spine.push(entry(MAX_STRUCTURAL_DEPTH)),
        Err(StructuralSpineError::Depth)
    );
    assert!(std::mem::size_of::<[Option<&SpinePage>; MAX_SPINE_PAGES]>() <= 4096);
}

#[test]
fn capacity_and_fuel_fail_before_allocation_and_checkpoint_keeps_budget_live() {
    let mut config = limits();
    config.max_live_page_bytes = PAGE_PAYLOAD_BYTES;
    let mut spine = StructuralSpineSnapshot::new(config).unwrap();
    spine.push(entry(0)).unwrap();
    let checkpoint = spine.clone();
    assert_eq!(
        spine.push(entry(1)),
        Err(StructuralSpineError::LivePageBytes)
    );
    assert_eq!(spine.len(), 1);
    assert_eq!(spine.payload_counts().1, 1);
    drop(checkpoint);
    spine.push(entry(1)).unwrap();
    assert_eq!(spine.payload_counts().1, 1); // 唯一页原位修改，未领新allocation。
    let mut config = limits();
    config.max_page_allocations = 1;
    let mut fuel = StructuralSpineSnapshot::new(config).unwrap();
    fuel.push(entry(0)).unwrap();
    let old = fuel.clone();
    assert_eq!(
        fuel.push(entry(1)),
        Err(StructuralSpineError::AllocationFuel)
    );
    assert_eq!(fuel.payload_counts().0, PAGE_PAYLOAD_BYTES);
    assert!(fuel.shares_tail(&old));
}

#[test]
fn near_top_mutation_is_bounded_and_does_not_change_old_snapshot() {
    let mut spine = StructuralSpineSnapshot::new(limits()).unwrap();
    for index in 0..33 {
        spine.push(entry(index)).unwrap();
    }
    let old = spine.clone();
    let allocations = spine.payload_counts().1;
    spine.update_near_top(1, entry(888)).unwrap();
    assert_eq!(spine.entry(31), Some(entry(888)));
    assert_eq!(old.entry(31), Some(entry(31)));
    assert_eq!(spine.payload_counts().1 - allocations, 2);
    assert_eq!(
        spine.update_near_top(2, entry(999)),
        Err(StructuralSpineError::InvalidIndex)
    );
}
