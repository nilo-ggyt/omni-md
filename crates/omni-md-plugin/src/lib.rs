//! Statically registered native Markdown plugin for Omni Bar.
//!
//! The SDK is the only host integration boundary. The foreground session owns
//! its confined workspace, mounted native view, and deterministic close contract.

#[cfg(any(target_os = "macos", test))]
mod close_policy;
#[cfg(any(target_os = "macos", test))]
mod foreground_owner;
#[cfg(any(target_os = "macos", test))]
mod terminal_close;

#[cfg(feature = "native-input-trace")]
mod native_input_trace;
#[cfg(feature = "native-input-trace")]
pub use native_input_trace::{
    NATIVE_INPUT_TRACE_CAPACITY, NativeInputTraceAccessError, NativeInputTraceDocument,
    NativeInputTraceKind, NativeInputTraceOutcome, NativeInputTraceRecord,
    NativeInputTraceRejection, NativeInputTraceSelector, NativeInputTraceSnapshot,
};
#[cfg(all(target_os = "macos", feature = "native-input-trace"))]
pub use native_surface::{arm_native_markdown_input_trace, drain_native_markdown_input_trace};

use omni_plugin_sdk::{
    OmniBarHostContext, PLUGIN_PROTOCOL_VERSION, PluginActivation, PluginCloseRequest,
    PluginCommandDescriptor, PluginDescriptor, PluginError, PluginInvocation, PluginNativeView,
    PluginRegistration, PluginSession,
};

#[cfg(target_os = "macos")]
pub mod native_surface;

#[cfg(all(target_os = "macos", feature = "native-markdown-runtime-probe"))]
pub use native_surface::{
    NativeMarkdownRuntimeProbeCompletion, NativeMarkdownRuntimeProbeSession,
    run_native_markdown_runtime_probe,
};

#[cfg(all(target_os = "macos", feature = "native-markdown-runtime-probe"))]
mod runtime_probe_application;
#[cfg(all(target_os = "macos", feature = "native-markdown-runtime-probe"))]
pub use runtime_probe_application::{
    native_markdown_runtime_probe_fixture, run_native_markdown_runtime_probe_application,
};

/// Stable protocol identity; the host treats it as descriptor data.
pub const PLUGIN_ID: &str = "com.omnibar.plugins.omni-md";
/// Query prefix declared to the generic host catalog.
pub const ACTIVATION_CODE: &str = "md";

const COMMANDS: &[PluginCommandDescriptor] = &[PluginCommandDescriptor {
    id: "open",
    title: "Markdown",
    subtitle: "管理和编辑 Markdown 笔记",
    keywords: &["markdown", "md", "note", "笔记"],
}];

/// Immutable idle descriptor; registration allocates no document or native resources.
pub const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    plugin_id: PLUGIN_ID,
    protocol_version: PLUGIN_PROTOCOL_VERSION,
    commands: COMMANDS,
    activation_code: ACTIVATION_CODE,
    symbol: "M",
};

/// Owns foreground workspace access and a borrowed mounted-view descriptor.
/// The native surface releases its view graph before the host drops this session.
pub struct MarkdownSession {
    workspace: omni_markdown_core::MarkdownWorkspace,
    native_view: Option<PluginNativeView>,
}

impl PluginSession for MarkdownSession {
    fn native_view(&self) -> Option<PluginNativeView> {
        // Copying the descriptor does not extend the native view lifetime.
        self.native_view
    }

    fn request_close(&mut self, request: PluginCloseRequest) -> Result<(), PluginError> {
        // Delegate save-before-close and the once-only barrier to the native owner.
        #[cfg(target_os = "macos")]
        {
            return native_surface::request_native_markdown_close(request)
                .map_err(|_| PluginError::new("close_failed", "Markdown 原生 surface 关闭失败"));
        };
        // Unsupported platforms expose an explicit protocol error and allocate no surface.
        #[cfg(not(target_os = "macos"))]
        {
            let _ = request;
            Err(PluginError::new(
                "platform_unavailable",
                "Markdown 原生 surface 只支持 macOS",
            ))
        }
    }
}

impl Drop for MarkdownSession {
    fn drop(&mut self) {
        // Invalidate core search access only after native teardown has completed.
        self.workspace.close_session();
    }
}

#[cfg(target_os = "macos")]
fn activate_markdown(
    host: &mut dyn OmniBarHostContext,
    _invocation: &PluginInvocation<'_>,
) -> Result<PluginActivation, PluginError> {
    // Activation is the first point where workspace/session resources may be created.
    let workspace = omni_markdown_core::MarkdownWorkspace::new(
        host.workspace_root().to_path_buf(),
        host.application_data_root().to_path_buf(),
    );
    // Opening failure propagates without publishing a partially usable plugin session.
    let session = workspace
        .open_session()
        .map_err(|_| PluginError::new("session_failed", "Markdown 工作区无法打开"))?;
    // The host mounts this surface synchronously; failure drops the unopened workspace.
    native_surface::show_native_markdown_window(session.clone(), host.foreground_lifecycle(), host)
        .map_err(|_| PluginError::new("surface_failed", "Markdown 原生 surface 创建失败"))?;
    // The host borrows the mounted view until the lifecycle unmount boundary.
    let native_view = native_surface::active_native_markdown_view();
    Ok(PluginActivation::Session(Box::new(MarkdownSession {
        workspace,
        native_view,
    })))
}

#[cfg(not(target_os = "macos"))]
fn activate_markdown(
    _host: &mut dyn OmniBarHostContext,
    _invocation: &PluginInvocation<'_>,
) -> Result<PluginActivation, PluginError> {
    // Static metadata remains portable, while actual native activation requires macOS.
    Err(PluginError::new(
        "platform_unavailable",
        "Markdown 原生插件只支持 macOS",
    ))
}

/// Compile-time entry consumed by the host composition catalog.
pub const REGISTRATION: PluginRegistration =
    PluginRegistration::new(&DESCRIPTOR, activate_markdown);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Guards the immutable registration shape without constructing a session.
    fn registration_is_static_and_contains_declared_command() {
        // Identity, command inventory and exact protocol version stay aligned.
        assert_eq!(REGISTRATION.descriptor().plugin_id, PLUGIN_ID);
        assert_eq!(REGISTRATION.descriptor().commands.len(), 1);
        assert_eq!(
            REGISTRATION.descriptor().protocol_version,
            PLUGIN_PROTOCOL_VERSION
        );
    }
}
