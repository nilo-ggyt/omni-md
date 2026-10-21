//! Streaming default UAX #14 revision-57 break opportunities. Width-based line
//! selection, dictionary segmentation, and locale tailoring have separate owners.

use super::{MarkdownLineBreakKind as BreakKind, unicode_data::Class};
use Class::*;

#[cfg(feature = "paged-line-compose")]
use crate::markdown_layout_source::folds::FoldSpan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Break {
    pub byte_offset: u64,
    pub utf16_offset: u64,
    pub kind: BreakKind,
    pub rule: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    OutOfOrder,
    InvalidUtf8,
    OffsetOverflow,
    Closed,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Meta {
    class: Class,
    pi: bool,
    pf: bool,
    east_asian: bool,
    unassigned_pictograph: bool,
    dotted_circle: bool,
}

impl Meta {
    /// Applies default LB1 class resolution while retaining raw quote, width,
    /// emoji, and dictionary-needed facts for later ordered rules.
    fn new(character: char) -> (Self, bool) {
        let (class, flags) = super::unicode_data::properties(character);
        let dictionary = class == Sa && flags & 4 == 0;
        let class = match class {
            Ai | Sg | Xx => Al,
            Sa if flags & 4 != 0 => Cm,
            Sa => Al,
            Cj => Ns,
            other => other, // Preserve CB for LB20; default breaking does not complete dictionary analysis.
        };
        (
            Self {
                class,
                pi: flags & 1 != 0,
                pf: flags & 2 != 0,
                east_asian: flags & 16 != 0,
                unassigned_pictograph: flags & 40 == 40,
                dotted_circle: flags & 64 != 0,
            },
            dictionary,
        )
    }
    fn latin_a() -> Self {
        // LB10 replaces all properties. Original isolated-mark width/quote/emoji
        // flags must not leak into rules for its substituted alphabetic token.
        Self {
            class: Al,
            pi: false,
            pf: false,
            east_asian: false,
            unassigned_pictograph: false,
            dotted_circle: false,
        }
    }
    /// Supplies the exact aksara-or-dotted-circle predicate used by LB28a.
    fn syllable(self) -> bool {
        matches!(self.class, Ak | As) || self.dotted_circle
    }
    /// Keeps LB28a's narrower post-virama class distinct from its syllable set.
    fn ak_or_dotted(self) -> bool {
        self.class == Ak || self.dotted_circle
    }
}

#[derive(Clone, Copy, Debug)]
struct Token {
    meta: Meta,
    byte_start: u64,
    utf16_start: u64,
    raw_last_zwj: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Left {
    last: Option<Token>,
    previous: Option<Meta>,
    non_space: Option<Meta>,
    initial_quote: bool,
    numeric_suffix: bool,
    numeric_closer: bool,
    odd_ri: bool,
}

impl Left {
    /// Retains finite left context for spaces, numeric suffixes, and RI parity;
    /// text length and combining-chain depth never enlarge this state.
    fn advance(&mut self, token: Token) {
        let meta = token.meta;
        let previous = self.last.map(|last| last.meta);
        if meta.class != Sp {
            self.non_space = Some(meta);
            self.initial_quote = meta.class == Qu
                && meta.pi
                && previous.is_none_or(|before| {
                    matches!(before.class, Bk | Cr | Lf | Nl | Op | Qu | Gl | Sp | Zw)
                });
        } // Arbitrarily long space sequences retain only preceding class/quote state, never their text.
        self.numeric_closer = matches!(meta.class, Cl | Cp) && self.numeric_suffix;
        self.numeric_suffix =
            meta.class == Nu || (matches!(meta.class, Sy | Is) && self.numeric_suffix);
        self.odd_ri = meta.class == Ri && !self.odd_ri;
        self.previous = previous;
        self.last = Some(token);
    }
}

/// Evaluates default rules in their required precedence using real effective
/// tokens. A returned opportunity is independent of width, shaping, and locale.
fn decision(
    left: &Left,
    right: Token,
    next: Option<Token>,
    third: Option<Token>,
) -> Option<(BreakKind, &'static str)> {
    let prior = left.last?; // LB2: actual start-of-text emits no break opportunity.
    let l = prior.meta;
    let r = right.meta;
    let lc = l.class;
    let rc = r.class;
    let n = next.map(|value| value.meta);
    let nc = n.map(|value| value.class);
    let tc = third.map(|value| value.meta.class);
    let before = left.previous;
    let non_space = left.non_space.map(|meta| meta.class);
    // Mandatory hard-break rules precede every ordinary opportunity or prohibition.
    if lc == Bk {
        return Some((BreakKind::Mandatory, "LB4"));
    }
    // A real CRLF is indivisible; the mandatory opportunity follows the LF.
    if lc == Cr && rc == Lf {
        return None;
    }
    if matches!(lc, Cr | Lf | Nl) {
        return Some((BreakKind::Mandatory, "LB5"));
    }
    if matches!(rc, Bk | Cr | Lf | Nl) {
        return None;
    } // LB6: no opportunity immediately before a hard-break control.
    if matches!(rc, Sp | Zw) {
        return None;
    } // LB7 runs before the rule permitting a break after zero-width space.
    // Zero-width space permits the following break even after an arbitrary SP run.
    if non_space == Some(Zw) {
        return Some((BreakKind::Allowed, "LB8"));
    }
    if prior.raw_last_zwj {
        return None;
    } // LB8a uses the raw final ZWJ, not merely its inherited class.
    if lc == Wj || rc == Wj {
        return None;
    } // LB11: word joiner prohibits breaks on both sides.
    if lc == Gl {
        return None;
    } // LB12: glue forbids the boundary immediately after itself.
    if rc == Gl && !matches!(lc, Sp | Hy | Hh) {
        return None;
    } // LB12a: only the prescribed space/hyphen classes may precede glue.
    if matches!(rc, Cl | Cp | Ex | Sy) {
        return None;
    } // LB13: closing punctuation, exclamation, and symbols remain attached on the left.
    if non_space == Some(Op) {
        return None;
    } // LB14: opening punctuation keeps following spaces with its next token.
    if non_space == Some(Qu) && left.initial_quote {
        return None;
    } // LB15a: an initial quotation mark keeps its following space sequence attached.
    if rc == Qu
        && r.pf
        && nc.is_none_or(|value| {
            matches!(
                value,
                Sp | Gl | Wj | Cl | Qu | Cp | Ex | Is | Sy | Bk | Cr | Lf | Nl | Zw
            )
        })
    {
        return None;
    } // LB15b: final quotation before closing/space/end context stays attached on its left.
    // The explicit numeric-infix exception is evaluated before general infix prohibition.
    if lc == Sp && rc == Is && nc == Some(Nu) {
        return Some((BreakKind::Allowed, "LB15c"));
    }
    if rc == Is {
        return None;
    } // LB15d: remaining numeric infix separators cannot begin a line.
    if matches!(non_space, Some(Cl | Cp)) && rc == Ns {
        return None;
    } // LB16: a closing bracket followed by spaces still binds a nonstarter.
    if non_space == Some(B2) && rc == B2 {
        return None;
    } // LB17: paired break-both characters keep intervening spaces inside the pair.
    // Only after earlier restrictions have been considered does ordinary space allow a break.
    if lc == Sp {
        return Some((BreakKind::Allowed, "LB18"));
    }
    if (rc == Qu && !r.pi) || (lc == Qu && !l.pf) {
        return None;
    } // LB19: quotation marks without the directional punctuation exception stay attached.
    if (!l.east_asian && rc == Qu)
        || (rc == Qu && n.is_none_or(|meta| !meta.east_asian))
        || (lc == Qu && !r.east_asian)
        || (lc == Qu && before.is_none_or(|meta| !meta.east_asian))
    {
        return None;
    } // LB19a: non-East-Asian neighbors constrain quotation-mark boundaries.
    // Unresolved contingent-break objects retain default break opportunities on either side.
    if rc == Cb || lc == Cb {
        return Some((BreakKind::Allowed, "LB20"));
    }
    if matches!(lc, Hy | Hh)
        && matches!(rc, Al | Hl)
        && before.is_none_or(|meta| matches!(meta.class, Bk | Cr | Lf | Nl | Sp | Zw | Cb | Gl))
    {
        return None;
    } // LB20a: an initial hyphen stays with the following alphabetic text.
    if matches!(rc, Ba | Hh | Hy | Ns) || lc == Bb {
        return None;
    } // LB21: break-after/nonstarter/hyphen classes cannot start, nor break-before end, a line.
    if before.is_some_and(|meta| meta.class == Hl) && matches!(lc, Hy | Hh) && rc != Hl {
        return None;
    } // LB21a: preserve the Hebrew-letter and following hyphen context.
    if lc == Sy && rc == Hl {
        return None;
    } // LB21b: a solidus stays with the following Hebrew letter.
    if rc == In {
        return None;
    } // LB22: inseparable characters remain attached to preceding text.
    if (matches!(lc, Al | Hl) && rc == Nu) || (lc == Nu && matches!(rc, Al | Hl)) {
        return None;
    } // LB23: adjacent alphabetic and numeric tokens remain in one unbroken sequence.
    if (lc == Pr && matches!(rc, Id | Eb | Em)) || (matches!(lc, Id | Eb | Em) && rc == Po) {
        return None;
    } // LB23a: numeric prefixes/postfixes stay with the prescribed ideographic/emoji classes.
    if (matches!(lc, Pr | Po) && matches!(rc, Al | Hl))
        || (matches!(lc, Al | Hl) && matches!(rc, Pr | Po))
    {
        return None;
    } // LB24: numeric affixes stay attached to adjacent alphabetic text.
    if (matches!(rc, Po | Pr) && (left.numeric_suffix || left.numeric_closer))
        || (matches!(lc, Po | Pr)
            && rc == Op
            && (nc == Some(Nu) || (nc == Some(Is) && tc == Some(Nu))))
        || (matches!(lc, Po | Pr | Hy | Is) && rc == Nu)
        || (left.numeric_suffix && rc == Nu)
    {
        return None;
    } // LB25 retains numeric-suffix state without caching the complete number string.
    if (lc == Jl && matches!(rc, Jl | Jv | H2 | H3))
        || (matches!(lc, Jv | H2) && matches!(rc, Jv | Jt))
        || (matches!(lc, Jt | H3) && rc == Jt)
    {
        return None;
    } // LB26: Hangul leading, vowel, trailing, and syllable classes keep valid sequences together.
    if (matches!(lc, Jl | Jv | Jt | H2 | H3) && rc == Po)
        || (lc == Pr && matches!(rc, Jl | Jv | Jt | H2 | H3))
    {
        return None;
    } // LB27: Hangul syllables bind the same numeric affixes as ideographic text.
    if matches!(lc, Al | Hl) && matches!(rc, Al | Hl) {
        return None;
    } // LB28: ordinary alphabetic/Hebrew letter sequences do not break internally.
    if (lc == Ap && r.syllable())
        || (l.syllable() && matches!(rc, Vf | Vi))
        || (before.is_some_and(Meta::syllable) && lc == Vi && r.ak_or_dotted())
        || (l.syllable() && r.syllable() && nc == Some(Vf))
    {
        return None;
    } // LB28a: preserve aksara/virama syllable context, including dotted-circle stand-ins.
    if lc == Is && matches!(rc, Al | Hl) {
        return None;
    } // LB29: an infix separator stays with following alphabetic text.
    if (matches!(lc, Al | Hl | Nu) && rc == Op && !r.east_asian)
        || (lc == Cp && !l.east_asian && matches!(rc, Al | Hl | Nu))
    {
        return None;
    } // LB30: non-East-Asian parentheses remain attached to adjacent letters/numbers.
    if lc == Ri && rc == Ri && left.odd_ri {
        return None;
    } // LB30a: RI parity keeps flag pairs together without retaining the entire RI run.
    if (lc == Eb || l.unassigned_pictograph) && rc == Em {
        return None;
    } // LB30b: emoji bases, including eligible unassigned pictographs, bind their modifier.
    // Only a boundary not claimed by any earlier prohibition/default rule reaches LB31.
    Some((BreakKind::Allowed, "LB31"))
}

/// All runtime state has fixed size. Three effective right-side tokens may
/// span arbitrarily long combining-mark/ZWJ text without retaining that text.
pub(super) struct Machine {
    left: Left,
    pending: [Option<Token>; 3],
    pending_len: usize,
    decoder: [u8; 4],
    decoder_len: usize,
    decoder_expected: usize,
    scalar_byte_start: u64,
    byte_offset: u64,
    utf16_offset: u64,
    scalar_count: u64,
    dictionary_required: bool,
    finished: bool,
    poisoned: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    /// Pending tokens before the original row start remain body-text obligations
    /// and cannot be discarded with the table interior.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn has_before_table(&self, byte: usize) -> bool {
        self.pending
            .first()
            .and_then(|token| *token)
            .is_some_and(|token| token.byte_start < byte as u64)
    }

    /// Settles pre-table rules one at a time through the original decision function
    /// and its existing actual lookahead only.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn drain_before_table(&mut self, byte: usize) -> Option<Break> {
        if !self.has_before_table(byte) {
            return None;
        }
        let mut event = None;
        self.consume_boundary(&mut |actual| event = Some(actual));
        event
    }

    /// A completed physical-row receipt proves hard-boundary context separation.
    /// Move the original machine to its real trailing control without inventing a break.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn begin_table_tail(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
    ) {
        self.left = Left::default();
        self.pending = [None; 3];
        self.pending_len = 0;
        self.decoder = [0; 4];
        self.decoder_len = 0;
        self.decoder_expected = 0;
        self.byte_offset = span.tail.byte as u64;
        self.utf16_offset = span.tail.utf16 as u64;
        self.scalar_byte_start = self.byte_offset;
    }
    /// Starts a fixed-size decoder/lookahead owner at real source offset zero,
    /// with no fabricated preceding token, dictionary result, or terminal boundary.
    pub(super) fn new() -> Self {
        Self {
            left: Left::default(),
            pending: [None; 3],
            pending_len: 0,
            decoder: [0; 4],
            decoder_len: 0,
            decoder_expected: 0,
            scalar_byte_start: 0,
            byte_offset: 0,
            utf16_offset: 0,
            scalar_count: 0,
            dictionary_required: false,
            finished: false,
            poisoned: false,
        }
    }
    /// Next raw source-byte coordinate, including any incomplete UTF-8 carry.
    pub(super) fn byte_offset(&self) -> u64 {
        self.byte_offset
    }
    /// Completed-scalar UTF-16 position; partial byte carry contributes no units.
    pub(super) fn utf16_offset(&self) -> u64 {
        self.utf16_offset
    }
    /// Counts actual decoded scalars so the outer job can charge each quantum.
    pub(super) fn scalar_count(&self) -> u64 {
        self.scalar_count
    }
    /// Reports encountered dictionary-requiring source text conservatively;
    /// default opportunities do not claim that segmentation has been performed.
    pub(super) fn needs_dictionary(&self) -> bool {
        self.dictionary_required
    }

    /// Only actual header CR/LF already consumed by the original decoder authorizes
    /// settling the preceding pending rules.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn fold_header_drained(&self) -> Result<bool, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.finished {
            return Err(Error::Closed);
        }
        if self.decoder_len != 0 {
            return Err(Error::InvalidUtf8);
        }
        let last = if self.pending_len == 0 {
            self.left.last
        } else {
            self.pending[self.pending_len - 1]
        };
        if !last.is_some_and(|token| matches!(token.meta.class, Cr | Lf)) {
            return Err(Error::OutOfOrder);
        }
        Ok(self.pending_len == 0)
    }

    /// The real line ending supplies preceding tokens with complete lookahead.
    /// Reuse original decisions without manufacturing EOF to fill the queue.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn drain_fold_header_one(&mut self) -> Result<Option<Break>, Error> {
        if self.fold_header_drained()? {
            return Ok(None);
        }
        let mut event = None;
        self.consume_boundary(&mut |value| event = Some(value));
        Ok(event)
    }

    /// A private same-source proof skips hidden text only between actual hard line
    /// endings. Its trailing bytes still require ordinary decoder input.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn resume_at_fold_tail(&mut self, span: FoldSpan) -> Result<(), Error> {
        if !self.fold_header_drained()?
            || self.byte_offset != span.header_end.byte as u64
            || self.utf16_offset != span.header_end.utf16 as u64
            || span.tail_start.byte <= span.header_end.byte
            || span.tail_start.byte > span.end.byte
            || span.tail_start.utf16 > span.end.utf16
            || span.tail_start.byte == span.end.byte && !span.eof
        {
            return Err(Error::OutOfOrder);
        }
        // Actual CR/LF cuts off all cross-paragraph suffix state. Unread hidden
        // text contributes neither classes nor dictionary-required conclusions.
        self.left = Left::default();
        self.pending = [None; 3];
        self.pending_len = 0;
        self.decoder = [0; 4];
        self.decoder_len = 0;
        self.decoder_expected = 0;
        self.byte_offset = span.tail_start.byte as u64;
        self.utf16_offset = span.tail_start.utf16 as u64;
        self.scalar_byte_start = self.byte_offset;
        Ok(())
    }

    /// Consumes exactly the earliest real effective token and its available
    /// lookahead, whether or not the ordered rule decision emits an opportunity.
    fn consume_boundary(&mut self, emit: &mut impl FnMut(Break)) {
        let token = self.pending[0].expect("固定队列只在有单元时消费");
        if let Some((kind, rule)) = decision(&self.left, token, self.pending[1], self.pending[2]) {
            emit(Break {
                byte_offset: token.byte_start,
                utf16_offset: token.utf16_start,
                kind,
                rule,
            });
        }
        self.left.advance(token);
        self.pending[0] = self.pending[1];
        self.pending[1] = self.pending[2];
        self.pending[2] = None;
        self.pending_len -= 1;
    }

    /// Resolves one complete scalar into finite effective-token context. LB9
    /// folds combining/ZWJ members into their base without a per-character queue.
    fn scalar(&mut self, character: char, emit: &mut impl FnMut(Break)) -> Result<(), Error> {
        let (mut meta, dictionary) = Meta::new(character);
        self.dictionary_required |= dictionary;
        let next_utf16 = self
            .utf16_offset
            .checked_add(character.len_utf16() as u64)
            .ok_or(Error::OffsetOverflow)?;
        let next_count = self
            .scalar_count
            .checked_add(1)
            .ok_or(Error::OffsetOverflow)?;
        let raw_zwj = meta.class == Zwj;
        if matches!(meta.class, Cm | Zwj) {
            if let Some(last) = self
                .pending_len
                .checked_sub(1)
                .and_then(|index| self.pending[index].as_mut())
                && !matches!(last.meta.class, Bk | Cr | Lf | Nl | Sp | Zw)
            {
                last.raw_last_zwj = raw_zwj; // LB9 never queues an internal boundary; retain raw ZWJ only for LB8a.
                self.utf16_offset = next_utf16;
                self.scalar_count = next_count;
                return Ok(());
            }
            meta = Meta::latin_a(); // An isolated mark at actual start or after hard break/SP/ZW uses full LB10.
        }
        self.pending[self.pending_len] = Some(Token {
            meta,
            byte_start: self.scalar_byte_start,
            utf16_start: self.utf16_offset,
            raw_last_zwj: raw_zwj,
        });
        self.pending_len += 1;
        self.utf16_offset = next_utf16;
        self.scalar_count = next_count;
        if self.pending_len == 3 {
            self.consume_boundary(emit);
        }
        Ok(())
    }

    /// Appends the actual global byte range. A chunk end preserves pending state
    /// and represents neither text start/end nor a property-context reset.
    /// A mid-chunk failure may follow valid prefix callbacks. The caller must revoke
    /// Ready authority for the entire failed construction;
    /// a prefix is not a complete index. Sticky poison prevents recovery with later
    /// chunks or substitution of a newer source.
    pub(super) fn push(
        &mut self,
        start_byte: u64,
        bytes: &[u8],
        mut emit: impl FnMut(Break),
    ) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.finished {
            return Err(Error::Closed);
        }
        if start_byte != self.byte_offset {
            // Gaps/replayed bytes revoke this machine rather than restarting with
            // fresh paragraph context over a different source interval.
            self.poisoned = true;
            return Err(Error::OutOfOrder);
        }
        let result = self.push_inner(bytes, &mut emit);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    /// Decodes only caller-bounded bytes, preserving incomplete UTF-8 across
    /// chunks and rejecting invalid starts/continuations before token publication.
    fn push_inner(&mut self, bytes: &[u8], emit: &mut impl FnMut(Break)) -> Result<(), Error> {
        for byte in bytes {
            let next_byte = self
                .byte_offset
                .checked_add(1)
                .ok_or(Error::OffsetOverflow)?;
            if self.decoder_len == 0 {
                self.scalar_byte_start = self.byte_offset;
                self.decoder_expected = match byte {
                    0..=0x7f => 1,
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => return Err(Error::InvalidUtf8), // Another chunk cannot repair this invalid leading byte into different text.
                };
            } else if !(0x80..=0xbf).contains(byte) {
                return Err(Error::InvalidUtf8);
            }
            self.decoder[self.decoder_len] = *byte;
            self.decoder_len += 1;
            self.byte_offset = next_byte;
            if self.decoder_len == self.decoder_expected {
                let value = std::str::from_utf8(&self.decoder[..self.decoder_len])
                    .map_err(|_| Error::InvalidUtf8)?;
                let character = value.chars().next().ok_or(Error::InvalidUtf8)?;
                self.decoder_len = 0;
                self.decoder_expected = 0;
                self.scalar(character, emit)?;
            } // Carry at most four incomplete scalar bytes; chunk end is never text end.
        }
        Ok(())
    }

    /// Actual EOF advances one emitted opportunity at a time. Flushing up to three
    /// pending events cannot bypass the caller's record budget.
    pub(super) fn finish_one(&mut self) -> Result<Option<Break>, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.finished {
            return Ok(None);
        }
        if self.decoder_len != 0 {
            // Actual EOF cannot repair an incomplete scalar. Revoke further use
            // instead of publishing a terminal opportunity after invalid text.
            self.poisoned = true;
            return Err(Error::InvalidUtf8);
        }
        let mut event = None;
        while self.pending_len != 0 && event.is_none() {
            self.consume_boundary(&mut |value| event = Some(value));
        }
        if event.is_some() {
            return Ok(event);
        }
        // Only real EOF after all pending decisions produces the single mandatory
        // terminal opportunity. Subsequent finish calls return no duplicate event.
        self.finished = true;
        Ok(Some(Break {
            byte_offset: self.byte_offset,
            utf16_offset: self.utf16_offset,
            kind: BreakKind::Mandatory,
            rule: "LB3",
        }))
    }

    /// True only after real EOF drained pending rules and emitted its terminal
    /// opportunity, never merely because the current input chunk ran out.
    pub(super) fn finished(&self) -> bool {
        self.finished
    }

    /// Boundaries before the earliest pending effective token are decided. That
    /// token's own start still has no Ready authority.
    pub(super) fn confirmed_offsets(&self) -> (u64, u64) {
        self.pending[0].map_or((self.byte_offset, self.utf16_offset), |value| {
            (value.byte_start, value.utf16_start)
        })
    }
}
