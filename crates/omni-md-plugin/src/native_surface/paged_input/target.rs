//! 字符读取与scene身份共享同一Body/Cell目标；cell局部UTF16不能借原Doc弱引用进入正文消费者。

use super::cell::{CellPermit, NativeMarkdownPagedCellSession};
use super::*;
use omni_markdown_core::{MarkdownCellDraftSourceAuthority, MarkdownLayoutTextSource};

pub(super) enum InputTarget {
    Body(Weak<RefCell<MarkdownDocumentBuffer>>),
    Cell {
        session: Weak<NativeMarkdownPagedCellSession>,
        serial: Cell<u64>,
    },
}

#[derive(Clone)]
enum Domain {
    Body(Weak<RefCell<MarkdownDocumentBuffer>>),
    Cell {
        session: Weak<NativeMarkdownPagedCellSession>,
        source: MarkdownCellDraftSourceAuthority,
    },
}

#[derive(Clone)]
/// 只有Input能捕获的不透明目标域；相同Doc内不同cell/root不能通过全局坐标或相同版本互换。
pub struct NativeMarkdownPagedTargetDomain(Domain);

impl NativeMarkdownPagedTargetDomain {
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Domain::Body(a), Domain::Body(b)) => Weak::ptr_eq(a, b),
            (
                Domain::Cell {
                    session: a,
                    source: x,
                },
                Domain::Cell {
                    session: b,
                    source: y,
                },
            ) => Weak::ptr_eq(a, b) && x == y,
            _ => false, // Body与Cell从不共享位置域，不能比较原Doc指针后继续解释局部range。
        }
    }

    #[must_use]
    pub fn is_body(&self, document: &Weak<RefCell<MarkdownDocumentBuffer>>) -> bool {
        matches!(&self.0, Domain::Body(original) if Weak::ptr_eq(original, document))
    }

    /// 此门只核原capture类别与弱内容身份；Body的具体Doc/capture仍由原producer和SceneKey核对。
    ///
    /// # Errors
    /// 源已失败或目标域不符时拒绝，不从公开range/Doc代次给新source补票。
    pub fn require_source(&self, source: &MarkdownLayoutTextSource) -> Result<()> {
        source.byte_offset_for_utf16(0)?; // public有限端点门同样保原Source首错，不另借正文或造Source票。
        let accepted = match &self.0 {
            Domain::Body(_) => source.cell_draft_authority().is_none(),
            Domain::Cell {
                session,
                source: original,
            } => {
                session.strong_count() != 0
                    && source.cell_draft_authority().as_ref() == Some(original)
            }
        };
        if !accepted {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        source.byte_offset_for_utf16(0)?;
        Ok(())
    }
}

impl InputTarget {
    pub(super) fn body_document(&self) -> Weak<RefCell<MarkdownDocumentBuffer>> {
        match self {
            Self::Body(document) => document.clone(),
            Self::Cell { .. } => Weak::new(), // 旧Body专用Find/Source消费者明确失败，绝不拿局部range改canonical。
        }
    }

    pub(super) fn cell(&self) -> Result<Option<Rc<NativeMarkdownPagedCellSession>>> {
        match self {
            Self::Body(_) => Ok(None),
            Self::Cell { session, .. } => session
                .upgrade()
                .map(Some)
                .ok_or(NativeMarkdownPagedInputError::Closed),
        }
    }

    pub(super) fn cell_permit(&self) -> Result<Option<CellPermit>> {
        self.cell()?.map(|session| session.capture()).transpose()
    }

    pub(super) fn domain(&self) -> Result<NativeMarkdownPagedTargetDomain> {
        let domain = match self {
            Self::Body(document) => Domain::Body(document.clone()),
            Self::Cell { session, serial } => {
                let original = session
                    .upgrade()
                    .ok_or(NativeMarkdownPagedInputError::Closed)?;
                let (facts, source) = original.capture()?.facts()?;
                if facts.serial != serial.get() {
                    return Err(NativeMarkdownPagedInputError::StateChanged);
                }
                Domain::Cell {
                    session: session.clone(),
                    source,
                }
            }
        };
        Ok(NativeMarkdownPagedTargetDomain(domain))
    }
}

impl NativeMarkdownPagedInput {
    /// 只捕获原输入票所属的布局源；Cell沿原草稿工厂，正文沿原Doc冻结，不复制文字或另建草稿。
    ///
    /// # Errors
    /// 原票、目标域、内容版本或Source失效时归还本次capture；原映射首错优先于前台撤权。
    pub fn capture_layout_source(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<MarkdownLayoutTextSource> {
        self.check_permit(permit, current)?;
        let domain = self.target.domain()?;
        let source = if let Some(cell) = &permit.cell {
            cell.session()?.capture_source()? // 原token签发局部Source，不能从canonical范围补Cell身份。
        } else {
            let original = self
                .document_identity()
                .upgrade()
                .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            MarkdownLayoutTextSource::capture(&document)
        };
        domain.require_source(&source)?;
        let checked = self.check_permit(permit, current);
        domain.require_source(&source)?; // current可重入修改Cell；旧Source真实健康不得被一般Revoked遮住。
        checked?;
        if !self.target.domain()?.same(&domain) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        Ok(source)
    }

    #[must_use]
    pub fn is_cell(&self) -> bool {
        matches!(self.target, InputTarget::Cell { .. })
    }
    /// 同一许可最多读取4096 UTF16单元，允许半代理项；Native字符串调用者另核标量端点。
    ///
    /// # Errors
    /// Source、原目标或serial失效时丢弃整次候选；Cell不新增冻结root或布局reader。
    pub fn copy_utf16_code_units(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        offset: usize,
        destination: &mut [u16],
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<()> {
        self.check_permit(permit, current)?;
        if destination.len() > 4096 {
            return Err(NativeMarkdownPagedInputError::InvalidRange);
        }
        if let Some(cell) = &permit.cell {
            cell.with_edit(|document, edit| {
                Ok(document.copy_cell_draft_utf16_code_units(edit, offset, destination)?)
            })?;
        } else {
            let original = self
                .document_identity()
                .upgrade()
                .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            document.copy_utf16_code_units(offset, destination)?;
        }
        self.check_permit(permit, current)
    }

    /// 原目标自己的局部/全局范围验证；不把Cell范围套到Doc字节树上。
    ///
    /// # Errors
    /// Source首错、原权限撤销、越界或切入代理项时拒绝，零范围和EOF也真实核健康。
    pub fn validate_utf16_range(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        range: Range<usize>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<()> {
        self.check_permit(permit, current)?;
        if let Some(cell) = &permit.cell {
            cell.with_edit(|document, edit| {
                Ok(document.validate_cell_draft_utf16_range(edit, range)?)
            })?;
        } else {
            let original = self
                .document_identity()
                .upgrade()
                .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            if range.start > range.end {
                document.byte_offset_for_utf16(0)?;
                return Err(NativeMarkdownPagedInputError::InvalidRange);
            }
            document.byte_offset_for_utf16(range.start)?;
            document.byte_offset_for_utf16(range.end)?;
        }
        self.check_permit(permit, current)
    }
}
