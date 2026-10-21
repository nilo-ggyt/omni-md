//! Actual paged Find, RootLeaf, and cell composition controls. The original Body
//! in paged_body_source independently verifies the input protocol.

use objc2_foundation::MainThreadMarker;

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("body-find-transactions"))
    {
        assert!(
            std::env::args_os().nth(2).is_none(),
            "事务控制不接受额外参数"
        );
        omni_md_plugin::native_surface::run_native_paged_find_body_transaction_control(mtm)
            .unwrap();
        return; // This explicit diagnostic remains separate from the full remote-target/resource sequence.
    }
    assert!(
        std::env::args_os().nth(1).is_none(),
        "正文输入控制不接受未知参数"
    );
    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    // The full sequence uses the actual Body/Composer, with strict release
    // between synthetic fixtures; no limited substitute Scene control runs first.
    omni_md_plugin::native_surface::run_native_paged_find_body_control(mtm).unwrap();
}
