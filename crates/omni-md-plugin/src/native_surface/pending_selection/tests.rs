//! Pure coordinate regressions use the production EOF derivation. Actual Native
//! notification delivery belongs to the isolated Native runtime acceptance.

use super::*;

/// Supply original replacement facts without manufacturing an owner, a Native
/// callback, or mutable selection authority.
fn replacement_sources(
    replacement: NSRange,
    caret: NSRange,
    is_input_replacement: bool,
) -> NativeMarkdownPendingSelectionSources {
    NativeMarkdownPendingSelectionSources {
        source: "pure_eof_test",
        replacement: Some(replacement),
        caret: Some(caret),
        marked_before: None,
        marked_after: None,
        is_input_replacement,
    }
}

#[test]
fn measured_growing_and_shrinking_input_derive_only_original_eof_sources() {
    // These coordinates came from real input: replace 9..11 in a 10687-unit
    // source with four units, then replace the resulting 9..13 with two units.
    // Fixed expected endpoints avoid reproducing the production derivation.
    for (before, after, range, caret, expected) in [
        (10687, 10689, NSRange::new(9, 2), NSRange::new(13, 0), 10687),
        (10689, 10687, NSRange::new(9, 4), NSRange::new(11, 0), 10689),
    ] {
        let input = replacement_sources(range, caret, true);
        assert_eq!(
            native_markdown_pending_selection_eof_sources(before, after, input),
            Some((NSRange::new(expected, 0), Some(NSRange::new(10685, 0)))),
        );
        // Canonical edits have the same lengths but lack permission to declare
        // the Native input primitive's transient deletion-stage endpoint.
        let canonical = replacement_sources(range, caret, false);
        assert_eq!(
            native_markdown_pending_selection_eof_sources(before, after, canonical),
            Some((NSRange::new(expected, 0), None)),
        );
    }
}

#[test]
fn malformed_replacement_caret_and_post_length_cannot_derive_eof_permissions() {
    for (range, caret, post) in [
        (NSRange::new(10686, 2), NSRange::new(10690, 0), 10689),
        (
            NSRange::new(usize::MAX, 1),
            NSRange::new(usize::MAX, 0),
            10689,
        ),
        (NSRange::new(9, 2), NSRange::new(8, 0), 10689),
        (NSRange::new(9, 2), NSRange::new(13, 1), 10689),
        (NSRange::new(9, 2), NSRange::new(13, 0), 10690),
    ] {
        // Out-of-bounds or overflowing replacements, a nonterminal/non-caret
        // insertion endpoint, and inconsistent post-edit lengths grant no EOF
        // authority; clamping cannot turn any of them into an original fact.
        assert!(
            native_markdown_pending_selection_eof_sources(
                10687,
                post,
                replacement_sources(range, caret, true),
            )
            .is_none()
        );
    }
}
