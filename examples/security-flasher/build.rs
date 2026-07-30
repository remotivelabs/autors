fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon.ico");
    println!("cargo:rerun-if-changed=assets/app-icon.png");
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target| target == "windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/app-icon.ico");
        resource
            .compile()
            .expect("failed to embed the Windows application icon");
    }
    slint_build::compile("ui/app-window.slint").expect("failed to compile the Slint UI");
}
