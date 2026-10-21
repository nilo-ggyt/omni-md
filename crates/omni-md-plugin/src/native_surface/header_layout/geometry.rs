//! 原58pt顶部操作与42pt横向格式区共用真实页眉高度；正文、Find和反馈各占原绝对区域。

use super::super::NATIVE_MARKDOWN_SIDEBAR_WIDTH;
use super::super::sidebar_geometry::{
    MINIMUM_CONTENT_HEIGHT, MINIMUM_CONTENT_WIDTH, STATUS_HEIGHT,
};

/// 点单位矩形只控制显示；完整标题和状态仍由原控件的值、AX和tooltip承载。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_surface) struct HeaderFrame {
    pub(in crate::native_surface) x: f64,
    pub(in crate::native_surface) y: f64,
    pub(in crate::native_surface) width: f64,
    pub(in crate::native_surface) height: f64,
}

/// 前四项、五个常用按钮、格式scroll、路径和三列footer；历史按钮仍在原4..6槽位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_surface) struct HeaderFrames(pub(in crate::native_surface) [HeaderFrame; 14]);

/// 临时反馈只在有消息时占这38pt；footer与Find不会复用该区域。
pub(in crate::native_surface) const FEEDBACK_HEIGHT: f64 = 38.0;

/// 同一内容根上的绝对布局；隐藏反馈/Find不留下空行，也不累积相对高度误差。
pub(in crate::native_surface) struct EditorAreaFrames {
    pub(in crate::native_surface) scroll: HeaderFrame,
    pub(in crate::native_surface) find: Option<HeaderFrame>,
}

pub(in crate::native_surface) fn editor_area_frames(
    base: objc2_foundation::NSRect,
    feedback_visible: bool,
    find_height: f64,
) -> Option<EditorAreaFrames> {
    if !base.origin.x.is_finite()
        || !base.origin.y.is_finite()
        || !base.size.width.is_finite()
        || !base.size.height.is_finite()
        || !find_height.is_finite()
        || base.size.width <= 0.0
        || base.size.height <= 0.0
        || find_height < 0.0
    {
        return None; // 原装配者传入的真实正文区域无效时不发布部分frame。
    }
    let feedback = if feedback_visible {
        FEEDBACK_HEIGHT
    } else {
        0.0
    };
    let top = base.origin.y + base.size.height - feedback;
    let scroll_height = base.size.height - feedback - find_height;
    if scroll_height <= 0.0 {
        return None;
    }
    Some(EditorAreaFrames {
        scroll: HeaderFrame {
            x: base.origin.x,
            y: base.origin.y,
            width: base.size.width,
            height: scroll_height,
        },
        find: (find_height > 0.0).then_some(HeaderFrame {
            x: base.origin.x,
            y: top - find_height,
            width: base.size.width,
            height: find_height,
        }),
    })
}

/// 顶部源码按钮回到查找与信息之间；窄窗保留全部固定操作，格式由唯一水平视口承载。
pub(in crate::native_surface) fn header_frames(width: f64, height: f64) -> Option<HeaderFrames> {
    if !width.is_finite()
        || !height.is_finite()
        || width < MINIMUM_CONTENT_WIDTH
        || height < MINIMUM_CONTENT_HEIGHT
    {
        // 不能把非法/小于声明下限的尺寸转为部分负矩形，既有布局保持原样。
        return None;
    }
    let left = NATIVE_MARKDOWN_SIDEBAR_WIDTH + 24.0;
    let right = width - 20.0;
    let menu_width = 64.0;
    let menu_x = right - menu_width;
    let mode_width = 44.0;
    let copy_x = menu_x - 8.0 - 68.0;
    let info_x = copy_x - 8.0 - 40.0;
    let mode_x = info_x - 8.0 - mode_width;
    let find_x = mode_x - 8.0 - 48.0;
    let redo_x = find_x - 8.0 - 44.0;
    let undo_x = redo_x - 8.0 - 44.0;
    let frame = |x, y, width, height| HeaderFrame {
        x,
        y,
        width,
        height,
    };
    Some(HeaderFrames([
        // 标题与下方路径共享原横向余量，不挤压常用操作或下行格式入口。
        // 标题与路径加大字号后保留独立行高，中文和拉丁下伸部不能借相邻行空间显示。
        frame(left, height - 29.0, undo_x - left - 12.0, 22.0),
        frame(mode_x, height - 41.0, mode_width, 29.0),
        frame(menu_x, height - 41.0, menu_width, 29.0),
        // 反馈行位于页眉和Find/正文之间；无消息时隐藏，垂直占用由同一绝对布局函数撤销。
        frame(
            left,
            height - super::super::sidebar_geometry::HEADER_HEIGHT - FEEDBACK_HEIGHT + 11.0,
            right - left,
            16.0,
        ),
        frame(undo_x, height - 41.0, 44.0, 29.0),
        frame(redo_x, height - 41.0, 44.0, 29.0),
        frame(find_x, height - 41.0, 48.0, 29.0),
        frame(info_x, height - 41.0, 40.0, 29.0),
        frame(copy_x, height - 41.0, 68.0, 29.0),
        // scroll覆盖原主文档完整宽度；其内部12pt padding和29pt按钮由format_strip单独装配。
        frame(
            NATIVE_MARKDOWN_SIDEBAR_WIDTH,
            height - super::super::sidebar_geometry::HEADER_HEIGHT,
            width - NATIVE_MARKDOWN_SIDEBAR_WIDTH,
            super::super::format_strip::HEIGHT,
        ),
        // 完整路径由标准label的值/AX和tooltip持有；视觉截断只限制这一行显示。
        frame(left, height - 47.0, undo_x - left - 12.0, 15.0),
        // 原单行三列footer：模式、保存事实、树聚合统计分别有独立值和AX。
        frame(left, 6.0, 90.0, STATUS_HEIGHT - 10.0),
        frame(
            left + 102.0,
            6.0,
            right - left - 354.0,
            STATUS_HEIGHT - 10.0,
        ),
        frame(right - 240.0, 6.0, 240.0, STATUS_HEIGHT - 10.0),
    ]))
}

#[cfg(test)]
mod tests {
    use super::super::super::sidebar_geometry::HEADER_HEIGHT;
    use super::*;

    #[test]
    /// 顶部源码与其余原动作同排，42pt横滚视口、真实路径与底栏在所有声明尺寸均不遮挡。
    fn original_top_actions_and_horizontal_format_strip_keep_separate_areas() {
        for width in [900.0, 900.5, 1_120.0, 1_600.0, 4_096.0] {
            for height in [600.0, 720.0, 1_200.5] {
                let frames = header_frames(width, height).unwrap().0;
                for frame in frames {
                    // 控件及完整格式视口均属于主文档区域，固定高度不依赖标题或正文长度。
                    assert!(frame.x >= NATIVE_MARKDOWN_SIDEBAR_WIDTH);
                    assert!(frame.x + frame.width <= width);
                    assert!(frame.width > 0.0 && frame.height > 0.0);
                }
                for pair in [0, 4, 5, 6, 1, 7, 8, 2].windows(2) {
                    // 实际原顺序为标题、撤销/重做、查找、源码、信息、复制全文和文档菜单。
                    assert!(frames[pair[0]].x + frames[pair[0]].width + 7.9 <= frames[pair[1]].x);
                }
                let strip = frames[9];
                assert_eq!(strip.y, height - HEADER_HEIGHT);
                assert_eq!(strip.height, 42.0);
                assert_eq!(strip.x, NATIVE_MARKDOWN_SIDEBAR_WIDTH);
                assert_eq!(strip.x + strip.width, width);
                for (index, frame) in frames
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != 3 && *index < 11)
                {
                    assert!(frame.y >= height - HEADER_HEIGHT);
                    assert!(frame.y + frame.height <= height);
                    if index != 9 {
                        assert!(frame.y > strip.y + strip.height);
                    } // 路径同样与下方格式视口分开，顶部不会把点击交给格式按钮。
                }
                assert!(frames[0].width >= 196.0);
                assert_eq!(frames[10].x, frames[0].x);
                assert_eq!(frames[10].width, frames[0].width);
                assert!(frames[10].y + frames[10].height < frames[0].y);
                for footer in &frames[11..14] {
                    assert!(footer.y + footer.height < STATUS_HEIGHT);
                }
                for pair in frames[11..14].windows(2) {
                    assert!(pair[0].x + pair[0].width < pair[1].x);
                }
                assert!(frames[3].y >= height - HEADER_HEIGHT - FEEDBACK_HEIGHT);
                assert!(frames[3].y + frames[3].height <= height - HEADER_HEIGHT);
            }
        }
        assert!(
            header_frames(1_600.0, 720.0).unwrap().0[0].width
                > header_frames(900.0, 720.0).unwrap().0[0].width
        );
    }

    #[test]
    /// 显隐与重复切换都从同一内容尺寸算绝对位置；正文、Find、反馈和footer互不覆盖。
    fn feedback_find_and_body_share_one_absolute_height_source() {
        for (width, height) in [(900.0, 600.0), (1120.0, 720.0), (1600.0, 1200.0)] {
            let plain = editor_area_frames(
                super::super::super::window::native_markdown_editor_base_frame(
                    objc2_foundation::NSSize::new(width, height),
                ),
                false,
                0.0,
            )
            .unwrap();
            for feedback in [false, true, false, true] {
                for find_height in [0.0, 76.0, 108.0, 76.0, 0.0] {
                    let frames = editor_area_frames(
                        super::super::super::window::native_markdown_editor_base_frame(
                            objc2_foundation::NSSize::new(width, height),
                        ),
                        feedback,
                        find_height,
                    )
                    .unwrap();
                    let used = if feedback { FEEDBACK_HEIGHT } else { 0.0 };
                    assert_eq!(frames.scroll.y, STATUS_HEIGHT);
                    assert_eq!(
                        frames.scroll.height + used + find_height,
                        plain.scroll.height
                    );
                    if let Some(find) = frames.find {
                        assert_eq!(find.y, frames.scroll.y + frames.scroll.height);
                        assert_eq!(find.y + find.height, height - HEADER_HEIGHT - used);
                    } else {
                        assert_eq!(find_height, 0.0);
                    }
                }
            }
        }
    }

    #[test]
    /// 独立Find消费者只有调用方原parent区域，不为不存在的侧栏、页眉和footer扣高度。
    fn independent_find_embedding_uses_its_exact_supplied_base() {
        let base = objc2_foundation::NSRect::new(
            objc2_foundation::NSPoint::ZERO,
            objc2_foundation::NSSize::new(720.0, 300.0),
        );
        let frames = editor_area_frames(base, false, 108.0).unwrap();
        assert_eq!(
            (
                frames.scroll.x,
                frames.scroll.y,
                frames.scroll.width,
                frames.scroll.height
            ),
            (0.0, 0.0, 720.0, 192.0)
        );
        let hidden = editor_area_frames(base, false, 0.0).unwrap();
        assert_eq!(hidden.scroll.height, 300.0);
    }

    #[test]
    /// 外框不等于内容尺寸，所有低于声明下限及非有限值都拒绝整组发布。
    fn invalid_content_sizes_do_not_publish_frames() {
        for width in [899.9, -1.0, f64::NAN, f64::INFINITY] {
            assert!(header_frames(width, 600.0).is_none());
        }
        for height in [599.9, 0.0, f64::NAN, f64::INFINITY] {
            assert!(header_frames(900.0, height).is_none());
        }
    }
}
