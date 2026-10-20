//! 已完成run与配对栈分别排序/寻址；各有一真实16KiB写buffer，固定总上限32KiB。
use super::{
    MarkdownScript as Script, MarkdownScriptCancellation as Cancellation,
    MarkdownScriptError as Error, MarkdownScriptRun as Run, Result,
};
use crate::markdown_layout_index::{LayoutIndexError, LayoutIndexObserver, PagedLayoutIndex};

pub(super) const NONE: usize = usize::MAX;
pub(super) struct Frame {
    pub(super) previous: usize,
    pub(super) same_kind: usize,
    pub(super) run: usize,
    pub(super) kind: u8,
}
pub(super) struct Store {
    runs: PagedLayoutIndex<64>,
    frames: PagedLayoutIndex<32>,
    end_byte: usize,
    end_utf16: usize,
    cancellation: Cancellation,
}
impl From<LayoutIndexError> for Error {
    fn from(error: LayoutIndexError) -> Self {
        match error {
            LayoutIndexError::InvalidRecord => Self::CorruptMetadata,
            LayoutIndexError::Overflow => Self::CoordinateOverflow,
            LayoutIndexError::Poisoned => Self::Poisoned,
            LayoutIndexError::Io(error) => Self::Storage(error),
        }
    }
}
fn put(record: &mut [u8], field: usize, value: usize) -> Result<()> {
    record[field..field + 8].copy_from_slice(
        &u64::try_from(value)
            .map_err(|_| Error::CoordinateOverflow)?
            .to_le_bytes(),
    );
    Ok(())
}
fn get(record: &[u8], field: usize) -> Result<usize> {
    let bytes: [u8; 8] = record[field..field + 8].try_into().expect("固定八字节字段");
    usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| Error::CoordinateOverflow)
}
impl Store {
    /// 表格本体没有 SCX run；此覆盖游标仅承认原物理硬边界，不能给被跳正文补 Common。
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn skip_table(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
    ) -> Result<()> {
        self.cancellation.check()?;
        if self.end_byte < span.start.byte || self.end_byte > span.end.byte {
            return Err(Error::CorruptMetadata);
        }
        self.end_byte = span.end.byte;
        self.end_utf16 = span.end.utf16;
        Ok(())
    }
    /// 同一折叠 proof 只转移覆盖游标；隐藏 body 没有 Script 值，也不制造 Unknown/空 run 填洞。
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn skip_fold(
        &mut self,
        span: &crate::markdown_layout_source::folds::FoldSpan,
    ) -> Result<()> {
        self.cancellation.check()?;
        if self.end_byte != span.header_end.byte || self.end_utf16 != span.header_end.utf16 {
            return Err(Error::CorruptMetadata);
        }
        self.end_byte = span.end.byte;
        self.end_utf16 = span.end.utf16;
        Ok(())
    }

    pub(super) fn new(cancellation: &Cancellation) -> Result<Self> {
        cancellation.check()?;
        let runs = PagedLayoutIndex::new()?;
        cancellation.check()?; // 第一份索引装配期间也可撤权，局部owner在返回前真实析构。
        let frames = PagedLayoutIndex::new()?;
        cancellation.check()?;
        Ok(Self {
            runs,
            frames,
            end_byte: 0,
            end_utf16: 0,
            cancellation: cancellation.clone(),
        })
    }
    #[cfg(test)]
    pub(super) fn with_runs_file(file: std::fs::File, cancellation: &Cancellation) -> Result<Self> {
        Ok(Self {
            runs: PagedLayoutIndex::test_with_file(file)?,
            frames: PagedLayoutIndex::new()?,
            end_byte: 0,
            end_utf16: 0,
            cancellation: cancellation.clone(),
        }) // 只替换真实FD权限，后续仍执行原追加buffer/换页实际写入路径，不代算任何脚本结果。
    }
    pub(super) fn run_count(&self) -> usize {
        self.runs.len()
    }
    /// 只借原唯一追加页已初始化的记录范围；不复制页、不读旧文件，也不创建后继缓存。
    /// 调用方仍须用 run 完整解码选中的实际记录，取消及已知索引首错不能被范围查询绕过。
    pub(super) fn run_append_page(&mut self) -> Result<Option<std::ops::Range<usize>>> {
        self.cancellation.check()?;
        self.runs.append_page_records().map_err(Into::into)
    }
    pub(super) fn frame_count(&self) -> usize {
        self.frames.len()
    }
    pub(super) fn append_run(&mut self, run: Run) -> Result<()> {
        self.cancellation.check()?; // 固定每次metadata操作核同handle，深栈循环不等整step完成才取消。
        if run.start_byte != self.end_byte
            || run.start_utf16 != self.end_utf16
            || run.start_byte >= run.end_byte
            || run.start_utf16 >= run.end_utf16
        {
            return Err(Error::CorruptMetadata); // 只发布真实连续非空覆盖，不用空run填补尚未解析的前缀。
        }
        let mut record = [0; 64];
        for (field, value) in [
            (0, run.start_byte),
            (8, run.end_byte),
            (16, run.start_utf16),
            (24, run.end_utf16),
        ] {
            put(&mut record, field, value)?;
        }
        record[32] = run.script.0;
        self.runs.append(record)?;
        self.end_byte = run.end_byte;
        self.end_utf16 = run.end_utf16;
        Ok(())
    }
    pub(super) fn run(&mut self, index: usize) -> Result<Run> {
        self.cancellation.check()?;
        let record = self.runs.read(index)?;
        let run = Run {
            start_byte: get(&record, 0)?,
            end_byte: get(&record, 8)?,
            start_utf16: get(&record, 16)?,
            end_utf16: get(&record, 24)?,
            script: Script(record[32]),
        };
        if run.start_byte >= run.end_byte
            || run.start_utf16 >= run.end_utf16
            || usize::from(run.script.0) >= super::unicode_data::TAGS.len()
        {
            return Err(Error::CorruptMetadata);
        }
        Ok(run)
    }
    pub(super) fn append_frame(&mut self, frame: Frame) -> Result<usize> {
        self.cancellation.check()?;
        let index = self.frames.len();
        if frame.kind >= 65
            || frame.run > self.runs.len()
            || [frame.previous, frame.same_kind]
                .into_iter()
                .any(|value| value != NONE && value >= index)
        {
            return Err(Error::CorruptMetadata);
        }
        let mut record = [0; 32];
        for (field, value) in [(0, frame.previous), (8, frame.same_kind), (16, frame.run)] {
            put(&mut record, field, value)?;
        }
        record[24] = frame.kind;
        self.frames.append(record)?;
        Ok(index)
    }
    pub(super) fn frame(&mut self, index: usize) -> Result<Frame> {
        self.cancellation.check()?;
        let record = self.frames.read(index)?;
        let frame = Frame {
            previous: get(&record, 0)?,
            same_kind: get(&record, 8)?,
            run: get(&record, 16)?,
            kind: record[24],
        };
        if frame.kind >= 65
            || frame.run > self.runs.len()
            || [frame.previous, frame.same_kind]
                .into_iter()
                .any(|value| value != NONE && value >= index)
        {
            return Err(Error::CorruptMetadata);
        }
        Ok(frame)
    }
    pub(super) fn seal_frames(&mut self) -> Result<()> {
        self.cancellation.check()?;
        self.frames.close().map_err(Into::into)
    }
    pub(super) fn seal_runs(&mut self) -> Result<()> {
        self.cancellation.check()?;
        self.runs.seal().map_err(Into::into)
    }
    pub(super) fn close(&mut self) -> Result<()> {
        let frames = self.frames.close();
        let runs = self.runs.close();
        frames?;
        runs?;
        Ok(()) // 即使第一份失败仍尝试归还另一份，不能用失败短路留下额外映射。
    }
    pub(super) fn poison(&mut self) {
        self.frames.poison();
        self.runs.poison();
    }
    pub(super) fn observers(&self) -> (LayoutIndexObserver, LayoutIndexObserver) {
        (
            self.runs.resource_observer(),
            self.frames.resource_observer(),
        )
    }
    pub(super) fn loads(&self) -> (u64, u64) {
        (self.runs.page_loads(), self.frames.page_loads())
    }
}
