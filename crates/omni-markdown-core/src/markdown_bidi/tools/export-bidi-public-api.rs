//! 独立离线控制：逐标量调用实际发布crate的公开API，不从源码表推测运行结果。

use std::fs::File;
use std::io::{BufWriter, Write};

use unicode_bidi::{BidiClass, BidiDataSource, HardcodedBidiData};

const SCALARS: u32 = 0x11_0000 - 0x800;
const RECORD_BYTES: u32 = 10;

fn class_code(class: BidiClass) -> u8 {
    // 文件编码显式列出23类；crate新增或修改枚举时必须编译失败而非误写默认类。
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
    // 40B头明确原crate数据版本、记录数和schema大小；失败的半文件不会通过Python验证。
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
            continue; // 代理区不是Rust scalar，不能用replacement character代替后比较。
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
        } // 无括号的tag和opening保持明确的零值，不冒充实际U+0000开括号。
        output.write_all(&record)?;
        class_counts[class as usize] += 1;
        records += 1;
    }
    if records != SCALARS {
        return Err("实际标量覆盖数不完整".into());
    }
    output.flush()?; // 只有所有公开API记录实际落入输出流，才发布完成统计。
    eprintln!(
        "public_api_export_complete scalars={records} bracket_entries={brackets} record_bytes={RECORD_BYTES} data_version={:?} class_counts={class_counts:?}",
        unicode_bidi::UNICODE_VERSION,
    );
    Ok(())
}
