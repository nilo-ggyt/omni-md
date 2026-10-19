//! 默认UAX14 rev57流式机会生成器。它不负责宽度选行、词典或locale tailoring。

use super::{MarkdownLineBreakKind as BreakKind, unicode_data::Class};
use Class::*;

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
    fn new(character: char) -> (Self, bool) {
        let (class, flags) = super::unicode_data::properties(character);
        let dictionary = class == Sa && flags & 4 == 0;
        let class = match class {
            Ai | Sg | Xx => Al,
            Sa if flags & 4 != 0 => Cm,
            Sa => Al,
            Cj => Ns,
            other => other, // CB保留给LB20；不把默认断行当词典分析已经完成。
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
        // LB10是完整属性替换；不能让原孤立CM的EAW/引号/EP属性污染后续规则。
        Self {
            class: Al,
            pi: false,
            pf: false,
            east_asian: false,
            unassigned_pictograph: false,
            dotted_circle: false,
        }
    }
    fn syllable(self) -> bool {
        matches!(self.class, Ak | As) || self.dotted_circle
    }
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
        } // 任意长SP仅保留其前一个有效类别和初引号状态，不积累空白正文。
        self.numeric_closer = matches!(meta.class, Cl | Cp) && self.numeric_suffix;
        self.numeric_suffix =
            meta.class == Nu || (matches!(meta.class, Sy | Is) && self.numeric_suffix);
        self.odd_ri = meta.class == Ri && !self.odd_ri;
        self.previous = previous;
        self.last = Some(token);
    }
}

fn decision(
    left: &Left,
    right: Token,
    next: Option<Token>,
    third: Option<Token>,
) -> Option<(BreakKind, &'static str)> {
    let prior = left.last?; // LB2：真实sot不发机会。
    let l = prior.meta;
    let r = right.meta;
    let lc = l.class;
    let rc = r.class;
    let n = next.map(|value| value.meta);
    let nc = n.map(|value| value.class);
    let tc = third.map(|value| value.meta.class);
    let before = left.previous;
    let non_space = left.non_space.map(|meta| meta.class);
    if lc == Bk {
        return Some((BreakKind::Mandatory, "LB4"));
    }
    if lc == Cr && rc == Lf {
        return None;
    }
    if matches!(lc, Cr | Lf | Nl) {
        return Some((BreakKind::Mandatory, "LB5"));
    }
    if matches!(rc, Bk | Cr | Lf | Nl) {
        return None;
    } // LB6
    if matches!(rc, Sp | Zw) {
        return None;
    } // LB7先于ZW后可断规则。
    if non_space == Some(Zw) {
        return Some((BreakKind::Allowed, "LB8"));
    }
    if prior.raw_last_zwj {
        return None;
    } // LB8a必须使用原始末ZWJ，不能只看继承后的类。
    if lc == Wj || rc == Wj {
        return None;
    } // LB11
    if lc == Gl {
        return None;
    } // LB12
    if rc == Gl && !matches!(lc, Sp | Hy | Hh) {
        return None;
    } // LB12a
    if matches!(rc, Cl | Cp | Ex | Sy) {
        return None;
    } // LB13
    if non_space == Some(Op) {
        return None;
    } // LB14
    if non_space == Some(Qu) && left.initial_quote {
        return None;
    } // LB15a
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
    } // LB15b
    if lc == Sp && rc == Is && nc == Some(Nu) {
        return Some((BreakKind::Allowed, "LB15c"));
    }
    if rc == Is {
        return None;
    } // LB15d
    if matches!(non_space, Some(Cl | Cp)) && rc == Ns {
        return None;
    } // LB16
    if non_space == Some(B2) && rc == B2 {
        return None;
    } // LB17
    if lc == Sp {
        return Some((BreakKind::Allowed, "LB18"));
    }
    if (rc == Qu && !r.pi) || (lc == Qu && !l.pf) {
        return None;
    } // LB19
    if (!l.east_asian && rc == Qu)
        || (rc == Qu && n.is_none_or(|meta| !meta.east_asian))
        || (lc == Qu && !r.east_asian)
        || (lc == Qu && before.is_none_or(|meta| !meta.east_asian))
    {
        return None;
    } // LB19a
    if rc == Cb || lc == Cb {
        return Some((BreakKind::Allowed, "LB20"));
    }
    if matches!(lc, Hy | Hh)
        && matches!(rc, Al | Hl)
        && before.is_none_or(|meta| matches!(meta.class, Bk | Cr | Lf | Nl | Sp | Zw | Cb | Gl))
    {
        return None;
    } // LB20a
    if matches!(rc, Ba | Hh | Hy | Ns) || lc == Bb {
        return None;
    } // LB21
    if before.is_some_and(|meta| meta.class == Hl) && matches!(lc, Hy | Hh) && rc != Hl {
        return None;
    } // LB21a
    if lc == Sy && rc == Hl {
        return None;
    } // LB21b
    if rc == In {
        return None;
    } // LB22
    if (matches!(lc, Al | Hl) && rc == Nu) || (lc == Nu && matches!(rc, Al | Hl)) {
        return None;
    } // LB23
    if (lc == Pr && matches!(rc, Id | Eb | Em)) || (matches!(lc, Id | Eb | Em) && rc == Po) {
        return None;
    } // LB23a
    if (matches!(lc, Pr | Po) && matches!(rc, Al | Hl))
        || (matches!(lc, Al | Hl) && matches!(rc, Pr | Po))
    {
        return None;
    } // LB24
    if (matches!(rc, Po | Pr) && (left.numeric_suffix || left.numeric_closer))
        || (matches!(lc, Po | Pr)
            && rc == Op
            && (nc == Some(Nu) || (nc == Some(Is) && tc == Some(Nu))))
        || (matches!(lc, Po | Pr | Hy | Is) && rc == Nu)
        || (left.numeric_suffix && rc == Nu)
    {
        return None;
    } // LB25保持数字后缀状态，不缓存整个数字串。
    if (lc == Jl && matches!(rc, Jl | Jv | H2 | H3))
        || (matches!(lc, Jv | H2) && matches!(rc, Jv | Jt))
        || (matches!(lc, Jt | H3) && rc == Jt)
    {
        return None;
    } // LB26
    if (matches!(lc, Jl | Jv | Jt | H2 | H3) && rc == Po)
        || (lc == Pr && matches!(rc, Jl | Jv | Jt | H2 | H3))
    {
        return None;
    } // LB27
    if matches!(lc, Al | Hl) && matches!(rc, Al | Hl) {
        return None;
    } // LB28
    if (lc == Ap && r.syllable())
        || (l.syllable() && matches!(rc, Vf | Vi))
        || (before.is_some_and(Meta::syllable) && lc == Vi && r.ak_or_dotted())
        || (l.syllable() && r.syllable() && nc == Some(Vf))
    {
        return None;
    } // LB28a
    if lc == Is && matches!(rc, Al | Hl) {
        return None;
    } // LB29
    if (matches!(lc, Al | Hl | Nu) && rc == Op && !r.east_asian)
        || (lc == Cp && !l.east_asian && matches!(rc, Al | Hl | Nu))
    {
        return None;
    } // LB30
    if lc == Ri && rc == Ri && left.odd_ri {
        return None;
    } // LB30a
    if (lc == Eb || l.unassigned_pictograph) && rc == Em {
        return None;
    } // LB30b
    Some((BreakKind::Allowed, "LB31"))
}

/// 全部运行时状态固定大小；三个右侧有效单元可以跨任意长CM/ZWJ，但不保留这些正文。
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
    pub(super) fn byte_offset(&self) -> u64 {
        self.byte_offset
    }
    pub(super) fn utf16_offset(&self) -> u64 {
        self.utf16_offset
    }
    pub(super) fn scalar_count(&self) -> u64 {
        self.scalar_count
    }
    pub(super) fn needs_dictionary(&self) -> bool {
        self.dictionary_required
    }

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
                last.raw_last_zwj = raw_zwj; // LB9内部边界永不入队；只更新末原始ZWJ事实供LB8a。
                self.utf16_offset = next_utf16;
                self.scalar_count = next_count;
                return Ok(());
            }
            meta = Meta::latin_a(); // 真正sot/硬换行/SP/ZW后的孤立mark执行完整LB10。
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

    /// 追加真实全局字节范围；chunk末尾只保留待决状态，不代表sot/eot或属性上下文切换。
    /// 若输入中段失败，回调可能已看到合法前缀机会；调用方必须撤销整个失败构建的Ready
    /// 权限，不能把前缀当完整索引。内部poison禁止后续chunk恢复或重用较新的来源。
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
            self.poisoned = true;
            return Err(Error::OutOfOrder);
        }
        let result = self.push_inner(bytes, &mut emit);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

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
                    _ => return Err(Error::InvalidUtf8), // 无效lead不能被下一chunk修补为另一串。
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
            } // 未完整scalar只保留至多4字节；chunk尾绝不充当eot。
        }
        Ok(())
    }

    /// EOF仍逐真实机会推进；调用方的records配额不能被一次flush最多三个事件绕过。
    pub(super) fn finish_one(&mut self) -> Result<Option<Break>, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.finished {
            return Ok(None);
        }
        if self.decoder_len != 0 {
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
        self.finished = true;
        Ok(Some(Break {
            byte_offset: self.byte_offset,
            utf16_offset: self.utf16_offset,
            kind: BreakKind::Mandatory,
            rule: "LB3",
        }))
    }

    pub(super) fn finished(&self) -> bool {
        self.finished
    }

    /// 最早待决有效单元之前的所有边界均已判定；该单元起点本身尚无Ready权限。
    pub(super) fn confirmed_offsets(&self) -> (u64, u64) {
        self.pending[0].map_or((self.byte_offset, self.utf16_offset), |value| {
            (value.byte_start, value.utf16_start)
        })
    }
}
