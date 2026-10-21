//! Reuse actual font choices for complete small graphemes within one Body; retain no Source, probe,
//! job, missing-glyph count, or old response.

use std::{
    ops::Range,
    rc::{Rc, Weak},
};

use omni_markdown_core::{MarkdownLineComposeStyle, MarkdownLineFontProbe, MarkdownShapingOptions};

use super::{NativeMarkdownLineFont, NativeMarkdownPagedBodyError as Error, Result};

const CAPACITY: usize = 128;
const GRAPHEME_BYTES: usize = 32;
const CONTEXT_BYTES: usize = 20;
// The key's maximum Source envelope lets the Producer decide whether this quantum can capture the
// complete key.
pub(super) const MAXIMUM_CHOICE_SOURCE_BYTES: usize = GRAPHEME_BYTES + 2 * CONTEXT_BYTES;

/// Complete grapheme bytes plus the shaper's 20-byte raw context on each side; equality preserves its
/// original five-scalar context without another decoder.
#[derive(Eq, PartialEq)]
struct ChoiceInput {
    // Three adjacent Source segments share one fixed envelope; explicit lengths retain their boundaries
    // and exact raw context.
    // Keep unused tail bytes zero; identical bytes elsewhere do not imply the same grapheme or
    // beginning/end-of-text semantics.
    bytes: [u8; MAXIMUM_CHOICE_SOURCE_BYTES],
    before_len: usize,
    grapheme_len: usize,
    after_len: usize,
}

/// Retain weak primary-font identity, fixed input, and bounded original options; point size or
/// PostScript name cannot identify another actual font.
pub(super) struct FontChoiceKey {
    primary: Weak<NativeMarkdownLineFont>,
    primary_points: u64,
    options: MarkdownShapingOptions,
    // Feature endpoints use global UTF-16 clusters; nonempty features retain the original range rather
    // than reusing a mask at another position.
    feature_range: Option<Range<usize>>,
    input: ChoiceInput,
}

impl FontChoiceKey {
    /// Capture complete bounded input under the original probe/Source ticket; oversized graphemes skip
    /// caching without truncation and retain the full trial.
    /// Return actual Source bytes read, including failures; the at-most-72-byte miss cost is not
    /// credited as input already consumed by the job.
    pub(super) fn capture(
        probe: &MarkdownLineFontProbe,
        style: &MarkdownLineComposeStyle,
        primary: &Rc<NativeMarkdownLineFont>,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> (usize, Result<Option<Self>>) {
        let mut read = 0_usize;
        let result = (|| {
            require_current_probe(probe, current)?;
            if !probe.shaping_segment_ready()? || !probe.complete_single_segment()? {
                // An outer grapheme may span several bidi/script segments; the first segment's options
                // or choice cannot stand in for the whole cluster.
                return Ok(None);
            }
            let range = probe.byte_range()?;
            if range.len() > GRAPHEME_BYTES {
                return Ok(None); // The first 32 bytes of an oversized combining sequence cannot certify complete input or a chosen font.
            }
            let options = probe.shaping_options(style)?;
            let feature_range = (!options.features.is_empty()).then(|| probe.source_range());
            let length = probe.source()?.byte_len();
            let before = range.start.saturating_sub(CONTEXT_BYTES)..range.start;
            let after = range.end..range.end.saturating_add(CONTEXT_BYTES).min(length);
            let mut input = ChoiceInput {
                bytes: [0; MAXIMUM_CHOICE_SOURCE_BYTES],
                before_len: before.len(),
                grapheme_len: range.len(),
                after_len: after.len(),
            };
            // The three adjacent segments fit one at-most-72-byte access, avoiding three preads for
            // each cache lookup.
            // Use certified byte endpoints; remote UTF-16 lookup, whole-paragraph scanning, or another
            // cache is outside this optimization.
            let envelope = before.start..after.end;
            require_current_probe(probe, current)?;
            let (used, copied) =
                probe.copy_choice_bytes(envelope.clone(), &mut input.bytes[..envelope.len()]);
            read = read.checked_add(used).ok_or(Error::CounterExhausted)?;
            copied?; // Preserve the typed first failure and charge only bytes actually delivered before a cross-piece failure.
            require_current_probe(probe, current)?; // Copying cannot reenter Native, but callback revocation must still reject key publication.
            Ok(Some(Self {
                primary: Rc::downgrade(primary),
                primary_points: primary.native().point_size().to_bits(),
                options,
                feature_range,
                input,
            }))
        })();
        (read, result)
    }

    /// Compare floating-point protocol bits and the original feature order/ranges; reuse only exactly
    /// identical shaping-choice input.
    fn same(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.primary, &other.primary)
            && self.primary_points == other.primary_points
            && self.options.points.to_bits() == other.options.points.to_bits()
            && self.options.direction == other.options.direction
            && self.options.script == other.options.script
            && self.options.language == other.options.language
            && self.options.beginning_of_text == other.options.beginning_of_text
            && self.options.end_of_text == other.options.end_of_text
            && self.options.features == other.options.features
            && self.feature_range == other.feature_range
            && self.input == other.input
    }
}

/// Validate the same probe around every Native/current callback so ordinary callback revocation cannot
/// hide a Source error.
fn require_current_probe(
    probe: &MarkdownLineFontProbe,
    current: &(impl Fn() -> bool + ?Sized),
) -> Result<()> {
    probe.validate()?;
    let permitted = current();
    probe.validate()?;
    if !permitted {
        return Err(Error::PreparationRevoked);
    }
    Ok(())
}

struct Entry {
    key: FontChoiceKey,
    selected: Weak<NativeMarkdownLineFont>,
}

/// A fixed 128-entry ring has no full-Source index, additional async queue, or process-wide font state.
pub(super) struct FontChoices {
    entries: Vec<Entry>,
    next: usize,
}

impl FontChoices {
    /// An empty Body reserves no payload; the first actual Selected result allocates fixed slots owned
    /// by that Body.
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
            next: 0,
        }
    }

    /// Return only an actual registered font still retained by the original palette; an expired weak
    /// value is a miss, never a reconstructed or renewed result.
    /// The caller must still validate the current probe, Source, and selection, then submit through the
    /// same production response path.
    pub(super) fn get(&self, key: &FontChoiceKey) -> Option<Rc<NativeMarkdownLineFont>> {
        self.entries.iter().find_map(|entry| {
            if entry.key.same(key) {
                entry.selected.upgrade()
            } else {
                None
            }
        })
    }

    /// Call only after a complete trial selected a font, its glyph/FD/mapping resources were returned,
    /// and actual font registration completed.
    /// No TrialResult or missing-glyph count enters this cache; weak fonts neither manufacture success
    /// receipts nor extend mapping/Native resource lifetimes.
    pub(super) fn remember(&mut self, key: FontChoiceKey, selected: &Rc<NativeMarkdownLineFont>) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key.same(&key)) {
            entry.selected = Rc::downgrade(selected);
            return; // Identical input discards the new finite metadata instead of accumulating duplicate history.
        }
        let entry = Entry {
            key,
            selected: Rc::downgrade(selected),
        };
        if self.entries.len() < CAPACITY {
            if self.entries.capacity() == 0 {
                self.entries = Vec::with_capacity(CAPACITY);
            }
            self.entries.push(entry);
            return; // Allocate capacity on the first Selected result and never grow beyond 128 entries.
        }
        self.entries[self.next] = entry;
        self.next = (self.next + 1) % CAPACITY;
    }

    /// Release keys and allocated slots together; a closed Body retains no context bytes, font
    /// weak-control blocks, or entry capacity.
    pub(super) fn clear(&mut self) {
        self.entries = Vec::new();
        self.next = 0;
    }
}
