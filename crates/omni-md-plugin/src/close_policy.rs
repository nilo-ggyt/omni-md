//! Pure close admission keeps destination identity immutable and scope promotion monotonic.
//!
//! Scope is separate from intent: only the Host may authorize terminal preparation, and a plugin
//! must retain its original destination even when application termination consumes its successor.

use omni_plugin_sdk::{PluginCloseRequest, PluginCloseScope};

/// The caller performs fallible effects only for a first request or Host-authorized scope promotion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseAdmission {
    First,
    Coalesced,
    Promote,
}

/// Classify a copied request without mutating ownership or guessing whether termination is authorized.
pub(crate) fn admit_close(
    accepted: Option<PluginCloseRequest>,
    requested: PluginCloseRequest,
) -> Result<CloseAdmission, &'static str> {
    let Some(accepted) = accepted else {
        // Native persistence/resignation and Host begin_close still decide whether this first request is accepted.
        return Ok(CloseAdmission::First);
    };
    if accepted.intent != requested.intent {
        // Promotion cannot redirect the original ordinary close destination, even when its successor is suppressed.
        return Err("An opposing Markdown close is still completing");
    }
    match (accepted.scope, requested.scope) {
        // Identical requests share the accepted native close and its one completion authority.
        (PluginCloseScope::ForegroundTransition, PluginCloseScope::ForegroundTransition)
        | (PluginCloseScope::ApplicationTermination, PluginCloseScope::ApplicationTermination) => {
            Ok(CloseAdmission::Coalesced)
        }
        // Only successful Host authorization may consume this proposal and update the stored request.
        (PluginCloseScope::ForegroundTransition, PluginCloseScope::ApplicationTermination) => {
            Ok(CloseAdmission::Promote)
        }
        // A terminal transaction can never reopen the ordinary successor path.
        (PluginCloseScope::ApplicationTermination, PluginCloseScope::ForegroundTransition) => {
            Err("A terminal Markdown close cannot downgrade to a foreground transition")
        }
    }
}

/// Remembers only an actual payload event and its once-only terminal verification reservation.
#[derive(Default)]
pub(crate) struct TerminalReleaseAuthority {
    resource_event_observed: bool,
    terminal_check_reserved: bool,
}

impl TerminalReleaseAuthority {
    /// Admit either event-before-promotion or promotion-before-event without guessing or retrying release.
    pub(crate) fn reserve(
        &mut self,
        scope: PluginCloseScope,
        actual_resources_released: bool,
    ) -> bool {
        self.resource_event_observed |= actual_resources_released;
        if scope != PluginCloseScope::ApplicationTermination
            || !self.resource_event_observed
            || self.terminal_check_reserved
        {
            // Ordinary scope stores only readiness; an incomplete or already consumed terminal event has no new authority.
            return false;
        }
        self.terminal_check_reserved = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_plugin_sdk::PluginCloseIntent;

    /// The full scope/intent product preserves identity and admits exactly one monotonic promotion.
    #[test]
    fn first_duplicate_promotion_opposition_and_downgrade_are_distinct() {
        let intents = [
            PluginCloseIntent::DismissAndResume,
            PluginCloseIntent::ExitToLauncher,
        ];
        let scopes = [
            PluginCloseScope::ForegroundTransition,
            PluginCloseScope::ApplicationTermination,
        ];
        for intent in intents {
            // Either initial scope must still execute the same native and Host preflight.
            for scope in scopes {
                let accepted = PluginCloseRequest { intent, scope };
                assert_eq!(admit_close(None, accepted), Ok(CloseAdmission::First));
                assert_eq!(
                    admit_close(Some(accepted), accepted),
                    Ok(CloseAdmission::Coalesced)
                );
                for opposing in intents {
                    // Every different destination rejects under both scopes, including apparent terminal promotion.
                    if opposing != intent {
                        for requested_scope in scopes {
                            assert!(
                                admit_close(
                                    Some(accepted),
                                    PluginCloseRequest {
                                        intent: opposing,
                                        scope: requested_scope,
                                    }
                                )
                                .is_err()
                            );
                        }
                    }
                }
            }
            let ordinary = PluginCloseRequest {
                intent,
                scope: PluginCloseScope::ForegroundTransition,
            };
            let terminal = PluginCloseRequest {
                intent,
                scope: PluginCloseScope::ApplicationTermination,
            };
            assert_eq!(
                admit_close(Some(ordinary), terminal),
                Ok(CloseAdmission::Promote)
            );
            assert!(admit_close(Some(terminal), ordinary).is_err());
            // Admission is pure; a rejected Host promotion leaves this exact ordinary request usable.
            assert_eq!(
                admit_close(Some(ordinary), ordinary),
                Ok(CloseAdmission::Coalesced)
            );
        }
    }

    /// Actual event and successful promotion may arrive in either order, but never authorize another check.
    #[test]
    fn terminal_resource_event_is_remembered_and_consumed_once() {
        let mut event_first = TerminalReleaseAuthority::default();
        assert!(!event_first.reserve(PluginCloseScope::ForegroundTransition, true));
        // A rejected Host promotion leaves ordinary scope and cannot consume a terminal reservation.
        assert!(!event_first.reserve(PluginCloseScope::ForegroundTransition, false));
        assert!(event_first.reserve(PluginCloseScope::ApplicationTermination, false));
        assert!(!event_first.reserve(PluginCloseScope::ApplicationTermination, true));
        let mut promotion_first = TerminalReleaseAuthority::default();
        assert!(!promotion_first.reserve(PluginCloseScope::ApplicationTermination, false));
        // Only the real completed payload event makes the first terminal check eligible.
        assert!(promotion_first.reserve(PluginCloseScope::ApplicationTermination, true));
        assert!(!promotion_first.reserve(PluginCloseScope::ApplicationTermination, false));
    }
}
