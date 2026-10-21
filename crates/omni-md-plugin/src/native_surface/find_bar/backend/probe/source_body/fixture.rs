//! All positions are fixed fixture facts. Stream one 4MiB Chinese paragraph in
//! blocks no larger than 4,095 bytes instead of materializing the whole text.

use std::io::Write;

pub(super) const UTF16: usize = 1_398_102;
pub(super) const BYTES: usize = 4_194_306;
pub(super) const FIRST: usize = 70_001;
pub(super) const SECOND: usize = 140_001;
pub(super) const LONG: usize = 9_000;
pub(super) const LONG_END_BYTES: usize = 237_003;
pub(super) const LOCAL_FIRST: usize = 2;
pub(super) const LOCAL_SECOND: usize = 64;

/// Fill exactly the requested count of fixed three-byte/one-UTF-16-unit scalars;
/// the caller owns the output and cleanup if any write fails.
pub(super) fn fill(file: &mut impl Write, mut units: usize) -> Result<(), String> {
    let mut block = [0_u8; 4095];
    for scalar in block.as_chunks_mut::<3>().0 {
        scalar.copy_from_slice(&[0xE4, 0xB8, 0xAD]); // Fixed U+4E2D, independent of the document/Source under test.
    }
    while units != 0 {
        let count = units.min(block.len() / 3);
        file.write_all(&block[..count * 3])
            .map_err(|cause| cause.to_string())?;
        units -= count;
    }
    Ok(())
}

/// Place independent local/remote markers while streaming filler. Verify the
/// actual output length before the original file becomes a mapped fixture.
pub(super) fn write(file: &mut tempfile::NamedTempFile) -> Result<(), String> {
    fill(file, LOCAL_FIRST)?;
    file.write_all("局部".as_bytes())
        .map_err(|cause| cause.to_string())?;
    fill(file, LOCAL_SECOND - LOCAL_FIRST - 2)?;
    file.write_all("局部".as_bytes())
        .map_err(|cause| cause.to_string())?;
    fill(file, FIRST - LOCAL_SECOND - 2)?;
    file.write_all("目标".as_bytes())
        .map_err(|cause| cause.to_string())?;
    fill(file, LONG - 4)?;
    file.write_all("终结".as_bytes())
        .map_err(|cause| cause.to_string())?;
    fill(file, SECOND - FIRST - LONG)?;
    file.write_all("目标".as_bytes())
        .map_err(|cause| cause.to_string())?;
    fill(file, UTF16 - SECOND - 2)?;
    file.flush().map_err(|cause| cause.to_string())?;
    if file
        .as_file()
        .metadata()
        .map_err(|cause| cause.to_string())?
        .len()
        != BYTES as u64
        || (FIRST + LONG) * 3 != LONG_END_BYTES
    {
        return Err("Find长文fixture的独立长度不符".into());
    }
    Ok(())
}

/// Construct only the explicit 9,000-unit query value. Its intentional bounded
/// allocation is separate from the streamed multi-megabyte document.
pub(super) fn long_query() -> String {
    let mut query = String::with_capacity(LONG * 3);
    query.push_str("目标");
    for _ in 0..LONG - 4 {
        query.push('中');
    }
    query.push_str("终结");
    query // The document still comes from fixed streamed blocks, never this query allocation.
}
