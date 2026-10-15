//! Finite terminal-window proof distinguishes a detached framework lease from native deallocation.
//!
//! AppKit can retain its closed window in the caller pool that waits for a termination reply. This
//! receipt admits only that empty shell after actual payload release; it never admits a successor.

/// Copied observations leave every native upgrade and project-state borrow behind before receipt use.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TerminalWindowFacts {
    pub close_notified: bool,
    pub project_owner_present: bool,
    pub panel_present: bool,
    pub visible: bool,
    pub key: bool,
    pub content_attached: bool,
    pub delegate_attached: bool,
    pub initial_responder_attached: bool,
    pub payload_responder_attached: bool,
    pub sheet_attached: bool,
}

/// A terminal-only receipt states whether AppKit still leases an empty panel, without owning it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameworkWindowLeaseReceipt {
    pub framework_panel_pending: bool,
}

impl TerminalWindowFacts {
    /// Reject every project/resource edge before acknowledging a possible framework-only shell.
    pub(crate) fn verify(self) -> Result<FrameworkWindowLeaseReceipt, &'static str> {
        if !self.close_notified || self.project_owner_present {
            // Visibility alone cannot prove the real close callback occurred or the unique owner ended.
            return Err(
                "The Markdown terminal close callback or project-owner release is incomplete",
            );
        }
        if self.visible
            || self.key
            || self.content_attached
            || self.delegate_attached
            || self.initial_responder_attached
            || self.payload_responder_attached
            || self.sheet_attached
        {
            // Any native payload, routing edge or visible/key state exceeds the sole permitted empty-shell lease.
            return Err("The Markdown terminal panel is not closed and detached");
        }
        Ok(FrameworkWindowLeaseReceipt {
            framework_panel_pending: self.panel_present,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal receipt permits only a completely detached shell or its already deallocated identity.
    #[test]
    fn every_live_edge_and_missing_close_or_owner_proof_rejects() {
        let empty = TerminalWindowFacts {
            close_notified: true,
            project_owner_present: false,
            panel_present: true,
            visible: false,
            key: false,
            content_attached: false,
            delegate_attached: false,
            initial_responder_attached: false,
            payload_responder_attached: false,
            sheet_attached: false,
        };
        assert_eq!(
            empty.verify(),
            Ok(FrameworkWindowLeaseReceipt {
                framework_panel_pending: true
            })
        );
        assert_eq!(
            TerminalWindowFacts {
                panel_present: false,
                ..empty
            }
            .verify(),
            Ok(FrameworkWindowLeaseReceipt {
                framework_panel_pending: false
            })
        );
        let failures = [
            TerminalWindowFacts {
                close_notified: false,
                ..empty
            },
            TerminalWindowFacts {
                project_owner_present: true,
                ..empty
            },
            TerminalWindowFacts {
                visible: true,
                ..empty
            },
            TerminalWindowFacts { key: true, ..empty },
            TerminalWindowFacts {
                content_attached: true,
                ..empty
            },
            TerminalWindowFacts {
                delegate_attached: true,
                ..empty
            },
            TerminalWindowFacts {
                initial_responder_attached: true,
                ..empty
            },
            TerminalWindowFacts {
                payload_responder_attached: true,
                ..empty
            },
            TerminalWindowFacts {
                sheet_attached: true,
                ..empty
            },
        ];
        for facts in failures {
            // Each independent edge vetoes the receipt; no aggregate visibility shortcut hides another owner.
            assert!(facts.verify().is_err());
        }
    }
}
