//! 独立有限像素oracle核分步游标；超4096glyph不能永久Pending或截掉不可见尾部。

use super::*;
use objc2_core_foundation::CGPoint;
use omni_markdown_core::MarkdownComposedLinePartKind;
use omni_md_plugin::native_surface::{
    paged_glyph_paint::{
        NativeMarkdownPagedGlyphOrigin, NativeMarkdownPagedGlyphPaintRequest, paint_composed_glyphs,
    },
    paged_line_viewport::{
        NativeMarkdownComposedViewportPaintBudget as Budget,
        NativeMarkdownComposedViewportPaintOutcome as Outcome,
        NativeMarkdownComposedViewportPaintSession as Session,
    },
};

fn first_glyph(session: Session, current: &dyn Fn() -> bool) -> Session {
    let step = session
        .step(
            Budget {
                glyphs: 1,
                metadata_records: 32,
            },
            current,
        )
        .unwrap();
    assert_eq!(step.work.glyphs, 1);
    assert!(step.work.metadata_records <= 32 && step.work.batches == 1);
    match step.outcome {
        Outcome::Pending(session) => session,
        Outcome::Complete(_) => panic!("本夹具大于1glyph，不能提前签完整候选"),
    }
}

pub(super) fn matches_finite_oracle(
    window: &Rc<NativeMarkdownComposedViewport>,
    expected: &[u8],
    expected_glyphs: usize,
    current: &dyn Fn() -> bool,
) {
    let baseline_claims = Rc::strong_count(window);
    let mut actual = Canvas::new();
    let session = window
        .clone()
        .begin_paint(actual.context.clone(), current)
        .unwrap();
    let zero = session
        .step(
            Budget {
                glyphs: 0,
                metadata_records: 0,
            },
            current,
        )
        .unwrap();
    assert_eq!(
        (
            zero.work.glyphs,
            zero.work.batches,
            zero.work.metadata_records
        ),
        (0, 0, 0)
    );
    assert_eq!(actual.ink(), 0); // 零配额实际无draw，不是只把统计隐藏为零。
    let mut session = match zero.outcome {
        Outcome::Pending(session) => session,
        Outcome::Complete(_) => panic!("零配额不能跳过非空原窗口"),
    };
    let mut work_glyphs = 0;
    let mut work_records = 0;
    let mut work_batches = 0;
    let mut calls = 0;
    loop {
        assert!(calls < 1000, "真实有限窗口没有终止，不允许无限重试制造完成");
        let budget = if calls == 0 {
            Budget {
                glyphs: 1,
                metadata_records: 3,
            }
        } else {
            Budget::default()
        };
        let step = session.step(budget, current).unwrap();
        assert!(
            step.work.glyphs <= budget.glyphs
                && step.work.metadata_records <= budget.metadata_records
        );
        work_glyphs += step.work.glyphs;
        work_records += step.work.metadata_records;
        work_batches += step.work.batches;
        calls += 1;
        match step.outcome {
            Outcome::Pending(next) => session = next,
            Outcome::Complete(completion) => {
                let receipt = completion.receipt();
                let work = completion.work();
                assert_eq!(receipt.glyphs, expected_glyphs);
                assert_eq!(
                    (work.glyphs, work.batches, work.metadata_records),
                    (work_glyphs, work_batches, work_records)
                );
                assert_eq!((receipt.batches, receipt.missing_glyphs), (work_batches, 0));
                assert!(completion.permits(window.key(), &actual.context));
                let other = Canvas::new();
                assert!(!completion.permits(window.key(), &other.context));
                let mut foreign = window.key().clone();
                foreign.request.geometry_epoch += 1;
                assert!(!completion.permits(&foreign, &actual.context));
                assert_eq!(Rc::strong_count(window), baseline_claims); // 完成只保弱key，不延长窗口/Font/Source。
                break;
            }
        }
    }
    assert!(calls > 1 && work_glyphs == expected_glyphs);
    actual.ink(); // flush真实原context后逐RGBA比较，不能以近似ink数或只比较前缀替代。
    assert_eq!(actual.pixels.as_slice(), expected);

    // 已draw的原会话失权后真实析构；不把失败后的重试或较新nonce补签成成功。
    let revoked = Canvas::new();
    let session = first_glyph(
        window
            .clone()
            .begin_paint(revoked.context.clone(), current)
            .unwrap(),
        current,
    );
    let error = session.step(Budget::default(), &|| false).unwrap_err();
    assert!(matches!(
        error.cause,
        NativeMarkdownComposedViewportPaintCause::Window(Error::Revoked)
    ));
    assert!(error.native_draw_returned);
    assert_eq!(Rc::strong_count(window), baseline_claims);

    // 原位图被其他入口改变时不得继续沿旧pen写入；消费式失败也归还context/window强边。
    let changed = Canvas::new();
    let session = first_glyph(
        window
            .clone()
            .begin_paint(changed.context.clone(), current)
            .unwrap(),
        current,
    );
    CGContext::translate_ctm(Some(&changed.context), 1.0, 0.0);
    let error = session.step(Budget::default(), current).unwrap_err();
    assert!(matches!(
        error.cause,
        NativeMarkdownComposedViewportPaintCause::ContextChanged
    ));
    assert!(error.native_draw_returned);
    assert_eq!(Rc::strong_count(window), baseline_claims);

    let excessive = Canvas::new();
    let session = window
        .clone()
        .begin_paint(excessive.context.clone(), current)
        .unwrap();
    let error = session
        .step(
            Budget {
                glyphs: 257,
                metadata_records: 32,
            },
            current,
        )
        .unwrap_err();
    assert!(matches!(
        error.cause,
        NativeMarkdownComposedViewportPaintCause::InvalidBudget
    ));
    assert!(!error.native_draw_returned);
    assert_eq!(Rc::strong_count(window), baseline_claims);
    let cancelled = window
        .clone()
        .begin_paint(excessive.context.clone(), current)
        .unwrap();
    drop(cancelled);
    assert_eq!(Rc::strong_count(window), baseline_claims);
    println!(
        "native_composed_paint_steps glyphs={work_glyphs} batches={work_batches} metadata_records={work_records} calls={calls} rgba_exact=true zero_budget_no_draw=true real_revocation=true context_changed_rejected=true window_claims_returned=true formal=false"
    );
}

pub(super) fn over_sync_limit(
    window: &Rc<NativeMarkdownComposedViewport>,
    output: &Rc<RefCell<MarkdownComposedLines>>,
    font: &Rc<NativeMarkdownLineFont>,
    current: &dyn Fn() -> bool,
) {
    let mut oracle = Canvas::new();
    let rows = handles(output);
    assert_eq!(rows.len(), 1); // 此固定5000glyph夹具只核超同步门，不把全段oracle引入生产。
    let line = &rows[0];
    let metrics = font.metrics().unwrap();
    let frame = window.line_frame(0).unwrap();
    let baseline = CGPoint::new(
        16.0,
        frame.origin.y + frame.size.height - metrics.ascent - metrics.leading / 2.0,
    );
    let mut glyphs = 0;
    for ordinal in 0..line.facts().parts {
        let part = output.borrow_mut().part(line, ordinal).unwrap().unwrap();
        if part.facts().kind != MarkdownComposedLinePartKind::Glyphs {
            continue; // 原控制项只贡献布局；本oracle也不能虚构glyph作为预期。
        }
        let facts = part.facts();
        let (mut x, mut y) = (facts.origin_x_26_6, facts.origin_y_26_6);
        let mut start = 0;
        while start < facts.glyphs {
            let end = (start + 256).min(facts.glyphs);
            let painted = paint_composed_glyphs(
                output,
                &part,
                NativeMarkdownPagedGlyphPaintRequest {
                    source: &window.key().source,
                    font_authority: &font.native().core_font().authority(),
                    native_font: font.native(),
                    glyph_range: start..end,
                    origin: NativeMarkdownPagedGlyphOrigin {
                        baseline,
                        pen_x_26_6: x,
                        pen_y_26_6: y,
                    },
                },
                &oracle.context,
                current,
            )
            .unwrap();
            x = painted.final_pen_x_26_6;
            y = painted.final_pen_y_26_6;
            glyphs += end - start;
            start = end;
        }
        assert_eq!(
            facts.origin_x_26_6.checked_add(facts.x_advance_26_6),
            Some(x)
        );
        assert_eq!(
            facts.origin_y_26_6.checked_add(facts.y_advance_26_6),
            Some(y)
        );
    }
    assert!(glyphs > 4096 && oracle.ink() > 0);
    matches_finite_oracle(window, &oracle.pixels, glyphs, current);
}
