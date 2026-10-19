// Copyright 2015 Google Inc. All rights reserved.
// 固定0.13.4的真实旧LineStart作prefix差分，MIT条款见LICENSE。
use super::*;
use crate::block_context::ContainerRole;
use crate::line_start_stream::oracle::OracleLineStart;

fn old_prefix(bytes: &[u8], roles: &[ContainerRole], options: Options) -> (LineStartState, usize) {
    let mut line = OracleLineStart::new(bytes);
    let mut matched = 0;
    for role in roles {
        // 保留原scan_container_role四分支作独立oracle，不调用新role_plan。
        let saved = line.clone();
        let okay = match *role {
            ContainerRole::Quote => {
                line.scan_space(3);
                line.scan_blockquote_marker()
            }
            ContainerRole::Item(indent) | ContainerRole::Definition(indent) => {
                line.scan_space(indent) || line.is_at_eol()
            }
            ContainerRole::Footnote if options.has_gfm_footnotes() => {
                line.scan_space(4) || line.is_at_eol()
            }
            _ => true,
        };
        if !okay {
            line = saved;
            break;
        }
        matched += 1;
    }
    (line.state(), matched)
}

#[test]
fn container_prefix_every_frame_and_step_matches_original_with_exact_rollback() {
    let role_sets = [
        vec![],
        vec![ContainerRole::Quote, ContainerRole::Other],
        vec![
            ContainerRole::Other,
            ContainerRole::Item(2),
            ContainerRole::Quote,
            ContainerRole::Other,
        ],
        vec![
            ContainerRole::Quote,
            ContainerRole::Footnote,
            ContainerRole::Item(1),
        ],
        vec![ContainerRole::Definition(3), ContainerRole::Quote],
    ];
    let lines: &[&[u8]] = &[
        b"",
        b"\r\n",
        b" >\t  body",
        b"\t>  \t| a |\r\n",
        b"   >    x",
        b"  | x |\n",
        b"\t\t   >x",
        "> 中🙂".as_bytes(),
    ];
    for roles in role_sets {
        for bytes in lines {
            for options in [
                Options::empty(),
                Options::ENABLE_FOOTNOTES,
                Options::ENABLE_OLD_FOOTNOTES | Options::ENABLE_FOOTNOTES,
            ] {
                let expected = old_prefix(bytes, &roles, options);
                for width in 1..=bytes.len().max(1) {
                    let spine = StructuralSpineSnapshot::from_roles(&roles);
                    let mut prefix = ContainerPrefix::new(7, 0, bytes.len(), &spine, options);
                    let mut complete = false;
                    for _ in 0..(bytes.len() * 32 + roles.len() * 32 + 256) {
                        let next = prefix.next_byte();
                        let end = (next + width).min(bytes.len());
                        let (progress, work) = prefix
                            .advance(
                                &SourceFrame {
                                    source_revision: 7,
                                    byte_start: next,
                                    bytes: &bytes[next..end],
                                    real_eof: end == bytes.len(),
                                },
                                1,
                                1,
                            )
                            .unwrap();
                        assert!(work.byte_reads <= 1 && work.transitions <= 1);
                        if let PrefixProgress::Complete { state, matched } = progress {
                            assert_eq!(
                                (state, matched),
                                expected,
                                "roles={roles:?} bytes={bytes:?} width={width}"
                            );
                            complete = true;
                            break;
                        }
                    }
                    assert!(complete);
                }
            }
        }
    }
}

#[test]
fn deep_other_spine_has_real_step_work_even_without_source_bytes() {
    let roles = vec![ContainerRole::Other; 16_384];
    let spine = StructuralSpineSnapshot::from_roles(&roles);
    let mut prefix = ContainerPrefix::new(1, 0, 0, &spine, Options::empty());
    let frame = SourceFrame {
        source_revision: 1,
        byte_start: 0,
        bytes: b"",
        real_eof: true,
    };
    let (first, work) = prefix.advance(&frame, 1, 1).unwrap();
    assert_eq!(first, PrefixProgress::Progress);
    assert_eq!(work.transitions, 1);
    let mut transitions = 1;
    loop {
        let (progress, work) = prefix.advance(&frame, 64, 64).unwrap();
        transitions += work.transitions;
        assert!(work.transitions <= 64);
        if let PrefixProgress::Complete { matched, .. } = progress {
            assert_eq!(matched, roles.len());
            break;
        }
        assert!(work.transitions > 0);
    }
    assert_eq!(transitions, 16_384 + 512 + 1);
}
