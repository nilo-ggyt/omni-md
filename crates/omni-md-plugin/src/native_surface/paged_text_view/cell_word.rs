//! 原真实格命中的选词查询意图；只有后继最新 Row/Cell 发布才能授予实际显示源读取权。

use super::NativeMarkdownPagedHeadAffinity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 行首、列和点击亲和只说明本次要查询哪一格，不持旧 Source、正文或 publication 强票。
/// 提交使 canonical 位置换代时，调用方只沿同次真实 edit map 移动 row_start 与查询 head。
pub struct NativeMarkdownPagedCellWordTarget {
    pub row_start: usize,
    pub column: usize,
    pub affinity: NativeMarkdownPagedHeadAffinity,
}
