//! 原分页 Find、RootLeaf 与单格的真实生产组合控制；输入协议由 paged_body_source 的原 Body 验证。

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
        return; // 明确独立诊断；无参完整序列和其原3秒/远端资源门继续保留。
    }
    assert!(
        std::env::args_os().nth(1).is_none(),
        "正文输入控制不接受未知参数"
    );
    #[cfg(all(
        feature = "native-paged-find-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    omni_md_plugin::native_surface::run_native_paged_find_backend_control(mtm).unwrap();
}
