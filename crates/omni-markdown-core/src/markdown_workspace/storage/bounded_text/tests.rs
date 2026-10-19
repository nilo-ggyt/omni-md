//! 对照标准UTF8分类与所有短窗口切分，确保残余、非法尾部和真实EOF分别处理。
use super::*;

#[test]
fn every_split_matches_standard_utf8_including_incomplete_and_invalid_sequences() {
    let cases: &[&[u8]] = &[
        b"",
        b"ascii\0\r\n",
        "中文🙂e\u{301}العربية".as_bytes(),
        &[0xc2],
        &[0xe0, 0xa0],
        &[0xf0, 0x90, 0x80],
        &[0xc0, 0xaf],
        &[0xed, 0xa0, 0x80],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0x80],
        &[0xe2, 0x28, 0xa1],
        &[0xf0, 0x90, 0x80, b'a'],
    ];
    for bytes in cases {
        for first in 0..=bytes.len() {
            for second in first..=bytes.len() {
                let mut state = Utf8Continuation::default();
                let actual = state
                    .push(&bytes[..first])
                    .and_then(|()| state.push(&bytes[first..second]))
                    .and_then(|()| state.push(&bytes[second..]))
                    .and_then(|()| state.finish());
                assert_eq!(
                    actual.is_ok(),
                    std::str::from_utf8(bytes).is_ok(),
                    "{bytes:?} {first}/{second}"
                );
            }
        }
    }
}

#[test]
fn arbitrarily_many_valid_windows_keep_only_one_scalar_residual() {
    let window = "🙂".as_bytes();
    let mut state = Utf8Continuation::default();
    for _ in 0..100_000 {
        for byte in window {
            state.push(std::slice::from_ref(byte)).unwrap();
            assert!(state.used < 4);
        }
    }
    state.finish().unwrap();
}
