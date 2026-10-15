//! Pure foreground close-intent policy shared by the retained-window and weak-observation phases.
//!
//! Native ownership determines when closing completes. Once accepted, the destination intent is
//! immutable: repeated requests coalesce, and an opposing request cannot redirect the host.

use omni_plugin_sdk::PluginCloseIntent;

/// Return whether an existing close absorbs this request, rejecting a different accepted destination.
pub(crate) fn coalesce_accepted_close(
    accepted: Option<PluginCloseIntent>,
    requested: PluginCloseIntent,
) -> Result<bool, &'static str> {
    let Some(accepted) = accepted else {
        // A first request must still pass actual save, responder and host lifecycle preflight.
        return Ok(false);
    };
    if accepted == requested {
        // Both native-close phases share the same original transaction and emit no second callback.
        Ok(true)
    } else {
        // The first accepted destination remains authoritative through weak graph verification.
        Err("An opposing Markdown close is still completing")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Neither close destination can bypass save/responder checks when no close was already accepted.
    fn first_close_always_requires_native_and_host_preflight() {
        for requested in [
            PluginCloseIntent::DismissAndResume,
            PluginCloseIntent::ExitToLauncher,
        ] {
            // Neither destination can bypass preflight merely because the foreground slot is empty.
            assert_eq!(coalesce_accepted_close(None, requested), Ok(false));
        }
    }

    #[test]
    /// Preserve one accepted destination through both duplicate and all opposing request combinations.
    fn accepted_destination_survives_duplicate_and_opposing_requests() {
        let intents = [
            PluginCloseIntent::DismissAndResume,
            PluginCloseIntent::ExitToLauncher,
        ];
        for accepted in intents {
            // Repetition is idempotent in both the native-window and weak-observation close phases.
            assert_eq!(coalesce_accepted_close(Some(accepted), accepted), Ok(true));
            for requested in intents {
                // Every differing pair must reject, leaving the copied original intent unchanged.
                if requested != accepted {
                    // An opposing destination cannot steal the existing native/host completion transaction.
                    assert!(coalesce_accepted_close(Some(accepted), requested).is_err());
                    assert_eq!(coalesce_accepted_close(Some(accepted), accepted), Ok(true));
                }
            }
        }
    }
}
