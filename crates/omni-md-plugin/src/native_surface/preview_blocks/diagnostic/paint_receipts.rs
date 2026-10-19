//! 巨型代码段的有限准备与真实绘制收据；仅显式探针保存弱正文和固定几何。

use crate::native_surface::{MarkdownPresentationMode, WeakMarkdownDocument};
use objc2_foundation::NSRect;
use std::cell::RefCell;
use std::ops::Range;

pub(in crate::native_surface) const MAX_PROBE_CODE_REQUEST_UTF16: usize = 16_384;
const RECEIPT_CAPACITY: usize = 8;

/// body 标量只有在探针同时核验原活 Weak/body 与文档身份时才有意义，绝不单独充当活身份。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::native_surface) struct ProbeBlockKey {
    pub body_owner: usize,
    pub generation: u64,
    pub semantic_epoch: u64,
    pub table_epoch: u64,
    pub fold_epoch: u64,
    pub mode: MarkdownPresentationMode,
}

/// 主模块仅在原批次完整认证后交付这些已有值；不持 Native 或 canonical 强 owner。
#[derive(Clone)]
pub(in crate::native_surface) struct NativeMarkdownPreviewBlockProbeSnapshot {
    pub identity: WeakMarkdownDocument,
    pub key: ProbeBlockKey,
    pub viewport: NSRect,
    pub fragment_source: Range<usize>,
    pub semantic_window: Range<usize>,
    pub semantic_source: Range<usize>,
    pub kind: u8,
    pub frame: NSRect,
}

#[derive(Clone)]
pub(in crate::native_surface) struct CodePaintReceipt {
    pub sequence: u64,
    pub paint: NSRect,
    prepared: NativeMarkdownPreviewBlockProbeSnapshot,
}

/// 等待只保第一次原准备票和一次闭包；闭包不得持 Native 或正文强 owner。
struct PaintWait {
    snapshot: NativeMarkdownPreviewBlockProbeSnapshot,
    after: u64,
    terminal: Box<dyn FnOnce(bool)>,
}

/// 只保最近八个巨型代码段，覆盖合成动作；超旧收据被淘汰后只能缺证据，不能伪造通过。
struct PaintReceipts {
    prepared: [Option<NativeMarkdownPreviewBlockProbeSnapshot>; RECEIPT_CAPACITY],
    painted: [Option<CodePaintReceipt>; RECEIPT_CAPACITY],
    next_prepared: usize,
    next_painted: usize,
    sequence: u64,
    invalid: bool,
    saturated: bool,
    paint_calls: u64,
    paint_without_prepared: u64,
    reports: u8,
    wait: Option<PaintWait>,
}

impl Default for PaintReceipts {
    fn default() -> Self {
        Self {
            prepared: std::array::from_fn(|_| None),
            painted: std::array::from_fn(|_| None),
            next_prepared: 0,
            next_painted: 0,
            sequence: 0,
            invalid: false,
            saturated: false,
            paint_calls: 0,
            paint_without_prepared: 0,
            reports: 0,
            wait: None,
        }
    }
}

thread_local! {
    // Rust Weak 的复制与析构不调用 Native；槽借用内没有 getter、绘图、日志或通知回调。
    static RECEIPTS: RefCell<PaintReceipts> = RefCell::new(PaintReceipts::default());
}

fn same_prepared(
    left: &NativeMarkdownPreviewBlockProbeSnapshot,
    right: &NativeMarkdownPreviewBlockProbeSnapshot,
) -> bool {
    WeakMarkdownDocument::ptr_eq(&left.identity, &right.identity)
        && left.key == right.key
        && left.viewport == right.viewport
        && left.fragment_source == right.fragment_source
        && left.semantic_window == right.semantic_window
        && left.semantic_source == right.semantic_source
        && left.kind == right.kind
        && left.frame == right.frame
}

/// 原等待只接受完整同一准备；调用者不能把后继 viewport 或语义窗口补签到旧 Find。
pub(in crate::native_surface) fn code_snapshot_matches(
    left: &NativeMarkdownPreviewBlockProbeSnapshot,
    right: &NativeMarkdownPreviewBlockProbeSnapshot,
) -> bool {
    same_prepared(left, right)
}

fn new_snapshot_revokes(
    original: &NativeMarkdownPreviewBlockProbeSnapshot,
    next: &NativeMarkdownPreviewBlockProbeSnapshot,
) -> bool {
    original.key.body_owner == next.key.body_owner
        && (!WeakMarkdownDocument::ptr_eq(&original.identity, &next.identity)
            || original.key != next.key
            || original.viewport != next.viewport
            || (original.fragment_source == next.fragment_source && !same_prepared(original, next)))
}

/// 纯状态只消费一次原等待；返回的闭包必须由外层在结束TLS/RefCell借用之后调用。
fn take_wait_for_paint(
    state: &mut PaintReceipts,
    prepared: &NativeMarkdownPreviewBlockProbeSnapshot,
    sequence: u64,
) -> Option<(PaintWait, bool)> {
    let successful = state
        .wait
        .as_ref()
        .is_some_and(|wait| sequence > wait.after && same_prepared(&wait.snapshot, prepared));
    let revoked = state
        .wait
        .as_ref()
        .is_some_and(|wait| new_snapshot_revokes(&wait.snapshot, prepared));
    if successful || revoked {
        state.wait.take().map(|wait| (wait, successful))
    } else {
        None // 其他同时可见块或旧序号没有消费本原等待的权限。
    }
}

fn finite_positive(rect: NSRect) -> bool {
    let right = rect.origin.x + rect.size.width;
    let bottom = rect.origin.y + rect.size.height;
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
        right,
        bottom,
    ]
    .into_iter()
    .all(f64::is_finite)
        && rect.size.width > 0.0
        && rect.size.height > 0.0
        && right > rect.origin.x
        && bottom > rect.origin.y
}

fn contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    inner.start < inner.end && outer.start <= inner.start && inner.end <= outer.end
}

fn snapshot_is_valid(snapshot: &NativeMarkdownPreviewBlockProbeSnapshot) -> bool {
    snapshot.key.mode == MarkdownPresentationMode::InlinePreview
        && snapshot.fragment_source.len() > MAX_PROBE_CODE_REQUEST_UTF16
        && !snapshot.semantic_window.is_empty()
        && snapshot.semantic_window.len() <= MAX_PROBE_CODE_REQUEST_UTF16
        && contains(&snapshot.fragment_source, &snapshot.semantic_source)
        && contains(&snapshot.semantic_window, &snapshot.semantic_source)
        && finite_positive(snapshot.viewport)
        && finite_positive(snapshot.frame)
}

/// 只登记实际巨型 Code 项；短块和其他种类不消耗这个独立场景的固定容量。
pub(in crate::native_surface) fn prepared(snapshot: NativeMarkdownPreviewBlockProbeSnapshot) {
    if snapshot.kind != super::super::NativeMarkdownPreviewBlockKind::Code as u8
        || snapshot.fragment_source.len() <= MAX_PROBE_CODE_REQUEST_UTF16
    {
        return;
    }
    let cancelled = RECEIPTS.with_borrow_mut(|state| {
        if !snapshot_is_valid(&snapshot) {
            state.invalid = true; // 原事实非法只留下失败，不能截断范围或修正几何来造有效收据。
            return state.wait.take();
        }
        let revoked = state
            .wait
            .as_ref()
            .is_some_and(|wait| new_snapshot_revokes(&wait.snapshot, &snapshot));
        let cancelled = if revoked { state.wait.take() } else { None };
        let index = state.prepared.iter().position(|item| {
            item.as_ref().is_some_and(|item| {
                WeakMarkdownDocument::ptr_eq(&item.identity, &snapshot.identity)
                    && item.key == snapshot.key
                    && item.viewport == snapshot.viewport
                    && item.fragment_source == snapshot.fragment_source
            })
        });
        let index = index.unwrap_or_else(|| {
            let index = state.next_prepared;
            state.next_prepared = (index + 1) % RECEIPT_CAPACITY;
            index
        });
        state.prepared[index] = Some(snapshot);
        cancelled
    });
    if let Some(cancelled) = cancelled {
        (cancelled.terminal)(false); // 新几何/语义只撤旧票；结束TLS借用后才可投递原验证事件。
    }
}

/// 只能在实际背景画调用返回、原 key/viewport 仍成立后使用；正面积损伤必须属于该屏幕和块。
pub(in crate::native_surface) fn painted_current(
    identity: &WeakMarkdownDocument,
    key: ProbeBlockKey,
    viewport: NSRect,
    fragment_source: &Range<usize>,
    paint: NSRect,
    kind: u8,
) {
    if kind != super::super::NativeMarkdownPreviewBlockKind::Code as u8
        || fragment_source.len() <= MAX_PROBE_CODE_REQUEST_UTF16
    {
        return;
    }
    let terminal = RECEIPTS.with_borrow_mut(|state| {
        let Some(calls) = state.paint_calls.checked_add(1) else {
            state.saturated = true;
            return state.wait.take().map(|wait| (wait, false));
        };
        state.paint_calls = calls;
        let prepared = state
            .prepared
            .iter()
            .flatten()
            .find(|item| {
                WeakMarkdownDocument::ptr_eq(&item.identity, identity)
                    && item.key == key
                    && item.viewport == viewport
                    && item.fragment_source == *fragment_source
                    && item.kind == kind
            })
            .cloned();
        let Some(prepared) = prepared else {
            match state.paint_without_prepared.checked_add(1) {
                Some(count) => state.paint_without_prepared = count,
                None => state.saturated = true,
            }
            return if state.saturated {
                state.wait.take().map(|wait| (wait, false))
            } else {
                None // 已被淘汰或不是原准备，不能从 paint 自行重建语义证据。
            };
        };
        let within = |outer: NSRect| {
            outer.origin.x <= paint.origin.x
                && outer.origin.y <= paint.origin.y
                && paint.origin.x + paint.size.width <= outer.origin.x + outer.size.width
                && paint.origin.y + paint.size.height <= outer.origin.y + outer.size.height
        };
        if !finite_positive(paint) || !within(viewport) || !within(prepared.frame) {
            state.invalid = true;
            return state.wait.take().map(|wait| (wait, false));
        }
        let Some(sequence) = state.sequence.checked_add(1) else {
            state.saturated = true; // 序号失去精确性后永久失败，不回绕或按动作清零。
            return state.wait.take().map(|wait| (wait, false));
        };
        state.sequence = sequence;
        let index = state.next_painted;
        state.next_painted = (index + 1) % RECEIPT_CAPACITY;
        let terminal = take_wait_for_paint(state, &prepared, sequence);
        state.painted[index] = Some(CodePaintReceipt {
            sequence,
            paint,
            prepared,
        });
        terminal
    });
    if let Some((wait, successful)) = terminal {
        (wait.terminal)(successful); // 实际paint已返回且TLS借用结束；只排一次原验证，不执行验证本身。
    }
}

/// 原布局终点之后只绑定一次实际准备票；已有实际绘图立即交付，否则等原paint事件。
pub(in crate::native_surface) fn observe_code_paint(
    snapshot: NativeMarkdownPreviewBlockProbeSnapshot,
    after: u64,
    terminal: impl FnOnce(bool) + 'static,
) {
    let wait = PaintWait {
        snapshot,
        after,
        terminal: Box::new(terminal),
    };
    let (terminal, report) = RECEIPTS.with_borrow_mut(|state| {
        let matching = matching_receipt(state, &wait.snapshot, after);
        let ready = match matching {
            Ok(Some(_)) => Some(true),
            Err(_) => Some(false),
            Ok(None) if state.wait.is_some() => Some(false), // 不覆盖另一份原等待。
            Ok(None) => None,
        };
        let report = (state.reports < 32).then(|| {
            state.reports += 1;
            let prepared = state
                .prepared
                .iter()
                .flatten()
                .any(|item| same_prepared(item, &wait.snapshot));
            let after_count = state
                .painted
                .iter()
                .flatten()
                .filter(|item| item.sequence > after)
                .count();
            let owner_count = state
                .painted
                .iter()
                .flatten()
                .filter(|item| {
                    item.sequence > after
                        && item.prepared.key.body_owner == wait.snapshot.key.body_owner
                        && WeakMarkdownDocument::ptr_eq(
                            &item.prepared.identity,
                            &wait.snapshot.identity,
                        )
                })
                .count();
            let key_count = state
                .painted
                .iter()
                .flatten()
                .filter(|item| {
                    item.sequence > after
                        && item.prepared.key == wait.snapshot.key
                        && WeakMarkdownDocument::ptr_eq(
                            &item.prepared.identity,
                            &wait.snapshot.identity,
                        )
                })
                .count();
            let viewport_count = state
                .painted
                .iter()
                .flatten()
                .filter(|item| {
                    item.sequence > after
                        && item.prepared.key == wait.snapshot.key
                        && item.prepared.viewport == wait.snapshot.viewport
                        && WeakMarkdownDocument::ptr_eq(
                            &item.prepared.identity,
                            &wait.snapshot.identity,
                        )
                })
                .count();
            (
                state.sequence,
                state.paint_calls,
                state.paint_without_prepared,
                prepared,
                after_count,
                owner_count,
                key_count,
                viewport_count,
                ready,
            )
        });
        let terminal = if let Some(ready) = ready {
            Some((wait, ready))
        } else {
            state.wait = Some(wait);
            None
        };
        (terminal, report)
    });
    if let Some((
        sequence,
        calls,
        without_prepared,
        prepared,
        after_count,
        owner_count,
        key_count,
        viewport_count,
        ready,
    )) = report
    {
        println!(
            "native_code_paint_endpoint find_terminal=true after_sequence={after} current_sequence={sequence} long_paint_calls={calls} paint_without_prepared={without_prepared} exact_prepared={prepared} paints_after={after_count} same_body_document_after={owner_count} same_key_after={key_count} same_viewport_after={viewport_count} terminal={ready:?}"
        );
    }
    if let Some((wait, successful)) = terminal {
        (wait.terminal)(successful); // 日志和回调均不持收据槽借用，不触发额外getter或绘制。
    }
}

/// 关闭/模式切换撤销原body的观察与几何；累计计数及其他body的收据继续有效。
pub(in crate::native_surface) fn cancel_code_paint_for_body(owner: usize) {
    let (cancelled, prepared, painted) = RECEIPTS.with_borrow_mut(|state| {
        let cancelled = if state
            .wait
            .as_ref()
            .is_some_and(|wait| wait.snapshot.key.body_owner == owner)
        {
            state.wait.take()
        } else {
            None
        };
        // 原body撤权后不留会话Weak控制块或旧几何；固定栈槽先接走对象，借用外才析构。
        let prepared: [Option<NativeMarkdownPreviewBlockProbeSnapshot>; RECEIPT_CAPACITY] =
            std::array::from_fn(|index| {
                if state.prepared[index]
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.key.body_owner == owner)
                {
                    state.prepared[index].take()
                } else {
                    None
                }
            });
        let painted: [Option<CodePaintReceipt>; RECEIPT_CAPACITY] = std::array::from_fn(|index| {
            if state.painted[index]
                .as_ref()
                .is_some_and(|receipt| receipt.prepared.key.body_owner == owner)
            {
                state.painted[index].take()
            } else {
                None
            }
        });
        (cancelled, prepared, painted)
    });
    drop((prepared, painted));
    if let Some(cancelled) = cancelled {
        (cancelled.terminal)(false);
    }
}

/// 首 Native 前固定原动作序号；已有背景不能冒充接下来 Find 的实际绘制。
pub(in crate::native_surface) fn code_paint_sequence() -> Result<u64, String> {
    RECEIPTS.with_borrow(|state| {
        if state.invalid || state.saturated {
            return Err("代码背景收据出现非法几何、范围或序号溢出".to_owned());
        }
        Ok(state.sequence)
    })
}

/// 只找原动作后、同一实际准备批次的真实 paint；None 是缺证据，不能按 cache 存在签通过。
pub(in crate::native_surface) fn matching_code_paint(
    snapshot: &NativeMarkdownPreviewBlockProbeSnapshot,
    after: u64,
) -> Result<Option<CodePaintReceipt>, String> {
    RECEIPTS.with_borrow(|state| matching_receipt(state, snapshot, after))
}

fn matching_receipt(
    state: &PaintReceipts,
    snapshot: &NativeMarkdownPreviewBlockProbeSnapshot,
    after: u64,
) -> Result<Option<CodePaintReceipt>, String> {
    if state.invalid || state.saturated || !snapshot_is_valid(snapshot) {
        return Err("巨型代码背景的原准备或绘制收据无效".to_owned());
    }
    Ok(state
        .painted
        .iter()
        .flatten()
        .filter(|item| item.sequence > after && same_prepared(&item.prepared, snapshot))
        .max_by_key(|item| item.sequence)
        .cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::{NSPoint, NSSize};
    use std::cell::Cell;
    use std::rc::Rc;

    fn snapshot() -> NativeMarkdownPreviewBlockProbeSnapshot {
        NativeMarkdownPreviewBlockProbeSnapshot {
            identity: WeakMarkdownDocument::new(),
            key: ProbeBlockKey {
                body_owner: 7,
                generation: 9,
                semantic_epoch: 11,
                table_epoch: 13,
                fold_epoch: 15,
                mode: MarkdownPresentationMode::InlinePreview,
            },
            viewport: NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(600.0, 400.0)),
            fragment_source: 1_000..50_000,
            semantic_window: 2_000..3_000,
            semantic_source: 2_000..3_000,
            kind: super::super::super::NativeMarkdownPreviewBlockKind::Code as u8,
            frame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 4_000.0)),
        }
    }

    #[test]
    fn prepared_cache_and_old_paint_cannot_sign_current_action() {
        let snapshot = snapshot();
        let mut state = PaintReceipts::default();
        state.prepared[0] = Some(snapshot.clone());
        assert!(matching_receipt(&state, &snapshot, 0).unwrap().is_none());
        state.painted[0] = Some(CodePaintReceipt {
            sequence: 4,
            paint: snapshot.viewport,
            prepared: snapshot.clone(),
        });
        assert!(matching_receipt(&state, &snapshot, 3).unwrap().is_some());
        assert!(matching_receipt(&state, &snapshot, 4).unwrap().is_none());
    }

    #[test]
    fn newer_owner_key_source_or_viewport_never_replaces_original_receipt() {
        let original = snapshot();
        let mut state = PaintReceipts::default();
        state.painted[0] = Some(CodePaintReceipt {
            sequence: 4,
            paint: original.viewport,
            prepared: original.clone(),
        });
        let mut changed = original.clone();
        changed.key.body_owner += 1;
        assert!(matching_receipt(&state, &changed, 0).unwrap().is_none());
        changed = original.clone();
        changed.key.semantic_epoch += 1;
        assert!(matching_receipt(&state, &changed, 0).unwrap().is_none());
        changed = original.clone();
        changed.viewport.origin.y += 1.0;
        assert!(matching_receipt(&state, &changed, 0).unwrap().is_none());
        changed = original.clone();
        changed.semantic_source.end -= 1;
        assert!(matching_receipt(&state, &changed, 0).unwrap().is_none());
        state.saturated = true;
        assert!(matching_receipt(&state, &original, 0).is_err());
    }

    #[test]
    fn giant_fragment_requires_finite_real_semantic_window_and_positive_viewport() {
        let original = snapshot();
        assert!(snapshot_is_valid(&original));
        let mut invalid = original.clone();
        invalid.semantic_window = 0..50_000;
        assert!(!snapshot_is_valid(&invalid));
        invalid = original.clone();
        invalid.semantic_source = 4_000..5_000;
        assert!(!snapshot_is_valid(&invalid));
        invalid = original;
        invalid.viewport.size.height = 0.0;
        assert!(!snapshot_is_valid(&invalid));
    }

    #[test]
    fn actual_matching_terminal_consumes_once_and_callback_runs_without_state_borrow() {
        let original = snapshot();
        let state = Rc::new(RefCell::new(PaintReceipts::default()));
        let calls = Rc::new(Cell::new(0));
        let callback_state = Rc::clone(&state);
        let callback_calls = Rc::clone(&calls);
        state.borrow_mut().wait = Some(PaintWait {
            snapshot: original.clone(),
            after: 3,
            terminal: Box::new(move |successful| {
                assert!(successful);
                assert!(callback_state.try_borrow_mut().is_ok());
                callback_calls.set(callback_calls.get() + 1);
            }),
        });
        assert!(take_wait_for_paint(&mut state.borrow_mut(), &original, 3).is_none());
        assert_eq!(calls.get(), 0); // 旧序号即使同一viewport也不能把等待升级为完成。
        let (wait, successful) =
            take_wait_for_paint(&mut state.borrow_mut(), &original, 4).unwrap();
        assert_eq!(calls.get(), 0); // 纯状态先移动回调，不在借用内执行它。
        (wait.terminal)(successful);
        assert_eq!(calls.get(), 1);
        assert!(take_wait_for_paint(&mut state.borrow_mut(), &original, 5).is_none());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn later_viewport_cancels_original_once_instead_of_resigning_the_wait() {
        let original = snapshot();
        let mut state = PaintReceipts::default();
        let result = Rc::new(Cell::new(None));
        let callback_result = Rc::clone(&result);
        state.wait = Some(PaintWait {
            snapshot: original.clone(),
            after: 3,
            terminal: Box::new(move |successful| callback_result.set(Some(successful))),
        });
        let mut unrelated = original.clone();
        unrelated.key.body_owner += 1;
        assert!(take_wait_for_paint(&mut state, &unrelated, 4).is_none());
        let mut later = original.clone();
        later.viewport.origin.y += 20.0;
        let (wait, successful) = take_wait_for_paint(&mut state, &later, 4).unwrap();
        assert!(!successful);
        assert!(same_prepared(&wait.snapshot, &original)); // 失败仍保留原票，从未把新viewport写回等待。
        (wait.terminal)(successful);
        assert_eq!(result.get(), Some(false));
        assert!(state.wait.is_none());
        assert!(take_wait_for_paint(&mut state, &original, 5).is_none());
    }
}
