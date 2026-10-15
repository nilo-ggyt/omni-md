//! Bounded bracket/control-character cues derived from canonical UTF-16 and settled TextKit geometry.
//!
//! Queued layout owns preparation. Paint reads copied rectangles only, so caret movement and drawing
//! never build a second string model or force offscreen layout. All caches end with the editor.

use super::*;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSTextLayoutManagerSegmentOptions,
    NSTextLayoutManagerSegmentType,
};
use omni_markdown_core::markdown_brackets::{MarkdownBracketQuery, markdown_bracket_match};

/// Control-character discovery and native marker ownership remain capped to one bounded viewport.
const MAX_DECORATION_VIEWPORT_UNITS: usize = 16_384;
const MAX_SPECIAL_CHARACTER_MARKERS: usize = 128;

/// Prepared native decoration geometry owns no document string or native layout objects.
#[derive(Clone, PartialEq)]
pub(super) struct NativeMarkdownDecorationGeometry {
    frame: NSRect,
    matched: Option<bool>,
}

/// Match the preserved special-character predicate while excluding canonical CR/LF line separators.
fn is_visible_special_character(unit: u16) -> bool {
    matches!(unit, 0x0000..=0x0008 | 0x000b..=0x000c | 0x000e..=0x001f | 0x007f..=0x009f | 0x00ad | 0x061c | 0x200b | 0x200e..=0x200f | 0x2028..=0x2029 | 0x202d..=0x202e | 0x2066..=0x2067 | 0x2069 | 0xfeff | 0xfff9..=0xfffc)
}

impl PieceBackedMarkdownTextView {
    /// Translate one admitted canonical handle into finite copied native segment rectangles.
    fn source_decoration_frames(
        &self,
        source: std::ops::Range<usize>,
        maximum_frames: usize,
    ) -> Vec<NSRect> {
        let Some(manager) = self.textLayoutManager() else {
            // A disconnected layout graph cannot produce visible cue geometry.
            return Vec::new();
        };
        let Some(content) = manager.textContentManager() else {
            // Source positions have no native location authority after content detaches.
            return Vec::new();
        };
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let start = provider.documentRange().location();
        let (Ok(from), Ok(to)) = (isize::try_from(source.start), isize::try_from(source.end))
        else {
            // Native signed coordinates must represent both complete scalar-aligned handle boundaries.
            return Vec::new();
        };
        let Some(location) = content.locationFromLocation_withOffset(&start, from) else {
            // Transient or omitted source intervals must not receive an invented overlay position.
            return Vec::new();
        };
        let Some(end) = content.locationFromLocation_withOffset(&start, to) else {
            // An incomplete native range is rejected as a whole rather than clipped arbitrarily.
            return Vec::new();
        };
        let Some(range) =
            NSTextRange::initWithLocation_endLocation(NSTextRange::alloc(), &location, Some(&end))
        else {
            // Invalid native ordering leaves the source glyphs unadorned until a later layout request.
            return Vec::new();
        };
        let origin = self.textContainerOrigin();
        let frames = RefCell::new(Vec::new());
        let block = block2::RcBlock::new(
            |_range: *mut NSTextRange,
             frame: NSRect,
             _baseline: f64,
             _container: NonNull<NSTextContainer>| {
                if frames.borrow().len() >= maximum_frames {
                    // Multi-line handles share the global viewport geometry cap with control markers.
                    return objc2::runtime::Bool::NO;
                }
                // Copy only geometry; callback-borrowed TextKit objects and source text never escape.
                frames.borrow_mut().push(NSRect::new(
                    NSPoint::new(frame.origin.x + origin.x, frame.origin.y + origin.y),
                    NSSize::new(frame.size.width.max(7.0), frame.size.height.max(14.0)),
                ));
                objc2::runtime::Bool::YES
            },
        );
        manager.enumerateTextSegmentsInRange_type_options_usingBlock(
            &range,
            NSTextLayoutManagerSegmentType::Standard,
            NSTextLayoutManagerSegmentOptions::RangeNotRequired,
            &block,
        );
        // End the callback's borrow before transferring its finite value-only geometry result.
        drop(block);
        frames.into_inner()
    }

    /// Prepare finite cues after the shared layout executor has left all native editing transactions.
    pub(super) fn refresh_source_decorations(&self) -> bool {
        let Some(document) = self
            .ivars()
            .text_storage
            .load()
            .and_then(|storage| storage.document())
        else {
            // Teardown releases old cue geometry without replacing its expired document owner.
            let changed = !self.ivars().source_decorations.borrow().is_empty();
            self.ivars().source_decorations.borrow_mut().clear();
            return changed;
        };
        let (generation, length) = {
            // End canonical borrows before viewport/segment calls can reenter native text callbacks.
            let document = document.borrow();
            (document.generation(), document.utf16_len())
        };
        let selection = self.selectedRange();
        let mut positions = Vec::with_capacity(MAX_SPECIAL_CHARACTER_MARKERS + 2);
        if selection.length == 0 && selection.location <= length {
            // Parsing/classification stays portable and bounded; release the canonical borrow before native geometry.
            let query = markdown_bracket_match(&document.borrow(), selection.location);
            match query {
                // Only a proven admitted token context supplies visible bracket handles.
                Ok(MarkdownBracketQuery::Match(found)) => {
                    positions.push((found.start_utf16, Some(found.matched)));
                    if let Some(end) = found.end_utf16 {
                        // Complete matching or mismatched pairs display both canonical source handles.
                        positions.push((end, Some(found.matched)));
                    }
                }
                // Ordinary carets have no nearby bracket cue and require no native segment queries.
                Ok(MarkdownBracketQuery::NoMatch) => {}
                // A bounded search cannot honestly label an unseen partner as unmatched.
                Ok(MarkdownBracketQuery::BudgetExhausted) => {}
                // Unestablished parser context is omitted rather than assigning a misleading cross-token pair.
                Ok(MarkdownBracketQuery::ContextUnavailable) => {}
                // Invalid/stale canonical coordinates produce no partial decoration or document mutation.
                Err(_) => {}
            }
        }
        let visible = self.visibleRect();
        let paragraph_window = self.visible_utf16_window(visible, length);
        let viewport = match (
            self.insertion_offset_at_view_point(NSPoint::new(visible.origin.x, visible.origin.y)),
            self.insertion_offset_at_view_point(NSPoint::new(
                visible.origin.x + visible.size.width - 1.0,
                visible.origin.y + visible.size.height - 1.0,
            )),
        ) {
            // Point navigation narrows giant wrapped paragraphs to the actual visible character interval.
            (Some(start), Some(end)) => {
                start.min(end)..start.max(end).saturating_add(1).min(length)
            }
            // Unavailable transient point geometry keeps the existing bounded paragraph fallback.
            _ => paragraph_window,
        };
        let folds = self.ivars().folds.borrow().clone();
        let bracket_count = positions.len();
        let mut cursor = viewport.start;
        let mut scanned = 0_usize;
        let mut scratch = [0_u16; 512];
        while cursor < viewport.end
            && scanned < MAX_DECORATION_VIEWPORT_UNITS
            && positions.len() - bracket_count < MAX_SPECIAL_CHARACTER_MARKERS
        {
            // Skip collapsed source intervals by metadata; they consume no copy/scan budget or native query.
            if let Some(hidden) = folds
                .iter()
                .find(|fold| fold.hidden_utf16_range.contains(&cursor))
            {
                // The canonical hidden text remains saved/searchable but contributes no visible marker geometry.
                cursor = hidden.hidden_utf16_range.end;
                continue;
            }
            let next_hidden = folds
                .iter()
                .filter(|fold| fold.hidden_utf16_range.start > cursor)
                .map(|fold| fold.hidden_utf16_range.start)
                .min()
                .unwrap_or(viewport.end);
            let count = scratch
                .len()
                .min(viewport.end - cursor)
                .min(next_hidden - cursor)
                .min(MAX_DECORATION_VIEWPORT_UNITS - scanned);
            if document
                .borrow()
                .copy_utf16_code_units(cursor, &mut scratch[..count])
                .is_err()
            {
                // A changed/disconnected canonical interval contributes no partially initialized control cues.
                break;
            }
            for (index, unit) in scratch[..count].iter().enumerate() {
                // Every inspected control comes from the fixed visible scratch and finite retained marker cap.
                if positions.len() - bracket_count >= MAX_SPECIAL_CHARACTER_MARKERS {
                    // Further visible controls are discovered only after a future viewport/selection request.
                    break;
                }
                if is_visible_special_character(*unit) {
                    // A dot identifies a control without changing or retaining the underlying source code unit.
                    positions.push((cursor + index..cursor + index + 1, None));
                }
            }
            cursor += count;
            scanned += count;
        }
        let maximum_geometry = MAX_SPECIAL_CHARACTER_MARKERS + 2;
        let mut geometry = Vec::with_capacity(positions.len());
        for (source, matched) in positions {
            if geometry.len() >= maximum_geometry {
                // A wrapped handle cannot expand persistent native geometry beyond the shared finite viewport budget.
                break;
            }
            if source.start >= viewport.end
                || source.end <= viewport.start
                || folds.iter().any(|fold| {
                    // A cue crossing omitted canonical text cannot claim one contiguous visible native handle.
                    source.start < fold.hidden_utf16_range.end
                        && source.end > fold.hidden_utf16_range.start
                })
            {
                // Offscreen partners and deliberately hidden source never force native layout or scrolling.
                continue;
            }
            let visible_source = source.start.max(viewport.start)..source.end.min(viewport.end);
            let frames =
                self.source_decoration_frames(visible_source, maximum_geometry - geometry.len());
            for frame in frames {
                if NSIntersectsRect(frame, visible) {
                    // Retain copied rectangles only for actual visible handle segments and control positions.
                    geometry.push(NativeMarkdownDecorationGeometry { frame, matched });
                }
            }
        }
        let changed = *self.ivars().source_decorations.borrow() != geometry;
        *self.ivars().source_decorations.borrow_mut() = geometry;
        self.ivars().source_decoration_generation.set(generation);
        self.ivars().source_decorations_dirty.set(false);
        changed
    }

    /// Paint already prepared bracket outlines and control dots without native element enumeration.
    pub(super) fn draw_source_decorations(&self, dirty: NSRect) {
        if self.ivars().textkit_input_depth.get() != 0
            || self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        {
            // Composition keeps native provisional glyphs unobscured and never paints stale cue positions.
            return;
        }
        let generation = self
            .ivars()
            .text_storage
            .load()
            .and_then(|storage| storage.document())
            .map(|document| document.borrow().generation());
        if generation != Some(self.ivars().source_decoration_generation.get()) {
            // Canonical edits invalidate all old rectangles before the next queued layout transaction.
            return;
        }
        let geometry = self.ivars().source_decorations.borrow().clone();
        NSGraphicsContext::saveGraphicsState_class();
        for cue in geometry {
            // Copying the bounded cache ends state borrows before native drawing can reenter AppKit.
            if !NSIntersectsRect(cue.frame, dirty) {
                // Undamaged viewport cues need no native draw work.
                continue;
            }
            let color = match cue.matched {
                // Balanced pairs use an unobtrusive green outline around their existing source glyphs.
                Some(true) => NSColor::colorWithSRGBRed_green_blue_alpha(0.4, 0.8, 0.6, 0.7),
                // Invalid/unclosed pairs retain a visible red outline without changing editable text.
                Some(false) => NSColor::systemRedColor(),
                // Special control characters use the same visible warning tint as the preserved editor.
                None => NSColor::systemRedColor(),
            };
            color.setStroke();
            if cue.matched.is_some() {
                // Outlines preserve the native glyph, selection and accessibility source value.
                NSBezierPath::strokeRect(NSInsetRect(cue.frame, -1.0, -1.0));
            } else {
                // A red dot reveals invisible control presence while its canonical code unit remains unchanged.
                let font = NSFont::systemFontOfSize(12.0);
                // SAFETY: these are immutable AppKit exported font/color keys with process lifetime.
                let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
                let values: [&AnyObject; 2] = [&font, &color];
                let attributes = MarkdownTextAttributes::from_slices(&keys, &values);
                // SAFETY: the complete immutable dictionary uses valid font/color objects retained during this draw.
                let label = unsafe {
                    NSAttributedString::initWithString_attributes(
                        NSAttributedString::alloc(),
                        ns_string!("•"),
                        Some(&attributes),
                    )
                };
                label.drawAtPoint(cue.frame.origin);
            }
        }
        NSGraphicsContext::restoreGraphicsState_class();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invisible_controls_match_the_preserved_editor_predicate_without_marking_emoji_joiners() {
        for unit in [0, 7, 0x061c, 0x200b, 0x202e, 0x2066, 0xfeff, 0xfffc] {
            // Explicit controls/bidi markers are visible cues; ordinary line breaks and emoji joiners are excluded.
            assert!(is_visible_special_character(unit));
        }
        for unit in [9, 10, 13, 32, 0x200c, 0x200d, 0x2068, 0xd83d] {
            // The exact baseline predicate does not treat every format character or surrogate as a special marker.
            assert!(!is_visible_special_character(unit));
        }
    }
}
