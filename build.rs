fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/filehelm.ico");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let version = concat!(
            env!("CARGO_PKG_VERSION_MAJOR"),
            ".",
            env!("CARGO_PKG_VERSION_MINOR"),
            ".",
            env!("CARGO_PKG_VERSION_PATCH"),
            ".0"
        );
        let manifest = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
            .join("filehelm.manifest");
        std::fs::write(
            &manifest,
            include_str!("app.manifest").replace("@VERSION@", version),
        )
        .expect("write the Filehelm application manifest");
        println!("cargo:rustc-link-arg-bin=filehelm=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=filehelm=/MANIFESTINPUT:{}",
            manifest.display()
        );
        winresource::WindowsResource::new()
            .set("ProductName", "Filehelm")
            .set("FileDescription", "Filehelm file manager")
            .set("OriginalFilename", "filehelm.exe")
            .set_icon("assets/filehelm.ico")
            .compile_for("filehelm")
            .expect("compile the Filehelm icon and version information");
    }
}
