//! 同一次同步字形调用的协作撤权；scope不持正文、字体、映射或任何Native对象。

use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crate::{PagingResourceObserver, ShapeError};

/// 只有一个请求位；Clone不保活任务，已请求后没有复位或转移给另一job的接口。
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// 创建新的独立请求位；一个job只使用同一个原token。
    pub fn new() -> Self {
        Self::default()
    }
    /// 其他线程可以请求停止；这不表示worker已退出或资源已经归还。
    pub fn request(&self) {
        self.0.store(true, Ordering::Release);
    }
    /// 只报告原请求事实，不能充当Ready或资源释放证明。
    pub fn requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// 有限阶段标量，仅帮助核实检查实际位于哪个算法循环，不包含字符或路径。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CooperativePhase {
    /// 初始/末端权限门。
    Boundary,
    /// Unicode属性、cluster与方向处理。
    Unicode,
    /// 分解、重排、组合。
    Normalize,
    /// GSUB及其上下文匹配。
    Substitute,
    /// GPOS、advance与attachment处理。
    Position,
    /// 脚本专用重排与syllable机器。
    Script,
    /// AAT状态机。
    Aat,
    /// buffer搬移、排序或反转。
    Buffer,
}

/// 区分真实循环检查与阶段边界；测试不能把调用前的检查冒充深层取消。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CooperativeCheckpointKind {
    /// 当前算法循环真实进入一次迭代。
    Loop,
    /// 阶段进入/返回的固定门。
    Boundary,
}

/// 只传固定标量；生产health回调只读原Source的原子错误，禁止Native或重新捕获正文。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CooperativeCheckpoint {
    /// 本次实际算法阶段。
    pub phase: CooperativePhase,
    /// 是否真实进入循环。
    pub kind: CooperativeCheckpointKind,
}

/// 回调只可读取原Source的固定健康收据；返回永久保存的原errno，不能执行IO/Native。
pub type CooperativeSourceHealth = Arc<dyn Fn(CooperativeCheckpoint) -> Option<i32> + Send + Sync>;

/// 同步调用的固定控制；其中的observer只保计数，不保分页文件或映射。
#[derive(Clone)]
pub struct CooperativeControl {
    token: CancellationToken,
    paging: PagingResourceObserver,
    source: Option<CooperativeSourceHealth>,
}
impl CooperativeControl {
    /// 绑定原token、原分页scope与可选原Source纯原子观察，不替任务制造新身份。
    pub fn new(
        token: CancellationToken,
        paging: PagingResourceObserver,
        source: Option<CooperativeSourceHealth>,
    ) -> Self {
        Self {
            token,
            paging,
            source,
        }
    }
    fn poll(&self, point: CooperativeCheckpoint) -> Result<(), ShapeError> {
        if let Some(errno) = self.source.as_ref().and_then(|health| health(point)) {
            return Err(ShapeError::SourceUnavailable(errno)); // 原Source失败不能被用户取消覆盖。
        }
        if let Some(error) = self.paging.first_error() {
            return Err(ShapeError::Paging {
                operation: error.operation as u32,
                errno: error.errno,
            });
        }
        if self.token.requested() {
            return Err(ShapeError::Cancelled);
        }
        Ok(())
    }
}

struct Active {
    control: CooperativeControl,
    phase: CooperativePhase,
    until_poll: u8,
    terminal: Option<ShapeError>,
}
thread_local! { static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) }; }

struct ScopeRestore(Option<Active>);
impl Drop for ScopeRestore {
    fn drop(&mut self) {
        let previous = self.0.take();
        let removed = ACTIVE.with(|active| active.replace(previous));
        drop(removed); // TLS借用结束才归还纯observer/callback，不在借用内运行任何析构。
    }
}

/// 在已准备好的字体上执行原同步算法；不覆盖冷Font不可失败OnceLock或任意raw callback。
///
/// # Errors
/// 原Source、分页或取消错误原样返回；scope退出不发布部分glyph，也不重置请求位。
pub fn with_cooperative_control<T>(
    control: CooperativeControl,
    operation: impl FnOnce() -> Result<T, ShapeError>,
) -> Result<T, ShapeError> {
    let previous = ACTIVE.with(|active| {
        active.replace(Some(Active {
            control,
            phase: CooperativePhase::Boundary,
            until_poll: 0,
            terminal: None,
        }))
    });
    let _restore = ScopeRestore(previous);
    poll_boundary()?;
    let result = operation();
    poll_boundary()?; // 返回时再次优先核原Source/IO，不能让成功或取消覆盖已观察首错。
    result
}

pub(crate) struct PhaseRestore(CooperativePhase);
impl Drop for PhaseRestore {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            if let Some(active) = active.borrow_mut().as_mut() {
                active.phase = self.0;
            }
        });
    }
}
pub(crate) fn phase(phase: CooperativePhase) -> PhaseRestore {
    let previous = ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        active
            .as_mut()
            .map_or(CooperativePhase::Boundary, |active| {
                std::mem::replace(&mut active.phase, phase)
            })
    });
    PhaseRestore(previous)
}
fn poll(kind: CooperativeCheckpointKind) -> Result<(), ShapeError> {
    let terminal = ACTIVE.with(|active| {
        active
            .borrow()
            .as_ref()
            .and_then(|active| active.terminal.clone())
    });
    if kind == CooperativeCheckpointKind::Loop {
        if let Some(error) = &terminal {
            return Err(error.clone());
        }
    } // 循环立即停止；边界仍核原子Source/IO，保留退出期间出现的真实更高优先错误。
    let original = ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let active = active.as_mut()?;
        if kind == CooperativeCheckpointKind::Loop && active.until_poll != 0 {
            active.until_poll -= 1;
            return None; // 至多127次循环间隔；此倒计时不是身份/epoch，不会用于续权。
        }
        active.until_poll = 127;
        Some((active.control.clone(), active.phase))
    });
    // 原health回调在TLS借用外执行；它不能靠重入留下RefCell借用或清除原请求位。
    match original {
        Some((control, phase)) => {
            let result = control.poll(CooperativeCheckpoint { phase, kind });
            if let Err(error) = &result {
                ACTIVE.with(|active| {
                    if let Some(active) = active.borrow_mut().as_mut() {
                        if active.terminal.is_none()
                            || matches!(active.terminal, Some(ShapeError::Cancelled))
                        {
                            active.terminal = Some(error.clone());
                        } // Cancelled不能遮掉同原scope后续发现的真实Source/分页首因。
                    }
                });
            }
            match (terminal, result) {
                (Some(error), Ok(())) => Err(error),
                (_, result) => result,
            }
        }
        None => terminal.map_or(Ok(()), Err),
    }
}
pub(crate) fn poll_boundary() -> Result<(), ShapeError> {
    poll(CooperativeCheckpointKind::Boundary)
}
pub(crate) fn poll_loop() -> Result<(), ShapeError> {
    poll(CooperativeCheckpointKind::Loop)
}

/// 固定void/bool/Option trait边界的实际退出；调用者必须在解释结果前传播同scope错误。
macro_rules! shape_cooperative_abort {
    () => {
        if crate::cooperative::poll_loop().is_err() {
            return Default::default();
        }
    };
}
macro_rules! shape_cooperative_boundary {
    () => {
        if crate::cooperative::poll_boundary().is_err() {
            return Default::default();
        }
    };
}

#[cfg(test)]
mod tests;
