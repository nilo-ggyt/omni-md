// Copyright 2015 Google Inc. All rights reserved.
// 原 unescape 有限流出口；完整 MIT 许可见 LICENSE。
//! 只有字节归一化，不证明块、隐藏或 Source 权限；标题 owner 继续保原事实及取消。

#[derive(Debug)]
pub struct InlineTextDecoder {
    stream: crate::scanners::UnescapeStream,
}

#[derive(Clone, Copy, Debug)]
pub struct InlineTextDecodeStep {
    pub consumed: usize,
    pub written: usize,
    pub transitions: usize,
    pub complete: bool,
}

impl Default for InlineTextDecoder {
    fn default() -> Self {
        Self {
            stream: crate::scanners::UnescapeStream::new(false),
        }
    }
}

impl InlineTextDecoder {
    /// Parser 与 URL 共用同一实体/反斜杠内核；固定 carry 只保一个实体及至多三个 UTF8 字节。
    /// real_end 来自已证语义区间末端，页面末端不能提前刷新未完成实体。
    pub fn feed(
        &mut self,
        input: &[u8],
        real_end: bool,
        output: &mut [u8],
        budget: usize,
    ) -> Result<InlineTextDecodeStep, super::GlobalFrontierError> {
        if output.len() < 8 || !(2048..=64 * 1024).contains(&budget) {
            return Err(super::GlobalFrontierError::InvalidBudget);
        }
        let (consumed, written, transitions, complete) = self
            .stream
            .feed_utf8(input, real_end, output, budget - 128)
            .map_err(|_| super::GlobalFrontierError::SourceUtf8)?;
        Ok(InlineTextDecodeStep {
            consumed,
            written,
            transitions: transitions + 128,
            complete,
        })
    }
}
