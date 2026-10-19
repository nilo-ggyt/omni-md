//! 几何永久撤票的纯边界；不创建原生视图、不提供假Ready坐标。

use super::*;

#[test]
fn exhausted_geometry_invalidates_the_previous_max_ticket_without_faking_closed() {
    let epoch = Cell::new(u64::MAX - 1);
    let failed = Cell::new(false);
    advance_geometry(&epoch, &failed).unwrap();
    assert!(geometry_matches(&epoch, &failed, u64::MAX));
    assert!(matches!(
        advance_geometry(&epoch, &failed),
        Err(NativeMarkdownPagedTextViewError::CounterExhausted)
    ));
    assert_eq!(epoch.get(), u64::MAX);
    assert!(!geometry_matches(&epoch, &failed, u64::MAX));
    assert!(matches!(
        advance_geometry(&epoch, &failed),
        Err(NativeMarkdownPagedTextViewError::Revoked)
    ));
}
