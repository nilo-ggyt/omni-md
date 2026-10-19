//! 同一Store的异步预算桥；await只保存Rust局部状态，不改变真实页或record格式。
use super::*;
use cooperate::{Gate, Operation};
use std::rc::Rc;
use store::DiskStack;

pub(in crate::markdown_bidi) struct CooperativeStore {
    pub(in crate::markdown_bidi) raw: Store,
    pub(in crate::markdown_bidi) gate: Rc<Gate>,
}
impl CooperativeStore {
    pub(in crate::markdown_bidi) async fn new(
        capacity: u32,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: &Arc<resource::ResourceState>,
        gate: Rc<Gate>,
    ) -> Result<Self, BidiError> {
        gate.operation(Operation::Resource).await?;
        let mut raw = Store::new_prepared_empty(capacity, budget, cancellation, resources)?;
        for _ in 0..budget.pages {
            // 原固定页预算逐页真实分配；暂停时FD已归原Store所有，不创建第二个IO owner。
            gate.operation(Operation::Resource).await?;
            raw.allocate_prepared_page()?;
        }
        gate.health()?;
        raw.healthy()?;
        Ok(Self { raw, gate })
    }
    pub(in crate::markdown_bidi) fn healthy(&self) -> Result<(), BidiError> {
        self.gate.source_health()?;
        self.raw.healthy()
    }
    pub(in crate::markdown_bidi) async fn flush(&mut self) -> Result<(), BidiError> {
        self.healthy()?;
        for index in 0..self.raw.prepared_page_count() {
            if self.raw.prepared_page_dirty(index) {
                self.gate.operation(Operation::Resource).await?;
                #[cfg(all(test, feature = "paged-layout-breaks"))]
                if self.gate.take_flush_failure() {
                    // 先执行真实前缀pwrite再报错；不以单纯置poison代替实际IO失败路径。
                    self.raw.fail_next_write_after_prefix(7);
                }
                self.raw.flush_prepared_page(index)?;
            }
        }
        self.healthy() // 全部原写操作成功后才允许prepare的最终Ready校验。
    }
    pub(in crate::markdown_bidi) fn into_inner(self) -> Store {
        self.raw
    }
    pub(in crate::markdown_bidi) async fn stack_push(
        &mut self,
        stack: &mut DiskStack,
        first: u32,
        last: u32,
    ) -> Result<(), BidiError> {
        self.gate.operation(Operation::Write).await?;
        stack.push(&mut self.raw, first, last)
    }
    pub(in crate::markdown_bidi) async fn stack_top(
        &mut self,
        stack: &DiskStack,
    ) -> Result<Option<(u32, u32)>, BidiError> {
        self.healthy()?;
        if stack.len() == 0 {
            return Ok(None); // 空栈没有真实record，不能消耗或伪报一次读。
        }
        self.gate.operation(Operation::Read).await?;
        stack.top(&mut self.raw)
    }
    pub(in crate::markdown_bidi) async fn stack_pop(
        &mut self,
        stack: &mut DiskStack,
    ) -> Result<Option<(u32, u32)>, BidiError> {
        self.healthy()?;
        if stack.len() == 0 {
            return Ok(None);
        }
        self.gate.operation(Operation::Read).await?;
        stack.pop(&mut self.raw) // 原pop只读top一次，成功后才更新原depth。
    }
}

// 原编解码器仍是唯一32B格式权威；只是把预算等待放在调用之前。编译器保存暂存record，
// 所有嵌套lookahead/backfill用同一桥，不以错误展开后重跑已改写的processing类别。
macro_rules! prepared_record {
    ($record:ty) => {
        impl $record {
            pub(in crate::markdown_bidi) async fn read_prepared(
                store: &mut CooperativeStore,
                id: u32,
            ) -> Result<Self, BidiError> {
                store.gate.operation(Operation::Read).await?;
                Self::read(&mut store.raw, id)
            }
            pub(in crate::markdown_bidi) async fn write_prepared(
                self,
                store: &mut CooperativeStore,
                id: u32,
            ) -> Result<(), BidiError> {
                store.gate.operation(Operation::Write).await?;
                self.write(&mut store.raw, id)
            }
        }
    };
}
prepared_record!(Text);
prepared_record!(Paragraph);
prepared_record!(Run);
prepared_record!(Sequence);
