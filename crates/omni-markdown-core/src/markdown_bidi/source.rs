//! Traverse only frozen-piece properties; one fixed 4-KiB window may survive await, after the original source lease has actually returned.
use super::*;
use prepare::{io::CooperativeStore, source::Reader};
use store::DiskStack;

pub(super) async fn capture_prepared(
    snapshot: &MarkdownTextSnapshot,
    source: &Range<usize>,
    bytes: Range<usize>,
    default_base: BidiBaseDirection,
    store: &mut CooperativeStore,
) -> Result<u32, BidiError> {
    // Keep the original snapshot and Store authority throughout capture; no new current-document snapshot can bless partially analyzed data.
    snapshot.original_backing_health()?;
    store.healthy()?;
    let gate = std::rc::Rc::clone(&store.gate);
    let mut reader = Reader::new(snapshot, bytes.clone());
    let mut state = Capture {
        store,
        units: 0,
        byte: bytes.start,
        paragraph_start: 0,
        paragraphs: 0,
        base: default_base.level(),
        default_base: default_base.level(),
        isolates: DiskStack::new(),
    };
    while let Some((scalar, _)) = reader.next(&gate).await? {
        state.scalar(scalar).await?;
    }
    snapshot.original_backing_health()?;
    state.store.healthy()?;
    // Only exact byte and UTF16 consumption of the requested range certifies capture completion; a buffer boundary cannot substitute for EOF.
    if state.units as usize != source.len()
        || state.byte != bytes.end
        || reader.position() != bytes.end
    {
        return Err(BidiError::CorruptMetadata);
    }
    if state.paragraph_start < state.units {
        state.finish_paragraph().await?;
    }
    Ok(state.paragraphs)
}

struct Capture<'a> {
    store: &'a mut CooperativeStore,
    units: u32,
    byte: usize,
    paragraph_start: u32,
    paragraphs: u32,
    base: Option<u8>,
    default_base: Option<u8>,
    isolates: DiskStack,
}

impl Capture<'_> {
    async fn scalar(&mut self, scalar: char) -> Result<(), BidiError> {
        use BidiClass::*;
        self.store.healthy()?; // A scalar has at most four source bytes; observe revocation before computing its properties.
        // The Unicode18 delta supplies the class/bracket properties; changing the data version does not replace the subsequent paged UBA algorithm.
        let data = UnicodeBidiData;
        let class = data.bidi_class(scalar);
        let bracket = data.bidi_matched_opening_bracket(scalar);
        let length = scalar.len_utf16() as u32;
        let first = self.units;
        // Write one metadata record per UTF16 unit, preserving low-surrogate records for comparison with original unicode-bidi UTF16 semantics.
        for offset in 0..length {
            Text {
                byte: u32::try_from(self.byte).map_err(|_| BidiError::CoordinateOverflow)?,
                bracket: bracket.map_or(0, |value| value.opening as u32),
                pair_end: NONE,
                pair_start_run: NONE,
                pair_end_run: NONE,
                original: class,
                processing: class,
                level: 0,
                flags: if offset != 0 {
                    2
                } else {
                    u8::from(length == 2)
                } | bracket.map_or(0, |value| if value.is_open { 4 } else { 8 })
                    | ((scalar.len_utf8() as u8 - 1) << 4),
                // Bits 4..5 retain the actual UTF8 width decoded by this capture, shared by head/low-surrogate records.
                // The same private 32-byte record lets a part's endpoint consumer avoid borrowing the next part or seeking through Source again.
            }
            .write_prepared(self.store, self.units + offset)
            .await?;
        }
        self.units = self
            .units
            .checked_add(length)
            .ok_or(BidiError::CoordinateOverflow)?;
        self.byte += scalar.len_utf8();
        match class {
            B => self.finish_paragraph().await?,
            L | R | AL => {
                // Resolve the nearest open FSI from its actual first strong character; paragraph P2 observes only strong characters outside isolates.
                if let Some((start, _)) = self.store.stack_top(&self.isolates).await? {
                    let mut isolate = Text::read_prepared(self.store, start).await?;
                    if isolate.original == FSI {
                        isolate.original = if class == L { LRI } else { RLI };
                        isolate.processing = isolate.original;
                        isolate.write_prepared(self.store, start).await?;
                    }
                } else if self.base.is_none() {
                    self.base = Some(u8::from(class != L)); // P2 uses only the first strong character outside isolates.
                }
            }
            RLI | LRI | FSI => {
                self.store
                    .stack_push(&mut self.isolates, first, NONE)
                    .await?
            }
            PDI => {
                self.store.stack_pop(&mut self.isolates).await?;
            }
            _ => {} // Weak, neutral, and formatting characters do not decide P2, but their records still participate in later rules.
        }
        Ok(())
    }

    async fn finish_paragraph(&mut self) -> Result<(), BidiError> {
        // Publish one actual paragraph range/base, then reset its isolate/P2 context without retaining a paragraph-length text buffer.
        Paragraph {
            start: self.paragraph_start,
            end: self.units,
            base: self.base.unwrap_or(0),
        }
        .write_prepared(self.store, self.paragraphs)
        .await?;
        self.paragraphs += 1;
        self.paragraph_start = self.units;
        self.base = self.default_base;
        self.isolates.clear(); // B ends the UBA paragraph; unmatched isolates must not leak into the next paragraph.
        Ok(())
    }
}
