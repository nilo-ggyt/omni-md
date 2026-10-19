//! 官方UCD属性增量；只修正稳定unicode-bidi 0.3.18的数据，不改变UBA规则。
//! 数据版本18.0.0；必须与生成时同一发布crate的hardcoded-data一起使用。
//! 生成器generate-bidi-data-delta.py；UnicodeData仅用于BD16开括号规范等价类。
// SPDX-License-Identifier: Unicode-3.0
// UNICODE LICENSE V3
//
// COPYRIGHT AND PERMISSION NOTICE
//
// Copyright © 1991-2026 Unicode, Inc.
//
// NOTICE TO USER: Carefully read the following legal agreement. BY
// DOWNLOADING, INSTALLING, COPYING OR OTHERWISE USING DATA FILES, AND/OR
// SOFTWARE, YOU UNEQUIVOCALLY ACCEPT, AND AGREE TO BE BOUND BY, ALL OF THE
// TERMS AND CONDITIONS OF THIS AGREEMENT. IF YOU DO NOT AGREE, DO NOT
// DOWNLOAD, INSTALL, COPY, DISTRIBUTE OR USE THE DATA FILES OR SOFTWARE.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of data files and any associated documentation (the "Data Files") or
// software and any associated documentation (the "Software") to deal in the
// Data Files or Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, and/or sell
// copies of the Data Files or Software, and to permit persons to whom the
// Data Files or Software are furnished to do so, provided that either (a)
// this copyright and permission notice appear with all copies of the Data
// Files or Software, or (b) this copyright and permission notice appear in
// associated Documentation.
//
// THE DATA FILES AND SOFTWARE ARE PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
// KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF
// THIRD PARTY RIGHTS.
//
// IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS INCLUDED IN THIS NOTICE
// BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT OR CONSEQUENTIAL DAMAGES,
// OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
// WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
// ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THE DATA
// FILES OR SOFTWARE.
//
// Except as contained in this notice, the name of a copyright holder shall
// not be used in advertising or otherwise to promote the sale, use or other
// dealings in these Data Files or Software without prior written
// authorization of the copyright holder.

// DerivedBidiClass-18.0.0.txt SHA256 d9e23222522551348ea1ccfbb4f62efbf98982afb95840f8959c08ed992c5607
// BidiBrackets-18.0.0.txt SHA256 4b3b62e4a14b84ee752808c810c602534921c09a4a1bf78cfbee566d66c125b3
// UnicodeData-18.0.0.txt SHA256 0736451de439ae7baf1425136617da495e09ee5afbe6e394374db7009ea08950
// ReadMe-18.0.0.txt SHA256 b0ea442f29dee90584aacd3665a6e1ce87ffc917a4d31f34e1916d2d6f7f9205
// tables.rs SHA256 8cbe4b6a4fc690e76107d4654047d3f0d8988b870fc3234f76dc265ad2e2a9ca
// bidi-public-api.bin SHA256 f52e26c72879336cd0ecda79b55eec7969322cb8506d3be845aa55f743930a61

use unicode_bidi::data_source::BidiMatchedOpeningBracket;
use unicode_bidi::{BidiClass, BidiDataSource, HardcodedBidiData};

pub const UNICODE_VERSION: (u64, u64, u64) = (18, 0, 0);
// crate原数据更新后必须重新生成增量，不能把旧差异覆盖到另一套基线。
const _: () = assert!(
    unicode_bidi::UNICODE_VERSION.0 == 16
        && unicode_bidi::UNICODE_VERSION.1 == 0
        && unicode_bidi::UNICODE_VERSION.2 == 0
);

pub struct UnicodeBidiData;

impl BidiDataSource for UnicodeBidiData {
    fn bidi_class(&self, scalar: char) -> BidiClass {
        let point = scalar as u32;
        // 增量区间完整覆盖所有实际差异；其余标量继续使用已固定的发布表。
        let slot = CLASS_DELTAS.partition_point(|&(_, end, _)| end < point);
        match CLASS_DELTAS.get(slot) {
            Some(&(start, _, value)) if start <= point => value,
            _ => HardcodedBidiData.bidi_class(scalar),
        }
    }

    fn bidi_matched_opening_bracket(&self, scalar: char) -> Option<BidiMatchedOpeningBracket> {
        // 显式None表示新数据删除该括号，不能和没有增量时的旧表回退混淆。
        match BRACKET_DELTAS.binary_search_by_key(&scalar, |&(point, _)| point) {
            Ok(slot) => BRACKET_DELTAS[slot]
                .1
                .map(|(opening, is_open)| BidiMatchedOpeningBracket { opening, is_open }),
            Err(_) => HardcodedBidiData.bidi_matched_opening_bracket(scalar),
        }
    }
}

const CLASS_DELTAS: &[(u32, u32, BidiClass)] = &[
    (0x5C8, 0x5C9, BidiClass::NSM),
    (0x86B, 0x86F, BidiClass::AL),
    (0x88F, 0x88F, BidiClass::AL),
    (0x892, 0x896, BidiClass::AL),
    (0xB53, 0xB54, BidiClass::NSM),
    (0x1ACF, 0x1AF0, BidiClass::NSM),
    (0x1B3A, 0x1B3A, BidiClass::L),
    (0x1B3C, 0x1B3C, BidiClass::L),
    (0x1B42, 0x1B42, BidiClass::L),
    (0x2065, 0x2065, BidiClass::BN),
    (0x208F, 0x208F, BidiClass::ON),
    (0x2B96, 0x2B96, BidiClass::ON),
    (0x2E60, 0x2E63, BidiClass::ON),
    (0xFBC3, 0xFBD2, BidiClass::ON),
    (0xFD90, 0xFD91, BidiClass::ON),
    (0xFDC8, 0xFDCE, BidiClass::ON),
    (0xFDD0, 0xFDEF, BidiClass::BN),
    (0xFFF0, 0xFFF8, BidiClass::BN),
    (0xFFFE, 0xFFFF, BidiClass::BN),
    (0x10D28, 0x10D2F, BidiClass::AL),
    (0x10D3A, 0x10D3F, BidiClass::AL),
    (0x10EC0, 0x10EC1, BidiClass::AL),
    (0x10EC5, 0x10ECA, BidiClass::AL),
    (0x10ECB, 0x10ECF, BidiClass::NSM),
    (0x10ED0, 0x10ED8, BidiClass::ON),
    (0x10ED9, 0x10EEF, BidiClass::AL),
    (0x10EF0, 0x10EFB, BidiClass::NSM),
    (0x10F5A, 0x10F6F, BidiClass::AL),
    (0x11B60, 0x11B60, BidiClass::NSM),
    (0x11B62, 0x11B64, BidiClass::NSM),
    (0x11B66, 0x11B66, BidiClass::NSM),
    (0x11DF0, 0x11DF0, BidiClass::NSM),
    (0x1CCFA, 0x1CCFC, BidiClass::ON),
    (0x1CEBA, 0x1CED0, BidiClass::ON),
    (0x1CED2, 0x1CED4, BidiClass::ON),
    (0x1CEDD, 0x1CEFD, BidiClass::ON),
    (0x1D127, 0x1D128, BidiClass::NSM),
    (0x1D25B, 0x1D25C, BidiClass::NSM),
    (0x1DB00, 0x1DB1C, BidiClass::ON),
    (0x1E6E3, 0x1E6E3, BidiClass::NSM),
    (0x1E6E6, 0x1E6E6, BidiClass::NSM),
    (0x1E6EE, 0x1E6EF, BidiClass::NSM),
    (0x1E6F5, 0x1E6F5, BidiClass::NSM),
    (0x1EC70, 0x1EC70, BidiClass::AL),
    (0x1ECB5, 0x1ECBF, BidiClass::AL),
    (0x1ED00, 0x1ED00, BidiClass::AL),
    (0x1ED3E, 0x1ED4F, BidiClass::AL),
    (0x1F6D8, 0x1F6D9, BidiClass::ON),
    (0x1F777, 0x1F77A, BidiClass::ON),
    (0x1F7DB, 0x1F7DB, BidiClass::ON),
    (0x1F7F1, 0x1F7FF, BidiClass::ON),
    (0x1F8D0, 0x1F8D8, BidiClass::ON),
    (0x1FA54, 0x1FA57, BidiClass::ON),
    (0x1FA8A, 0x1FA8E, BidiClass::ON),
    (0x1FAC8, 0x1FAC8, BidiClass::ON),
    (0x1FACC, 0x1FACD, BidiClass::ON),
    (0x1FADD, 0x1FADD, BidiClass::ON),
    (0x1FAEA, 0x1FAEB, BidiClass::ON),
    (0x1FAEF, 0x1FAEF, BidiClass::ON),
    (0x1FAF9, 0x1FAFA, BidiClass::ON),
    (0x1FBFA, 0x1FBFA, BidiClass::ON),
    (0x1FFFE, 0x1FFFF, BidiClass::BN),
    (0x2FFFE, 0x2FFFF, BidiClass::BN),
    (0x3FFFE, 0x3FFFF, BidiClass::BN),
    (0x4FFFE, 0x4FFFF, BidiClass::BN),
    (0x5FFFE, 0x5FFFF, BidiClass::BN),
    (0x6FFFE, 0x6FFFF, BidiClass::BN),
    (0x7FFFE, 0x7FFFF, BidiClass::BN),
    (0x8FFFE, 0x8FFFF, BidiClass::BN),
    (0x9FFFE, 0x9FFFF, BidiClass::BN),
    (0xAFFFE, 0xAFFFF, BidiClass::BN),
    (0xBFFFE, 0xBFFFF, BidiClass::BN),
    (0xCFFFE, 0xCFFFF, BidiClass::BN),
    (0xDFFFE, 0xE0000, BidiClass::BN),
    (0xE0002, 0xE001F, BidiClass::BN),
    (0xE0080, 0xE00FF, BidiClass::BN),
    (0xE01F0, 0xE0FFF, BidiClass::BN),
    (0xEFFFE, 0xEFFFF, BidiClass::BN),
    (0xFFFFE, 0xFFFFF, BidiClass::BN),
    (0x10FFFE, 0x10FFFF, BidiClass::BN),
];

const BRACKET_DELTAS: &[(char, Option<(char, bool)>)] = &[
    ('\u{2E62}', Some(('\u{2E62}', true))),
    ('\u{2E63}', Some(('\u{2E62}', false))),
];

// 以下逐区间原属性仅供离线控制和边界测试；生产查询不创建额外表或堆对象。
#[cfg(test)]
pub const CHANGED_BOUNDARIES: &[(u32, u32, BidiClass, BidiClass)] = &[
    (0x5C8, 0x5C9, BidiClass::R, BidiClass::NSM),
    (0x86B, 0x86F, BidiClass::R, BidiClass::AL),
    (0x88F, 0x88F, BidiClass::R, BidiClass::AL),
    (0x892, 0x896, BidiClass::R, BidiClass::AL),
    (0xB53, 0xB54, BidiClass::L, BidiClass::NSM),
    (0x1ACF, 0x1AF0, BidiClass::L, BidiClass::NSM),
    (0x1B3A, 0x1B3A, BidiClass::NSM, BidiClass::L),
    (0x1B3C, 0x1B3C, BidiClass::NSM, BidiClass::L),
    (0x1B42, 0x1B42, BidiClass::NSM, BidiClass::L),
    (0x2065, 0x2065, BidiClass::L, BidiClass::BN),
    (0x208F, 0x208F, BidiClass::L, BidiClass::ON),
    (0x2B96, 0x2B96, BidiClass::L, BidiClass::ON),
    (0x2E60, 0x2E63, BidiClass::L, BidiClass::ON),
    (0xFBC3, 0xFBD2, BidiClass::AL, BidiClass::ON),
    (0xFD90, 0xFD91, BidiClass::AL, BidiClass::ON),
    (0xFDC8, 0xFDCE, BidiClass::AL, BidiClass::ON),
    (0xFDD0, 0xFDEF, BidiClass::L, BidiClass::BN),
    (0xFFF0, 0xFFF8, BidiClass::L, BidiClass::BN),
    (0xFFFE, 0xFFFF, BidiClass::L, BidiClass::BN),
    (0x10D28, 0x10D2F, BidiClass::R, BidiClass::AL),
    (0x10D3A, 0x10D3F, BidiClass::R, BidiClass::AL),
    (0x10EC0, 0x10EC1, BidiClass::R, BidiClass::AL),
    (0x10EC5, 0x10ECA, BidiClass::R, BidiClass::AL),
    (0x10ECB, 0x10ECF, BidiClass::R, BidiClass::NSM),
    (0x10ED0, 0x10ED8, BidiClass::R, BidiClass::ON),
    (0x10ED9, 0x10EEF, BidiClass::R, BidiClass::AL),
    (0x10EF0, 0x10EFB, BidiClass::R, BidiClass::NSM),
    (0x10F5A, 0x10F6F, BidiClass::R, BidiClass::AL),
    (0x11B60, 0x11B60, BidiClass::L, BidiClass::NSM),
    (0x11B62, 0x11B64, BidiClass::L, BidiClass::NSM),
    (0x11B66, 0x11B66, BidiClass::L, BidiClass::NSM),
    (0x11DF0, 0x11DF0, BidiClass::L, BidiClass::NSM),
    (0x1CCFA, 0x1CCFC, BidiClass::L, BidiClass::ON),
    (0x1CEBA, 0x1CED0, BidiClass::L, BidiClass::ON),
    (0x1CED2, 0x1CED4, BidiClass::L, BidiClass::ON),
    (0x1CEDD, 0x1CEFD, BidiClass::L, BidiClass::ON),
    (0x1D127, 0x1D128, BidiClass::L, BidiClass::NSM),
    (0x1D25B, 0x1D25C, BidiClass::L, BidiClass::NSM),
    (0x1DB00, 0x1DB1C, BidiClass::L, BidiClass::ON),
    (0x1E6E3, 0x1E6E3, BidiClass::L, BidiClass::NSM),
    (0x1E6E6, 0x1E6E6, BidiClass::L, BidiClass::NSM),
    (0x1E6EE, 0x1E6EF, BidiClass::L, BidiClass::NSM),
    (0x1E6F5, 0x1E6F5, BidiClass::L, BidiClass::NSM),
    (0x1EC70, 0x1EC70, BidiClass::R, BidiClass::AL),
    (0x1ECB5, 0x1ECBF, BidiClass::R, BidiClass::AL),
    (0x1ED00, 0x1ED00, BidiClass::R, BidiClass::AL),
    (0x1ED3E, 0x1ED4F, BidiClass::R, BidiClass::AL),
    (0x1F6D8, 0x1F6D9, BidiClass::L, BidiClass::ON),
    (0x1F777, 0x1F77A, BidiClass::L, BidiClass::ON),
    (0x1F7DB, 0x1F7DB, BidiClass::L, BidiClass::ON),
    (0x1F7F1, 0x1F7FF, BidiClass::L, BidiClass::ON),
    (0x1F8D0, 0x1F8D8, BidiClass::L, BidiClass::ON),
    (0x1FA54, 0x1FA57, BidiClass::L, BidiClass::ON),
    (0x1FA8A, 0x1FA8E, BidiClass::L, BidiClass::ON),
    (0x1FAC8, 0x1FAC8, BidiClass::L, BidiClass::ON),
    (0x1FACC, 0x1FACD, BidiClass::L, BidiClass::ON),
    (0x1FADD, 0x1FADD, BidiClass::L, BidiClass::ON),
    (0x1FAEA, 0x1FAEB, BidiClass::L, BidiClass::ON),
    (0x1FAEF, 0x1FAEF, BidiClass::L, BidiClass::ON),
    (0x1FAF9, 0x1FAFA, BidiClass::L, BidiClass::ON),
    (0x1FBFA, 0x1FBFA, BidiClass::L, BidiClass::ON),
    (0x1FFFE, 0x1FFFF, BidiClass::L, BidiClass::BN),
    (0x2FFFE, 0x2FFFF, BidiClass::L, BidiClass::BN),
    (0x3FFFE, 0x3FFFF, BidiClass::L, BidiClass::BN),
    (0x4FFFE, 0x4FFFF, BidiClass::L, BidiClass::BN),
    (0x5FFFE, 0x5FFFF, BidiClass::L, BidiClass::BN),
    (0x6FFFE, 0x6FFFF, BidiClass::L, BidiClass::BN),
    (0x7FFFE, 0x7FFFF, BidiClass::L, BidiClass::BN),
    (0x8FFFE, 0x8FFFF, BidiClass::L, BidiClass::BN),
    (0x9FFFE, 0x9FFFF, BidiClass::L, BidiClass::BN),
    (0xAFFFE, 0xAFFFF, BidiClass::L, BidiClass::BN),
    (0xBFFFE, 0xBFFFF, BidiClass::L, BidiClass::BN),
    (0xCFFFE, 0xCFFFF, BidiClass::L, BidiClass::BN),
    (0xDFFFE, 0xE0000, BidiClass::L, BidiClass::BN),
    (0xE0002, 0xE001F, BidiClass::L, BidiClass::BN),
    (0xE0080, 0xE00FF, BidiClass::L, BidiClass::BN),
    (0xE01F0, 0xE0FFF, BidiClass::L, BidiClass::BN),
    (0xEFFFE, 0xEFFFF, BidiClass::L, BidiClass::BN),
    (0xFFFFE, 0xFFFFF, BidiClass::L, BidiClass::BN),
    (0x10FFFE, 0x10FFFF, BidiClass::L, BidiClass::BN),
];
