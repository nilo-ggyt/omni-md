//! The existing 58-point actions and 42-point format strip share one header
//! height; body, Find, and feedback receive separate absolute regions.

use super::super::NATIVE_MARKDOWN_SIDEBAR_WIDTH;
use super::super::sidebar_geometry::{
    MINIMUM_CONTENT_HEIGHT, MINIMUM_CONTENT_WIDTH, STATUS_HEIGHT,
};

/// Point-based display geometry. Controls retain complete title/status values,
/// accessibility content, and tooltips independently from visual truncation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_surface) struct HeaderFrame {
    pub(in crate::native_surface) x: f64,
    pub(in crate::native_surface) y: f64,
    pub(in crate::native_surface) width: f64,
    pub(in crate::native_surface) height: f64,
}

/// The existing title/actions, format scroller, path, and three footer columns.
/// Undo and redo retain their original indices for the sole Native assembler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_surface) struct HeaderFrames(pub(in crate::native_surface) [HeaderFrame; 14]);

/// Visible feedback alone consumes this height; Find and footer never reuse it.
pub(in crate::native_surface) const FEEDBACK_HEIGHT: f64 = 38.0;

/// Absolute layout on one content root avoids accumulated height error and
/// removes the space of hidden feedback or Find controls.
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
        return None; // Invalid caller-supplied body geometry cannot publish partial frames.
    }
    let feedback = if feedback_visible {
        FEEDBACK_HEIGHT
    } else {
        0.0
    };
    let top = base.origin.y + base.size.height - feedback;
    let scroll_height = base.size.height - feedback - find_height;
    if scroll_height <= 0.0 {
        // Feedback and Find cannot consume the entire body; keep the previous
        // complete publication instead of installing a negative scroll region.
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

/// Preserve the source action between Find and information. Narrow windows keep
/// all fixed actions while the sole horizontal viewport carries formatting.
pub(in crate::native_surface) fn header_frames(width: f64, height: f64) -> Option<HeaderFrames> {
    if !width.is_finite()
        || !height.is_finite()
        || width < MINIMUM_CONTENT_WIDTH
        || height < MINIMUM_CONTENT_HEIGHT
    {
        // Reject nonfinite or undersized input as a whole; the existing complete
        // Native layout remains authoritative until a valid size is available.
        return None;
    }
    let left = NATIVE_MARKDOWN_SIDEBAR_WIDTH + 24.0;
    // Allocate fixed actions from the right edge so the title/path absorb width
    // changes without displacing established action targets or format controls.
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
        // Title and path share the remaining width but keep separate line heights;
        // Chinese glyphs and Latin descenders must not paint into the adjacent row.
        frame(left, height - 29.0, undo_x - left - 12.0, 22.0),
        frame(mode_x, height - 41.0, mode_width, 29.0),
        frame(menu_x, height - 41.0, menu_width, 29.0),
        // Feedback sits between header and Find/body. Hiding it returns its
        // vertical space through the same absolute body layout above.
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
        // The format viewport spans the main document width; format_strip owns
        // its internal padding and button assembly.
        frame(
            NATIVE_MARKDOWN_SIDEBAR_WIDTH,
            height - super::super::sidebar_geometry::HEADER_HEIGHT,
            width - NATIVE_MARKDOWN_SIDEBAR_WIDTH,
            super::super::format_strip::HEIGHT,
        ),
        // The label retains the full path for accessibility and tooltips while
        // the frame bounds only its visible line.
        frame(left, height - 47.0, undo_x - left - 12.0, 15.0),
        // Preserve independent mode, save outcome, and aggregate statistics
        // columns, including each control's accessibility value.
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
    /// Preserve separate action, format, path, and footer regions at supported sizes.
    fn original_top_actions_and_horizontal_format_strip_keep_separate_areas() {
        for width in [900.0, 900.5, 1_120.0, 1_600.0, 4_096.0] {
            for height in [600.0, 720.0, 1_200.5] {
                let frames = header_frames(width, height).unwrap().0;
                for frame in frames {
                    // Every action and the full format viewport stay in the
                    // document region independently from title or text length.
                    assert!(frame.x >= NATIVE_MARKDOWN_SIDEBAR_WIDTH);
                    assert!(frame.x + frame.width <= width);
                    assert!(frame.width > 0.0 && frame.height > 0.0);
                }
                for pair in [0, 4, 5, 6, 1, 7, 8, 2].windows(2) {
                    // Preserve title, undo/redo, Find, source, information,
                    // copy-all, and document menu order without overlapping targets.
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
                    } // The path cannot redirect clicks into the format row below.
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
    /// Repeated visibility changes derive all body/Find positions from one content size.
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
                // Toggle both bars repeatedly: previously consumed height must
                // never accumulate or overlap the retained footer.
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
    /// An independent Find consumer uses exactly its caller-supplied parent region.
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
    /// Reject unsupported content sizes without publishing any partial layout.
    fn invalid_content_sizes_do_not_publish_frames() {
        for width in [899.9, -1.0, f64::NAN, f64::INFINITY] {
            assert!(header_frames(width, 600.0).is_none());
        }
        for height in [599.9, 0.0, f64::NAN, f64::INFINITY] {
            assert!(header_frames(900.0, height).is_none());
        }
    }
}
