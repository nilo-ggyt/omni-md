//! 原正文或真实表格格的装饰续体；每个 line、part 与 selection 项支付同一 metadata 预算。
//!
//! 同步代码背景与选区入口保留作完整像素对照，异步正文与Grid共用这里的原窗口循环。
//! 背景先于所有选区，选区先于所有字形；暂停不会改变这个像素覆盖顺序。
//! 游标没有自己的context，唯一Scene Paint一直持有未发布候选；失败直接丢弃候选。
//! 原Line/Part与display映射都已封存，跨量子只保存有限下标，不创建第二份排版结果。
//! 字形、源文字和选中内容不会进入本文件的状态；metadata是实际遍历项而非整格折算。

use super::*;
use omni_markdown_core::MarkdownComposedLineAffinity;

/// 仅保原窗口的循环位置和一个已映射范围；没有正文、context claim 或第二套布局对象。
pub(in crate::native_surface) struct NativeMarkdownComposedViewportDecorationCursor {
    cursor: Decoration,
    // 背景任务没有canonical选区；只有selection任务携带同原格范围求交所需的两个端点。
    canonical: Option<Range<usize>>,
    // 当前一个选区完成前保留其display端点，下一选区覆盖它，不收集全部映射结果。
    selected: Option<Range<usize>>,
    native_draw_returned: bool,
}

#[derive(Clone, Copy)]
// 各项直接对应原双层line/part循环及外层selection循环，结束事实本身也支付一次遍历项。
// 阶段切换不签发Glyph完成票，只有Scene的后续真实字形会话能完成整张候选。
enum Decoration {
    BackgroundLine(usize),
    BackgroundPart {
        line: usize,
        part: usize,
    },
    SelectionEntry(usize),
    SelectionLine {
        selection: usize,
        line: usize,
    },
    SelectionPart {
        selection: usize,
        line: usize,
        part: usize,
    },
    Done,
}

pub(in crate::native_surface) struct NativeMarkdownComposedViewportDecorationStep {
    pub metadata_records: usize,
    pub complete: bool,
}

impl NativeMarkdownComposedViewportDecorationCursor {
    /// 代码背景只消费原封存行/part，构造不进入 Native、不取得额外来源或字体。
    pub(in crate::native_surface) fn backgrounds() -> Self {
        Self {
            cursor: Decoration::BackgroundLine(0),
            canonical: None,
            selected: None,
            native_draw_returned: false,
        }
    }

    /// 已封存窗口范围只与 canonical 选区求交；正文不换坐标，格仍由同一 display Source 映射。
    pub(in crate::native_surface) fn selections(
        canonical: Range<usize>,
        selections: &[Range<usize>],
    ) -> Self {
        // 原Input已经给出有序不相交范围；只跳过严格位于本格之前的metadata，不读取那些正文。
        // 当前格之后的首范围会在SelectionEntry结束任务，不逐项扫全文Find结果。
        let first = selections.partition_point(|selection| selection.end <= canonical.start);
        Self {
            cursor: Decoration::SelectionEntry(first),
            canonical: Some(canonical),
            selected: None,
            native_draw_returned: false,
        }
    }
}

impl NativeMarkdownComposedViewport {
    /// 只推进调用者剩余的 metadata 项；任何失败使外层丢弃整张未发布画布，不重放原 draw。
    pub(in crate::native_surface) fn advance_decorations(
        &self,
        cursor: &mut NativeMarkdownComposedViewportDecorationCursor,
        context: &CGContext,
        selections: &[Range<usize>],
        selection_color: [f64; 4],
        budget: usize,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportDecorationStep,
        NativeMarkdownComposedViewportPaintError,
    > {
        let result = (|| -> std::result::Result<_, NativeMarkdownComposedViewportPaintCause> {
            self.require(current)?;
            // 每次调用配平自己的颜色和背景clip，游标不会把图形状态留给另一个格。
            // 背景由未裁剪的真实行/part框绘制，因此与同步背景入口一样领取窗口clip。
            // 选区在下面已把矩形裁到同一clip；不能再把分数像素边缘的coverage乘一次。
            // 外层边线阶段在背景任务完成后才建立路径，ClipToRect不会清掉待提交边线。
            CGContext::save_g_state(Some(context));
            let restore = Restore(context);
            if cursor.canonical.is_none() {
                CGContext::clip_to_rect(Some(context), self.clip());
            }
            let mut records = 0;
            while records < budget && !matches!(cursor.cursor, Decoration::Done) {
                self.require(current)?;
                records += 1; // 下列每次只消费一个真实循环项；没有隐藏的全格遍历。
                match cursor.cursor {
                    Decoration::BackgroundLine(line) => {
                        // 行背景有固定数量的矩形，行内parts必须交下一阶段逐个消费。
                        // overscan只保metadata；真正离屏的行不产生本候选像素，也不需要进入其parts。
                        if let Some(actual) = self.lines.get(line) {
                            if self.visible(actual.frame) {
                                self.line_background(
                                    actual,
                                    context,
                                    &mut cursor.native_draw_returned,
                                    current,
                                )?;
                                cursor.cursor = Decoration::BackgroundPart { line, part: 0 };
                            } else {
                                cursor.cursor = Decoration::BackgroundLine(line + 1);
                            }
                        } else {
                            cursor.cursor = Decoration::Done;
                        }
                    }
                    Decoration::BackgroundPart { line, part } => {
                        // 每次最多访问一个原part；不论它是否inline-code都支付相同的查阅费用。
                        // 暂停位置只在一次完整矩形操作之后前移，不能重放已经返回的Native fill。
                        if let Some(actual) = self.lines[line].parts.get(part) {
                            self.part_background(
                                &self.lines[line],
                                actual,
                                context,
                                &mut cursor.native_draw_returned,
                                current,
                            )?;
                            cursor.cursor = Decoration::BackgroundPart {
                                line,
                                part: part + 1,
                            };
                        } else {
                            cursor.cursor = Decoration::BackgroundLine(line + 1);
                        }
                    }
                    Decoration::SelectionEntry(selection) => {
                        // canonical范围先和原窗口求交，正文保持原端点，格的隐藏语法沿同一目录映射。
                        // 这一步不访问line或part；两端映射归还源读取租约后才进入后续Native绘制。
                        let canonical = cursor.canonical.as_ref().expect("选区续体持原格范围");
                        if let Some(actual) = selections
                            .get(selection)
                            .filter(|range| range.start < canonical.end)
                        {
                            let start = actual.start.max(canonical.start);
                            let end = actual.end.min(canonical.end);
                            if start >= end {
                                cursor.cursor = Decoration::SelectionEntry(selection + 1);
                            } else {
                                let (start, end) = match &self.domain {
                                    LayoutDomain::Body => (start, end),
                                    LayoutDomain::TableCell { display, .. } => {
                                        // 同一真实格的起点取下游、终点取上游，隐藏URL没有选区面积。
                                        // 两次实际读取失败都保留Source错误，不以未映射范围代替显示坐标。
                                        let start = display
                                            .canonical_to_display(
                                                start,
                                                MarkdownComposedLineAffinity::Downstream,
                                            )
                                            .map_err(|cause| {
                                                NativeMarkdownComposedViewportError::Composed(
                                                    MarkdownLineComposeError::Source(cause),
                                                )
                                            })?
                                            .expect("原格交集在显示映射范围内");
                                        let end = display
                                            .canonical_to_display(
                                                end,
                                                MarkdownComposedLineAffinity::Upstream,
                                            )
                                            .map_err(|cause| {
                                                NativeMarkdownComposedViewportError::Composed(
                                                    MarkdownLineComposeError::Source(cause),
                                                )
                                            })?
                                            .expect("原格交集在显示映射范围内");
                                        (start, end)
                                    }
                                };
                                if end < start {
                                    return Err(
                                        NativeMarkdownComposedViewportError::InvalidRange.into()
                                    );
                                } // 与原selection_rects相同：倒序映射是实际坐标失败，不能静默丢失高亮。
                                if start == end {
                                    // 纯隐藏语法没有可见面积，不需要为了确认零矩形再扫描所有parts。
                                    cursor.cursor = Decoration::SelectionEntry(selection + 1);
                                } else {
                                    cursor.selected = Some(start..end);
                                    cursor.cursor =
                                        Decoration::SelectionLine { selection, line: 0 };
                                }
                            }
                        } else {
                            cursor.cursor = Decoration::Done;
                        }
                    }
                    Decoration::SelectionLine { selection, line } => {
                        // 一个selection可以跨多条已准备软行；每条真实行先单独付费，再查看其parts。
                        // 行的可见性与原同步selection_rects完全一致，不用选区包络代替真实clip。
                        if let Some(actual) = self.lines.get(line) {
                            cursor.cursor = if self.visible(actual.frame) {
                                Decoration::SelectionPart {
                                    selection,
                                    line,
                                    part: 0,
                                }
                            } else {
                                Decoration::SelectionLine {
                                    selection,
                                    line: line + 1,
                                }
                            };
                        } else {
                            cursor.selected = None;
                            cursor.cursor = Decoration::SelectionEntry(selection + 1);
                        }
                    }
                    Decoration::SelectionPart {
                        selection,
                        line,
                        part,
                    } => {
                        // 一个真实part最多交一个相交矩形；同part中的全部GCB不会被复制成Vec。
                        // 查实际GCB端点与原part几何后才fill，任何目录/字体错误撤回整个候选。
                        if let Some(actual) = self.lines[line].parts.get(part) {
                            let facts = actual.handle.facts();
                            if !matches!(
                                facts.kind,
                                MarkdownComposedLinePartKind::FoldedSection
                                    | MarkdownComposedLinePartKind::TableRow
                            ) {
                                let selected = cursor
                                    .selected
                                    .as_ref()
                                    .expect("逐 part 绘制保同一已映射选区");
                                let start = selected.start.max(facts.source_utf16.start);
                                let end = selected.end.min(facts.source_utf16.end);
                                if start < end {
                                    // 先保原完整GCB，再将包络裁回本真实整形part。
                                    // 一个GCB可跨多个UBA/字体段；每段只负责自己的实际几何，
                                    // 不能用相邻part端点重复覆盖或请求本part没有的caret。
                                    let range =
                                        self.boundary(start, false)?.max(facts.source_utf16.start)
                                            ..self.boundary(end, true)?.min(facts.source_utf16.end);
                                    let frame =
                                        self.part_rect(&self.lines[line], actual, range, current)?;
                                    if let Some(frame) =
                                        super::super::geometry::clip_selection(frame, self.clip())
                                    {
                                        let [r, g, b, a] = selection_color;
                                        self.fill_color(context, [r, g, b, a]);
                                        // Reader和Font临时借用已结束，Native fill前后仍核同Source/current。
                                        // 外层尚未发布画布，因此取消不会把这一个矩形当作成功帧。
                                        self.require(current)?;
                                        CGContext::fill_rect(Some(context), frame);
                                        cursor.native_draw_returned = true;
                                    }
                                }
                            }
                            cursor.cursor = Decoration::SelectionPart {
                                selection,
                                line,
                                part: part + 1,
                            };
                        } else {
                            cursor.cursor = Decoration::SelectionLine {
                                selection,
                                line: line + 1,
                            };
                        }
                    }
                    Decoration::Done => unreachable!("完成续体不再消费 metadata"),
                }
                self.require(current)?;
            }
            // 先归还图形状态，再运行可重入current；不会让回调继承格局部clip或选区颜色。
            drop(restore);
            self.require(current)?;
            Ok(NativeMarkdownComposedViewportDecorationStep {
                metadata_records: records,
                complete: matches!(cursor.cursor, Decoration::Done),
            })
        })();
        // 派生几何错误或撤销发生后仍核原 Source，避免把真实读失败改写为普通取消。
        let result = match self.require(current) {
            Ok(()) => result,
            Err(cause) => Err(cause.into()),
        };
        result.map_err(|cause| NativeMarkdownComposedViewportPaintError {
            cause,
            native_draw_returned: cursor.native_draw_returned,
        })
    }

    /// 单行块背景的几何取自已封存行；至多固定五个矩形，不枚举本行 parts。
    fn line_background(
        &self,
        line: &Line,
        context: &CGContext,
        native_draw_returned: &mut bool,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintCause> {
        // 颜色来自原Native常量；此单行步骤不改变字号、baseline或段距。
        // 同步oracle仍保原循环实现，此处直接消费同一已封存几何的一个实际行。
        let [red, green, blue, alpha] = NATIVE_MARKDOWN_CODE_BACKGROUND;
        self.fill_color(context, [red, green, blue, alpha]);
        // 块背景只消费已经决定软换行、baseline 和滚动高度的同一行框；不能在这里扩张正文。
        use crate::native_surface::preview_paragraph::PreviewParagraphBackground as Background;
        match line.paragraph.background {
            Some(Background::Code) => {
                // 块代码背景覆盖真实行槽，软换行和滚动高度不能在绘图阶段重新估算。
                CGContext::fill_rect(Some(context), line.frame);
                *native_draw_returned = true;
            }
            Some(Background::Quote) => {
                // 引用背景及左右边线来自原段落策略；只有真实首末软行拥有横向封口。
                // 固定五个以内矩形不枚举格中的任何文字或相邻part。
                self.fill_color(context, [0.10, 0.12, 0.16, 1.0]);
                CGContext::fill_rect(Some(context), line.frame);
                self.fill_color(context, [0.28, 0.34, 0.44, 1.0]);
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(line.frame.origin, NSSize::new(1.0, line.frame.size.height)),
                );
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(
                            line.frame.origin.x + line.frame.size.width - 1.0,
                            line.frame.origin.y,
                        ),
                        NSSize::new(1.0, line.frame.size.height),
                    ),
                );
                // 软行之间没有重复横框；只有同一源段真正首末行才拥有对应边缘。
                let facts = line.handle.facts();
                if facts.source_utf16.start == facts.paragraph_utf16.start {
                    CGContext::fill_rect(
                        Some(context),
                        NSRect::new(
                            NSPoint::new(
                                line.frame.origin.x,
                                line.frame.origin.y + line.frame.size.height - 1.0,
                            ),
                            NSSize::new(line.frame.size.width, 1.0),
                        ),
                    );
                }
                if facts.source_utf16.end == facts.paragraph_utf16.end {
                    CGContext::fill_rect(
                        Some(context),
                        NSRect::new(line.frame.origin, NSSize::new(line.frame.size.width, 1.0)),
                    );
                }
                *native_draw_returned = true;
            }
            Some(Background::Rule) => {
                // 分隔线仍消费原段落文字槽，不因当前viewport边缘或暂停位置移动。
                // 固定细线只位于真实 12 点文字槽内；段距和 viewport 边缘不会移动其位置。
                let low = line.frame.origin.y + line.geometry.after;
                let y = low + line.geometry.text_height / 2.0;
                self.fill_color(context, [0.28, 0.34, 0.44, 1.0]);
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(line.frame.origin.x, y),
                        NSSize::new(line.frame.size.width, 1.0),
                    ),
                );
                *native_draw_returned = true;
            }
            None => {}
        }
        self.fill_color(context, [red, green, blue, alpha]);
        self.require(current)?;
        Ok(())
    }

    /// 单个 inline-code part 只读其真实字体度量；不扫描相邻字形、行或来源。
    fn part_background(
        &self,
        line: &Line,
        part: &Part,
        context: &CGContext,
        native_draw_returned: &mut bool,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintCause> {
        let [red, green, blue, alpha] = NATIVE_MARKDOWN_CODE_BACKGROUND;
        self.fill_color(context, [red, green, blue, alpha]);
        // 原part已经绑定真实字体与来源，这里只判断这一项是否有代码底色面积。
        // 零advance不造占位矩形；隐藏marker也不能借INLINE_CODE位着色。
        let facts = part.handle.facts();
        let styles = facts.presentation;
        if styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            || !styles.contains(MarkdownPresentationStyles::INLINE_CODE)
            || facts.x_advance_26_6 == 0
        {
            return Ok(()); // 隐藏语法、硬分隔和零宽字符都没有可绘制的代码底色面积。
        }
        self.require(current)?;
        // 使用该part原Native字体上下沿，绝不借邻格的字号或整个行的最大高度。
        // 负advance仍按两端实际位置取矩形，保RTL代码段的覆盖范围。
        let metrics = self.palette[part.font]
            .metrics()
            .map_err(NativeMarkdownComposedViewportError::Font)?;
        let start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
        let end = start + facts.x_advance_26_6 as f64 / 64.0;
        let baseline = self.part_baseline(line, part).y + facts.origin_y_26_6 as f64 / 64.0;
        CGContext::fill_rect(
            Some(context),
            NSRect::new(
                NSPoint::new(start.min(end), baseline - metrics.descent),
                NSSize::new((end - start).abs(), metrics.ascent + metrics.descent),
            ),
        ); // 底色采用该part原字体真实上下沿，选区稍后覆盖，不能把已选文字涂回代码底色。
        *native_draw_returned = true;
        // 本次实际fill已经发生；后核失败保留draw事实并让外层丢弃整张候选。
        self.require(current)?;
        Ok(())
    }
}
