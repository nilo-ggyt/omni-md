//! Independent offline control calls the released crate's actual public API for each scalar,
//! rather than inferring runtime results solely from parsed source tables.

use std::fs::File;
use std::io::{BufWriter, Write};

use unicode_bidi::{BidiClass, BidiDataSource, HardcodedBidiData};

const SCALARS: u32 = 0x11_0000 - 0x800;
const RECORD_BYTES: u32 = 10;

fn class_code(class: BidiClass) -> u8 {
    // Enumerate all 23 encoded classes explicitly. An upstream enum change must fail
    // compilation instead of silently writing a default class into the control file.
    match class {
        BidiClass::AL => 0,
        BidiClass::AN => 1,
        BidiClass::B => 2,
        BidiClass::BN => 3,
        BidiClass::CS => 4,
        BidiClass::EN => 5,
        BidiClass::ES => 6,
        BidiClass::ET => 7,
        BidiClass::FSI => 8,
        BidiClass::L => 9,
        BidiClass::LRE => 10,
        BidiClass::LRI => 11,
        BidiClass::LRO => 12,
        BidiClass::NSM => 13,
        BidiClass::ON => 14,
        BidiClass::PDF => 15,
        BidiClass::PDI => 16,
        BidiClass::R => 17,
        BidiClass::RLE => 18,
        BidiClass::RLI => 19,
        BidiClass::RLO => 20,
        BidiClass::S => 21,
        BidiClass::WS => 22,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let path = arguments.next().ok_or("需要唯一二进制输出路径")?;
    if arguments.next().is_some() {
        return Err("只接受唯一输出路径".into());
    }
    let mut output = BufWriter::new(File::create(path)?);
    // The 40-byte header records the actual crate data version, count, and schema size.
    // The Python consumer rejects a partial file left by a failed export.
    output.write_all(b"OMUBIDI1")?;
    output.write_all(&unicode_bidi::UNICODE_VERSION.0.to_le_bytes())?;
    output.write_all(&unicode_bidi::UNICODE_VERSION.1.to_le_bytes())?;
    output.write_all(&unicode_bidi::UNICODE_VERSION.2.to_le_bytes())?;
    output.write_all(&SCALARS.to_le_bytes())?;
    output.write_all(&RECORD_BYTES.to_le_bytes())?;
    let data = HardcodedBidiData;
    let mut records = 0_u32;
    let mut brackets = 0_u32;
    let mut class_counts = [0_u32; 23];
    for point in 0..0x11_0000 {
        let Some(scalar) = char::from_u32(point) else {
            continue; // Surrogates are not Rust scalars; do not compare replacement characters in their place.
        };
        let class = class_code(data.bidi_class(scalar));
        let bracket = data.bidi_matched_opening_bracket(scalar);
        let mut record = [0_u8; RECORD_BYTES as usize];
        record[..4].copy_from_slice(&point.to_le_bytes());
        record[4] = class;
        if let Some(bracket) = bracket {
            record[5] = if bracket.is_open { 1 } else { 2 };
            record[6..].copy_from_slice(&(bracket.opening as u32).to_le_bytes());
            brackets += 1;
        } // No-bracket records retain zero tag/opening fields without inventing a U+0000 opening bracket.
        output.write_all(&record)?;
        class_counts[class as usize] += 1;
        records += 1;
    }
    if records != SCALARS {
        return Err("实际标量覆盖数不完整".into());
    }
    output.flush()?; // Report completion only after all actual API records reach the output stream.
    eprintln!(
        "public_api_export_complete scalars={records} bracket_entries={brackets} record_bytes={RECORD_BYTES} data_version={:?} class_counts={class_counts:?}",
        unicode_bidi::UNICODE_VERSION,
    );
    Ok(())
}
