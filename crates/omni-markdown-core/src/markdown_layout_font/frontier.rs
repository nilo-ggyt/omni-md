//! 同一个原字体owner与已验证选项的计划，不用PSName、role或同名字体重建来源。

use harfrust::{
    AsciiFrontierCapability, AsciiFrontierRejection, Direction, Feature, Language, Script,
};
use std::sync::Arc;

use super::{MarkdownLayoutFont, MarkdownLayoutFontAuthority};

/// 计划由唯一frontier owner持有；子任务暂借原计划执行，不能换成同名字体的另一计划。
pub(crate) struct FontFrontierPlan {
    plan: Arc<harfrust::ShapePlan>,
    capability: Option<AsciiFrontierCapability>,
    font: MarkdownLayoutFontAuthority,
    local: bool,
}

impl FontFrontierPlan {
    pub(crate) fn new(
        font: &MarkdownLayoutFont,
        authority: &MarkdownLayoutFontAuthority,
        direction: Direction,
        script: Script,
        language: Option<&Language>,
        features: &[Feature],
    ) -> Result<Self, AsciiFrontierRejection> {
        if !font.permits(authority) {
            return Err(AsciiFrontierRejection::LookupUnavailable); // 原调用方另报精确FontMismatch。
        }
        let plan =
            harfrust::ShapePlan::new(font.font(), direction, Some(script), language, features);
        let (capability, local) = match plan.ascii_frontier_capability(font.font()) {
            Ok(capability) => (Some(capability), false),
            Err(_) => {
                // 这里只取得尚未完成的局部证书游标构造资格；绝不把它伪装成已通过
                // 单输入查表。实际可达闭包和支持半径仍须原frontier逐记录完成。
                plan.ascii_local_frontier_job(font.font())?;
                (None, true)
            }
        };
        Ok(Self {
            plan: Arc::new(plan),
            capability,
            font: authority.clone(),
            local,
        })
    }

    pub(crate) fn capability(&self) -> Option<AsciiFrontierCapability> {
        self.capability // 固定元数据不是另一个可构造源票；实际plan仍只在本owner中。
    }

    pub(crate) fn requires_local_certificate(&self) -> bool {
        self.local
    }

    pub(crate) fn local_job(
        &self,
        font: &MarkdownLayoutFont,
        bounds: (u8, u8),
    ) -> Result<harfrust::AsciiLocalFrontierJob, AsciiFrontierRejection> {
        if !self.local || !self.permits_font(font) {
            return Err(AsciiFrontierRejection::LookupUnavailable);
        }
        self.plan
            .ascii_local_frontier_job_for_range(font.font(), bounds.0..=bounds.1)
    }

    pub(crate) fn plan(&self) -> Arc<harfrust::ShapePlan> {
        Arc::clone(&self.plan)
    }

    pub(crate) fn simple_gcb_job(
        &self,
        font: &MarkdownLayoutFont,
    ) -> Result<harfrust::SimpleGcbFrontierJob, AsciiFrontierRejection> {
        if !self.permits_font(font) {
            return Err(AsciiFrontierRejection::LookupUnavailable);
        }
        self.plan.simple_gcb_frontier_job()
    }

    pub(crate) fn permits_font(&self, font: &MarkdownLayoutFont) -> bool {
        // 同script/features甚至同字节的Font也不能借原计划证明；这里只保弱owner身份。
        font.permits(&self.font)
    }
}
