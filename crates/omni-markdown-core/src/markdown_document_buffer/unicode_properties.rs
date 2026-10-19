//! 在建叶的原标量遍历中附记精确Unicode属性；不保存排版策略或派生算法权限。

use crate::unicode_bidi_data::UnicodeBidiData;
use unicode_bidi::{BidiClass, BidiDataSource};

// 与固定Unicode 18 provider相同的完整ASCII属性，编译期生成只读512B表；
// 没有lazy初始化、堆或样本推断，避免原建叶遍历对每个ASCII字节重复二分两张范围表。
static ASCII_BIDI_MASKS: [u32; 128] = ascii_bidi_masks();

const fn ascii_bidi_class(byte: u8) -> BidiClass {
    match byte {
        0x00..=0x08 | 0x0e..=0x1b | 0x7f => BidiClass::BN,
        0x09 | 0x0b | 0x1f => BidiClass::S,
        0x0a | 0x0d | 0x1c..=0x1e => BidiClass::B,
        0x0c | b' ' => BidiClass::WS,
        b'#'..=b'%' => BidiClass::ET,
        b'+' | b'-' => BidiClass::ES,
        b',' | b'.' | b'/' | b':' => BidiClass::CS,
        b'0'..=b'9' => BidiClass::EN,
        b'A'..=b'Z' | b'a'..=b'z' => BidiClass::L,
        _ => BidiClass::ON, // 完整0..127域中其余标点；构表循环绝不传入非ASCII值。
    }
}

const fn ascii_bidi_masks() -> [u32; 128] {
    let mut masks = [0; 128];
    let mut index = 0;
    while index < masks.len() {
        // 同一显式class→bit映射供所有字符共用，不能因ASCII快路建立另一套集合位序。
        masks[index] = MarkdownUnicodeProperties::class_mask(ascii_bidi_class(index as u8));
        index += 1;
    }
    masks
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MarkdownUnicodeProperties {
    pub(super) bidi_classes: u32,
    // 六位分别对应TAB/CR/LF/NEL/LS/PS；空文本两组集合均为空，是OR合并的单位元。
    pub(super) separators: u8,
    // 可打印ASCII真实包络；0表示空集合。两字节利用原u32对齐余量，不存完整字符位集。
    pub(super) printable_ascii_min: u8,
    pub(super) printable_ascii_max: u8,
}

impl MarkdownUnicodeProperties {
    pub(super) fn observe(&mut self, scalar: char) {
        self.bidi_classes |= if scalar.is_ascii() {
            // 只有精确ASCII域可直接索引；128个属性全部与同一个Unicode18 provider差分。
            ASCII_BIDI_MASKS[scalar as usize]
        } else {
            // 非ASCII仍与实际UBA共用固定数据，未知/新分配字符不能按Rust字符类别猜方向。
            Self::class_mask(UnicodeBidiData.bidi_class(scalar))
        };
        self.separators |= Self::separator_mask(scalar);
        if (' '..='~').contains(&scalar) {
            let byte = scalar as u8;
            self.printable_ascii_min = if self.printable_ascii_min == 0 {
                byte
            } else {
                self.printable_ascii_min.min(byte)
            };
            self.printable_ascii_max = self.printable_ascii_max.max(byte);
        } // 非ASCII和控制不贡献包络；完整范围是否可打印仍由原三态单独证明。
    }

    pub(super) fn joined(self, other: Self) -> Self {
        // 两组集合仅记录真实存在性，逐位OR具有结合律，拆叶、旋转与历史共享不改语义。
        Self {
            bidi_classes: self.bidi_classes | other.bidi_classes,
            separators: self.separators | other.separators,
            printable_ascii_min: match (self.printable_ascii_min, other.printable_ascii_min) {
                (0, right) => right,
                (left, 0) => left,
                (left, right) => left.min(right),
            }, // 空集合保持合并单位元，不能用0扩大真实ASCII下界。
            printable_ascii_max: self.printable_ascii_max.max(other.printable_ascii_max),
        }
    }

    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn bidi_classes(self) -> u32 {
        self.bidi_classes
    }

    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn separators(self) -> u8 {
        self.separators
    }

    pub(crate) const fn class_mask(class: BidiClass) -> u32 {
        // 显式映射全部23种实际Bidi_Class；不依赖第三方enum内存布局或隐式判别值。
        let bit = match class {
            BidiClass::L => 0,
            BidiClass::R => 1,
            BidiClass::AL => 2,
            BidiClass::EN => 3,
            BidiClass::ES => 4,
            BidiClass::ET => 5,
            BidiClass::AN => 6,
            BidiClass::CS => 7,
            BidiClass::NSM => 8,
            BidiClass::BN => 9,
            BidiClass::B => 10,
            BidiClass::S => 11,
            BidiClass::WS => 12,
            BidiClass::ON => 13,
            BidiClass::LRE => 14,
            BidiClass::LRO => 15,
            BidiClass::RLE => 16,
            BidiClass::RLO => 17,
            BidiClass::PDF => 18,
            BidiClass::LRI => 19,
            BidiClass::RLI => 20,
            BidiClass::FSI => 21,
            BidiClass::PDI => 22,
        };
        1_u32 << bit
    }

    const fn separator_mask(scalar: char) -> u8 {
        match scalar {
            '\t' => 1 << 0,
            '\r' => 1 << 1,
            '\n' => 1 << 2,
            '\u{0085}' => 1 << 3,
            '\u{2028}' => 1 << 4,
            '\u{2029}' => 1 << 5,
            _ => 0, // 其它S/B/Control仍保留其真实Bidi类，不擅自扩大这六个精确字符集合。
        }
    }
}

#[cfg(test)]
#[path = "unicode_properties/tests.rs"]
mod tests;
