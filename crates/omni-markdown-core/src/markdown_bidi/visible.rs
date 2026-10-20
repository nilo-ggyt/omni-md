//! 折叠显示只分析实际需要的完整 B 段；前一段与当前段各保留一次原 UBA 结果。

use super::*;
use crate::{
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    markdown_document_buffer::FrozenBidiParagraphCursor, markdown_layout_source::folds::FoldPlan,
};

pub(super) struct VisibleParagraphs {
    plan: Option<FoldPlan>,
    #[cfg(feature = "s2-table-window-prototype")]
    tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
    base: BidiBaseDirection,
    budget: BidiMemoryBudget,
    cancellation: MarkdownBidiCancellation,
    resources: Arc<resource::ResourceState>,
    previous: Option<Box<MarkdownBidiParagraph>>,
    current: Option<Box<MarkdownBidiParagraph>>,
    failure: Option<prepare::Failure>,
}

/// 仅原协调线程持在途 Future/Gate；已封存的 Paragraph 不持这些 !Send 资源，仍可原样跨线程。
#[derive(Default)]
pub(crate) struct VisiblePreparation {
    cursor: Option<FrozenBidiParagraphCursor>,
    preparation: Option<MarkdownBidiPrepareJob>,
    target: Option<usize>,
}

/// 只缓存这个已封存段的真实 UBA 查询进度；纯数值仍可随 Paragraph 原样跨线程转移。
#[derive(Default)]
pub(super) struct WordOrder {
    next: u32,
    permitted: Option<bool>,
}

/// 词组消费者单独支付原元数据访问；此结果不包含 GCB、Source 端点或 Native 提交权限。
pub(crate) struct WordOrderStep {
    pub(crate) records: usize,
    pub(crate) metadata_units: usize,
    pub(crate) permitted: Option<bool>,
}

impl MarkdownBidiParagraph {
    /// CR/LF/TAB 是合法词组空白，不要求 Markdown 的“无内联控制”证书；真实完整段仍须全部零级。
    pub(crate) fn prepare_word_order(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        range: Range<usize>,
        records: usize,
    ) -> Result<(WordOrderStep, usize), BidiError> {
        self.require_layout_source(authority)?;
        if records == 0 {
            return Err(BidiError::InvalidBudget);
        }
        if range.is_empty() || range.start < self.source.start || range.end > self.source.end {
            return Err(BidiError::InvalidRange);
        }
        let step = match &mut self.resolution {
            Resolution::VisibleParagraphs(visible) => {
                let paragraph = visible.character_mut(range.start)?;
                let through = range.end.min(paragraph.source.end);
                // CRLF 是一个原 GCB，却属于两个原 B 段；这里只证明实际当前段，后继由同一游标再准备。
                paragraph.prepare_word_order(authority, range.start..through, records)?
            }
            Resolution::UniformZero(_) => (
                WordOrderStep {
                    records: 0,
                    metadata_units: 1,
                    permitted: Some(true),
                },
                range.end,
            ), // 原完整证书已经证明零级；换行等 Markdown 控制不影响视觉与逻辑前后的等价。
            Resolution::Paged(store) => (
                self.word_order.advance(store, self.units, records)?,
                range.end,
            ),
        };
        self.require_layout_source(authority)?; // 缓存命中和真实文件读取都不能绕过同源、取消与存储撤权。
        Ok(step)
    }
}

impl WordOrder {
    /// 每条原 Text 记录只消费一次；在整段未完成前不把当前 GCB 的 level0 冒充全段视觉顺序。
    fn advance(
        &mut self,
        store: &mut Store,
        units: u32,
        limit: usize,
    ) -> Result<WordOrderStep, BidiError> {
        let mut records = 0;
        if self.permitted.is_none() {
            while self.next < units && records < limit {
                let text = Text::read(store, self.next)?;
                records += 1;
                self.next += 1;
                // 共用原 UBA 快证书的类别规则，拒绝嵌入/隔离控制；不以最终偶然为零的 level 猜方向。
                if text.level != 0 || !uniform::eligible_class(text.original) {
                    self.permitted = Some(false);
                    break;
                }
            }
            if self.next == units && self.permitted.is_none() {
                self.permitted = Some(true);
            } // 低代理项也是原表的真实记录，同样计入记录量子，不再借正文解码或复制段落。
        }
        Ok(WordOrderStep {
            records,
            metadata_units: records.max(1),
            permitted: self.permitted,
        })
    }
}

impl VisibleParagraphs {
    /// 此值只封闭显示范围，不签任何 level；真正段结果仍必须由原 prepare 完成。
    pub(super) fn new(
        plan: FoldPlan,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: Arc<resource::ResourceState>,
    ) -> Self {
        Self::create(
            Some(plan),
            #[cfg(feature = "s2-table-window-prototype")]
            None,
            base,
            budget,
            cancellation,
            resources,
        )
    }

    /// Table scope 从构造起保持实际段准备；将来的真实行由共享目录追加，不签任何隐藏 level。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(super) fn new_tables(
        tables: crate::markdown_layout_source::table_rows::TablePlan,
        folds: Option<FoldPlan>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: Arc<resource::ResourceState>,
    ) -> Self {
        Self::create(folds, Some(tables), base, budget, cancellation, resources)
    }

    /// 资源和相邻两段仍只有原初始化图；折叠与表格只改变各自真实区间的查询权限。
    fn create(
        plan: Option<FoldPlan>,
        #[cfg(feature = "s2-table-window-prototype")] tables: Option<
            crate::markdown_layout_source::table_rows::TablePlan,
        >,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: Arc<resource::ResourceState>,
    ) -> Self {
        Self {
            plan,
            #[cfg(feature = "s2-table-window-prototype")]
            tables,
            base,
            budget,
            cancellation,
            resources,
            previous: None,
            current: None,
            failure: None,
        }
    }

    /// 原取消、任一真实子 Store 错误及首 close 失败都会撤销整个显示分析范围。
    pub(super) fn healthy(&self) -> Result<(), BidiError> {
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(tables) = &self.tables {
            tables.health()?;
        }
        if let Some(failure) = self.failure {
            return Err(failure.error());
        }
        if let Some(errno) = self.resources.observe().snapshot().close_errno {
            return Err(BidiError::Storage(std::io::Error::from_raw_os_error(errno)));
        }
        for paragraph in [&self.previous, &self.current].into_iter().flatten() {
            match paragraph.resolution.healthy() {
                Ok(()) | Err(BidiError::Cancelled) => {}
                Err(error) => return Err(error),
            }
        } // 两段共享同一取消位；先交付任一实际存储永久失败，再统一处理这个较弱原因。
        self.cancellation.check()
    }

    /// 保留实际在途失败的稳定首因；显示范围错误属于无权限查询，不撤销其它已完成可见段。
    pub(super) fn remember_failure(&mut self, error: &BidiError) {
        if self.failure.is_none()
            && !matches!(
                error,
                BidiError::FoldedRange | BidiError::UnpreparedRange | BidiError::TableRange
            )
        {
            self.failure = Some(prepare::Failure::capture(error));
        }
    }

    /// 隐区仍有原正文，但没有可见 level/run 权限；CR 起点自身保留原段控制职责。
    pub(super) fn require_range(&self, range: &Range<usize>) -> Result<(), BidiError> {
        if range.start > range.end {
            return Err(BidiError::InvalidRange);
        }
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(tables) = &self.tables
            && tables.intersects(range)?
        {
            return Err(BidiError::TableRange);
        }
        let hidden = if range.is_empty() {
            self.plan
                .as_ref()
                .is_some_and(|plan| plan.hides(range.start, true))
        } else {
            self.plan.as_ref().is_some_and(|plan| {
                plan.spans()
                    .iter()
                    .any(|span| range.start < span.end.utf16 && span.start.utf16 + 1 < range.end)
            })
        };
        if hidden {
            return Err(BidiError::FoldedRange);
        }
        Ok(())
    }

    /// 仅委托完整覆盖请求的真实结果；两个有限槽之外的正文不会默认为 level0。
    pub(super) fn range(&self, range: &Range<usize>) -> Result<&MarkdownBidiParagraph, BidiError> {
        self.require_range(range)?;
        [&self.current, &self.previous]
            .into_iter()
            .flatten()
            .find(|paragraph| {
                paragraph.source.start <= range.start && range.end <= paragraph.source.end
            })
            .map(Box::as_ref)
            .ok_or(BidiError::UnpreparedRange)
    }

    /// 查询只借已有两段；不会在同步绘制或 run 遍历中隐式开始另一次全文规则分析。
    pub(super) fn range_mut(
        &mut self,
        range: &Range<usize>,
    ) -> Result<&mut MarkdownBidiParagraph, BidiError> {
        self.require_range(range)?;
        [&mut self.current, &mut self.previous]
            .into_iter()
            .flatten()
            .find(|paragraph| {
                paragraph.source.start <= range.start && range.end <= paragraph.source.end
            })
            .map(Box::as_mut)
            .ok_or(BidiError::UnpreparedRange)
    }

    /// 字符所属段必须半开命中，不能把前一段的末端误签为后继段的 level/base。
    pub(super) fn character_mut(
        &mut self,
        position: usize,
    ) -> Result<&mut MarkdownBidiParagraph, BidiError> {
        self.require_range(&(position..position))?;
        [&mut self.current, &mut self.previous]
            .into_iter()
            .flatten()
            .find(|paragraph| paragraph.source.contains(&position))
            .map(Box::as_mut)
            .ok_or(BidiError::UnpreparedRange)
    }

    /// 原端点游标与原 UBA 子任务共同分摊一个量子；段落完整之前不发布任何局部层级。
    pub(super) fn step(
        &mut self,
        source: &MarkdownLayoutTextSource,
        snapshot: &MarkdownTextSnapshot,
        authority: &MarkdownLayoutTextAuthority,
        position: usize,
        budget: MarkdownBidiPrepareBudget,
        work: &mut VisiblePreparation,
    ) -> Result<uniform::ParagraphPrepareStep, BidiError> {
        self.require_range(&(position..position))?;
        if let Some(job) = &work.preparation {
            job.require_healthy()?;
        } // 在途首错仍属于同一个 Item 任务，不能因缓存中恰有旧段就绕过它。
        if let Some(paragraph) = [&mut self.current, &mut self.previous]
            .into_iter()
            .flatten()
            .find(|paragraph| paragraph.source.contains(&position))
        {
            // 原完整UBA已就绪仍不等于正文控制边界已封闭。子Uniform继续自己的唯一
            // PreparedParagraph游标，真实换行留在段尾；Paged结果则沿原入口零工作返回。
            return paragraph.prepare_uniform_paragraph_range(authority, position, budget);
        }
        if work.target != Some(position) {
            drop(work.preparation.take());
            drop(work.cursor.take());
            self.healthy()?;
            work.target = Some(position);
            work.cursor = Some(snapshot.begin_bidi_paragraph(position)?);
        }
        let mut progress = uniform::ParagraphPrepareStep::ready();
        progress.ready = false;
        if let Some(cursor) = &mut work.cursor {
            let found = snapshot.advance_bidi_paragraph(
                cursor,
                budget.bytes,
                budget.scalars,
                budget.metadata_ops,
            )?;
            progress.bytes = found.bytes;
            progress.scalars = found.scalars;
            progress.metadata = found.metadata;
            progress.maximum_borrow = found.maximum_borrow;
            let Some(range) = found.range else {
                return Ok(progress);
            };
            let bytes = found.byte_range.ok_or(BidiError::CorruptMetadata)?;
            // 第三段启动前退役最旧一段；当前段仍用于原 hard-break 的后继前探和发布。
            if let Some(previous) = self.previous.take() {
                drop(previous);
                self.healthy()?;
            }
            work.preparation = Some(MarkdownBidiPrepareJob::new_layout_paragraph_observed(
                source.clone(),
                authority,
                (range, bytes),
                self.base,
                self.budget,
                &self.cancellation,
                Arc::clone(&self.resources),
            )?);
            drop(work.cursor.take());
        }
        let remaining = MarkdownBidiPrepareBudget {
            bytes: budget.bytes - progress.bytes,
            scalars: budget.scalars - progress.scalars,
            metadata_ops: budget.metadata_ops - progress.metadata,
        };
        if remaining.bytes == 0 || remaining.scalars == 0 || remaining.metadata_ops == 0 {
            return Ok(progress);
        }
        let preparation = work
            .preparation
            .as_mut()
            .ok_or(BidiError::CorruptMetadata)?;
        let actual = preparation.step(authority, remaining)?;
        progress.bytes += actual.bytes;
        progress.scalars += actual.scalars;
        progress.metadata += actual.metadata_ops;
        progress.maximum_borrow = progress.maximum_borrow.max(actual.source_borrow_max_bytes);
        if actual.status == MarkdownBidiPrepareStatus::Ready {
            let paragraph = preparation
                .take_ready(authority)?
                .ok_or(BidiError::CorruptMetadata)?;
            self.previous = self.current.take();
            self.current = Some(Box::new(paragraph));
            drop(work.preparation.take());
            work.target = None;
            let remaining = MarkdownBidiPrepareBudget {
                bytes: remaining.bytes - actual.bytes,
                scalars: remaining.scalars - actual.scalars,
                metadata_ops: remaining.metadata_ops - actual.metadata_ops,
            };
            if remaining.bytes != 0 && remaining.scalars != 0 && remaining.metadata_ops != 0 {
                // 首次Ready也必须经过同一个子段入口，不能让零工作完成的Future绕开
                // 正文端点准备。本次已发生的UBA工作先扣账，端点仅消费剩余原量子。
                let prepared = self
                    .current
                    .as_mut()
                    .expect("原Ready已移交当前完整段")
                    .prepare_uniform_paragraph_range(authority, position, remaining)?;
                progress.bytes += prepared.bytes;
                progress.scalars += prepared.scalars;
                progress.metadata += prepared.metadata;
                progress.maximum_borrow = progress.maximum_borrow.max(prepared.maximum_borrow);
                progress.ready = prepared.ready;
            }
        }
        self.healthy()?;
        Ok(progress)
    }

    /// 一个聚合收据包含两段真实文件和页；观察者本身不保活任何 Source 或 UBA 结果。
    pub(super) fn resource_observer(&self) -> MarkdownBidiResourceObserver {
        self.resources.observe()
    }

    /// 页读数保留已经实际退役的段，当前缓存字节直接取真实共活资源收据。
    pub(super) fn storage(&self) -> (usize, u64) {
        (
            self.resources.observe().snapshot().cache_bytes,
            self.resources.page_loads(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MarkdownDocumentBuffer;

    /// 原量子控制端点与完整规则的总工作，测试只对有限可见段建立独立 UBA oracle。
    fn prepare(
        paragraph: &mut MarkdownBidiParagraph,
        source: &MarkdownLayoutTextSource,
        position: usize,
    ) -> usize {
        let budget = MarkdownBidiPrepareBudget {
            bytes: 7,
            scalars: 3,
            metadata_ops: 5,
        };
        let mut bytes = 0;
        let mut work = VisiblePreparation::default();
        for _ in 0..200_000 {
            let step = paragraph
                .prepare_layout_paragraph_range(&source.authority(), position, budget, &mut work)
                .unwrap();
            assert!(
                step.bytes <= budget.bytes
                    && step.scalars <= budget.scalars
                    && step.metadata <= budget.metadata_ops
            );
            bytes += step.bytes;
            if step.ready {
                return bytes;
            }
            assert!(step.bytes != 0 || step.scalars != 0 || step.metadata != 0);
        }
        panic!("实际可见 B 段没有在原预算内完成");
    }

    /// 只比较真实完整原段，不把被隐藏的 RTL 后缀移出段后再改变 P2/UBA 方向结果。
    fn assert_levels(actual: &mut MarkdownBidiParagraph, text: &str, start: usize) {
        let units: Vec<u16> = text.encode_utf16().collect();
        let oracle =
            unicode_bidi::utf16::BidiInfo::new_with_data_source(&UnicodeBidiData, &units, None);
        for (offset, level) in oracle.levels.iter().enumerate() {
            assert_eq!(
                actual.level_at_utf16(start + offset).unwrap(),
                level.number()
            );
        }
    }

    #[test]
    /// 原 CR/LF 各结束一个真实 B 段；同一 GCB 的权限必须分两次完整证明，不能扩大任一段范围。
    fn word_order_proof_stops_at_each_actual_b_paragraph_inside_crlf() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("crlf-word.md");
        let text = "one two\r\n# H\r\nאבג\r\n# N\r\n";
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let start = "one two\r\n# H".len();
        let end = "one two\r\n# H\r\nאבג\r\n".encode_utf16().count();
        let plan = FoldPlan::new(&source, std::slice::from_ref(&(start..end))).unwrap();
        let cancel = MarkdownBidiCancellation::new();
        let (job, receipt) = MarkdownBidiPrepareJob::new_layout_source_folded_observed(
            source.clone(),
            &authority,
            plan,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &cancel,
        );
        let mut job = job.unwrap();
        assert_eq!(
            job.step(&authority, MarkdownBidiPrepareBudget::default())
                .unwrap()
                .status,
            MarkdownBidiPrepareStatus::Ready
        );
        let mut paragraph = job.take_ready(&authority).unwrap().unwrap();
        drop(job);
        for (position, expected_source, expected_through) in [(7, 0..8, 8), (8, 8..9, 9)] {
            prepare(&mut paragraph, &source, position);
            let Resolution::VisibleParagraphs(visible) = &mut paragraph.resolution else {
                panic!("隐藏 RTL 必须保留真实可见段职责")
            };
            assert_eq!(
                visible.character_mut(position).unwrap().source,
                expected_source
            );
            let mut permitted = None;
            for _ in 0..16 {
                let (step, through) = paragraph
                    .prepare_word_order(&authority, position..9, 1)
                    .unwrap();
                assert_eq!(through, expected_through);
                assert!(step.records <= 1 && step.metadata_units <= 1);
                if step.permitted.is_some() {
                    permitted = step.permitted;
                    break;
                }
            }
            assert_eq!(permitted, Some(true));
        }
        drop(paragraph);
        assert!(receipt.is_released());
    }

    #[test]
    /// 隐区 RTL 不触发全源 UBA；两段发布权限、第三段退役和真实页释放必须同时成立。
    fn hidden_rtl_keeps_two_actual_paragraphs_and_releases_the_oldest_before_third() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("visible-bidi.md");
        let first = "标题 אב";
        let hidden = "אב\u{202b}123\u{202c}😀".repeat(32_768);
        let second = "中 אב 12\n";
        let third = "אב (tail)\n";
        let text = format!("{first}\n{hidden}\n{second}{third}");
        std::fs::write(&path, &text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let start = first.encode_utf16().count();
        let end = start + 1 + hidden.encode_utf16().count() + 1;
        let plan = FoldPlan::new(&source, std::slice::from_ref(&(start..end))).unwrap();
        let cancel = MarkdownBidiCancellation::new();
        let (job, receipt) = MarkdownBidiPrepareJob::new_layout_source_folded_observed(
            source.clone(),
            &authority,
            plan,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &cancel,
        );
        let mut job = job.unwrap();
        assert_eq!(
            job.step(&authority, MarkdownBidiPrepareBudget::default())
                .unwrap()
                .status,
            MarkdownBidiPrepareStatus::Ready
        );
        let mut paragraph = job.take_ready(&authority).unwrap().unwrap();
        drop(job);
        assert!(!paragraph.whole_source_uniform_zero(&authority).unwrap());
        assert!(matches!(
            paragraph.level_at_utf16(0),
            Err(BidiError::UnpreparedRange)
        ));
        assert!(matches!(
            paragraph.level_at_utf16(start + 1),
            Err(BidiError::FoldedRange)
        ));
        let first_bytes = prepare(&mut paragraph, &source, 0);
        assert_levels(&mut paragraph, first, 0);
        let second_bytes = prepare(&mut paragraph, &source, end);
        assert_levels(&mut paragraph, first, 0);
        assert_levels(&mut paragraph, second, end);
        let live = receipt.snapshot();
        assert!(live.file_created && live.file_open && live.cache_bytes == 131_072);
        paragraph = std::thread::spawn(move || {
            // 真正完成的两段可以沿原公共合同跨线程；在途 Gate/Future 没有进入这个输出 owner。
            assert_levels(&mut paragraph, second, end);
            paragraph
        })
        .join()
        .unwrap();
        let third_start = end + second.encode_utf16().count();
        let third_bytes = prepare(&mut paragraph, &source, third_start);
        assert_levels(&mut paragraph, second, end);
        assert_levels(&mut paragraph, third, third_start);
        assert!(matches!(
            paragraph.level_at_utf16(0),
            Err(BidiError::UnpreparedRange)
        ));
        assert!(first_bytes + second_bytes + third_bytes < 32 * 1024);
        assert!(receipt.snapshot().peak_cache_bytes <= 131_072);
        cancel.cancel();
        assert!(matches!(
            paragraph.level_at_utf16(end),
            Err(BidiError::Cancelled)
        ));
        drop(paragraph);
        assert!(receipt.is_released());
        let closed = receipt.snapshot();
        assert!(closed.close_succeeded && !closed.file_open && closed.cache_bytes == 0);
    }
}
