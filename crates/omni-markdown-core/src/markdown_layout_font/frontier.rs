//! Bind a shaping plan to its original font owner/options without rebuilding source authority from PSName, role, or same-name fonts.

use harfrust::{
    AsciiFrontierCapability, AsciiFrontierRejection, Direction, Feature, Language, Script,
};
use std::sync::Arc;

use super::{MarkdownLayoutFont, MarkdownLayoutFontAuthority};

/// The frontier owns this original plan; child jobs borrow it rather than substituting a plan for another same-name font.
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
            return Err(AsciiFrontierRejection::LookupUnavailable); // The original caller separately reports its precise FontMismatch.
        }
        let plan =
            harfrust::ShapePlan::new(font.font(), direction, Some(script), language, features);
        let (capability, local) = match plan.ascii_frontier_capability(font.font()) {
            Ok(capability) => (Some(capability), false),
            Err(_) => {
                // Establish only eligibility to construct an unfinished local-certificate job, not completed single-input lookup proof.
                // The original frontier still visits actual records to establish reachable closure and supported radius.
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
        self.capability // Fixed metadata cannot manufacture another source ticket; the actual plan remains bound to this owner.
    }

    pub(crate) fn requires_local_certificate(&self) -> bool {
        self.local
    }

    pub(crate) fn local_job(
        &self,
        font: &MarkdownLayoutFont,
        bounds: (u8, u8),
    ) -> Result<harfrust::AsciiLocalFrontierJob, AsciiFrontierRejection> {
        // Only the admitted local path and same original font may start this finite actual input-domain certificate.
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
        // The plan's GCB proof still belongs to its original font allocation, even if another font shares all bytes/options.
        if !self.permits_font(font) {
            return Err(AsciiFrontierRejection::LookupUnavailable);
        }
        self.plan.simple_gcb_frontier_job()
    }

    pub(crate) fn permits_font(&self, font: &MarkdownLayoutFont) -> bool {
        // Equal script/features or even identical font bytes cannot borrow this plan's proof; retain original weak owner identity only.
        font.permits(&self.font)
    }
}
