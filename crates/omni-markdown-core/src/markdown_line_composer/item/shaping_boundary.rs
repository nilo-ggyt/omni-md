//! 原 UBA 标量级自然整形分界；它与编辑/断行 GCB 是两种不同事实。

use super::*;
use crate::markdown_bidi::MarkdownBidiShapingUnit;

/// 只有同一原 Bidi owner 实际读到的相邻记录可构造；公开范围不能补成此票。
/// 不保 Bidi、Source 或新目录强边，原 Source authority 与请求 nonce 继续负责后继准入。
#[derive(Clone)]
pub(crate) struct ResolvedBidiShapingSeam {
    source: MarkdownLayoutTextAuthority,
    byte: usize,
    utf16: usize,
    left_level: u8,
    right_level: u8,
}

impl ResolvedBidiShapingSeam {
    /// 原扫描只能在确实相邻、不同 level 的真实标量头封口；低代理项永远没有此能力。
    pub(super) fn from_adjacent_units(
        source: &MarkdownLayoutTextAuthority,
        left: &MarkdownBidiShapingUnit,
        right: &MarkdownBidiShapingUnit,
    ) -> Result<Self> {
        if !left.permits(source) || !right.permits(source) || !left.precedes_different_level(right)
        {
            return Err(MarkdownLineComposeError::InconsistentCoverage);
        }
        Ok(Self {
            source: source.clone(),
            byte: right.byte,
            utf16: right.utf16,
            left_level: left.level,
            right_level: right.level,
        })
    }

    /// 返回原 byte/UTF16 两套标量坐标；这仍不是字素边界，不能用它签 caret 或 LineEnd。
    pub(crate) fn boundary(&self, source: &MarkdownLayoutTextAuthority) -> Result<(usize, usize)> {
        if &self.source != source {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        Ok((self.byte, self.utf16))
    }

    /// 保留真实两侧 UBA level；消费方不得通过合并同奇偶方向来改写原视觉顺序。
    pub(crate) fn levels(&self) -> (u8, u8) {
        (self.left_level, self.right_level)
    }
}

/// 两种真实端点只共同授权本地 shaper 坐标，只有 Grapheme 分支还具有原编辑 GCB 事实。
/// 保留额外 seam 是为了裁掉不同 level 的上下文，不能因它恰逢 GCB 就丢失 UBA 屏障。
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) enum CertifiedShapingEndpoint {
    Grapheme {
        proof: crate::markdown_layout_breaks::MarkdownCertifiedGraphemeEndpoint,
        seam: Option<ResolvedBidiShapingSeam>,
    },
    Bidi(ResolvedBidiShapingSeam),
}

#[cfg(feature = "paged-shaping-frontier")]
impl CertifiedShapingEndpoint {
    /// 返回值只有坐标含义；必须继续携带原枚举，不能凭这个结构向 GCB 目录登记新记录。
    pub(crate) fn boundary(
        &self,
        source: &MarkdownLayoutTextAuthority,
    ) -> std::result::Result<crate::MarkdownGraphemeBoundary, MarkdownLayoutBreakError> {
        match self {
            Self::Grapheme { proof, seam } => {
                let point = proof.boundary(source)?;
                if let Some(seam) = seam
                    && seam
                        .boundary(source)
                        .map_err(|_| MarkdownLayoutBreakError::SourceMismatch)?
                        != (point.byte, point.utf16)
                {
                    return Err(MarkdownLayoutBreakError::SourceMismatch);
                }
                Ok(point)
            }
            Self::Bidi(seam) => {
                let (byte, utf16) = seam
                    .boundary(source)
                    .map_err(|_| MarkdownLayoutBreakError::SourceMismatch)?;
                Ok(crate::MarkdownGraphemeBoundary { byte, utf16 })
            }
        }
    }

    /// 每个端点必须真正属于原 Source；两端均由 GCB 签发时还须同一原 BreakJob scope。
    /// Bidi seam 则由唯一 Style nonce/取消 scope 保有，不能要求它冒充另一个索引的 GCB 票。
    pub(crate) fn require_pair(
        &self,
        other: &Self,
        source: &MarkdownLayoutTextAuthority,
    ) -> std::result::Result<(), MarkdownLayoutBreakError> {
        self.boundary(source)?;
        other.boundary(source)?;
        if let (Self::Grapheme { proof: left, .. }, Self::Grapheme { proof: right, .. }) =
            (self, other)
            && !left.same_scope(right)
        {
            return Err(MarkdownLayoutBreakError::SourceMismatch);
        }
        Ok(())
    }

    pub(crate) fn seam(&self) -> Option<&ResolvedBidiShapingSeam> {
        match self {
            Self::Grapheme { seam, .. } => seam.as_ref(),
            Self::Bidi(seam) => Some(seam),
        }
    }

    pub(crate) fn is_grapheme(&self) -> bool {
        matches!(self, Self::Grapheme { .. })
    }
}
