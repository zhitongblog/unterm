include!("../build_support/winres.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../build_support/winres.rs");

    // The icon the window and the tray draw at run time: a product's own
    // (UNTERM_BRAND_ICON_PNG, 256 px) or Unterm's.
    println!("cargo:rerun-if-env-changed=UNTERM_BRAND_ICON_PNG");
    let png = std::env::var_os("UNTERM_BRAND_ICON_PNG")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from("../assets/icon/unterm-icon-256.png"));
    println!("cargo:rerun-if-changed={}", png.display());
    let out = std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("app-icon-256.png");
    std::fs::copy(&png, &out).expect("copy the app icon");

    // The exe's own icon and version block. Without this resource the
    // taskbar, Alt-Tab and Explorer all show the default program icon —
    // which read as "the logo is missing" next to any released build.
    #[cfg(windows)]
    {
        let repo = std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.parent().map(|parent| parent.to_path_buf()))
            .expect("unterm-app sits inside the repository");
        let ico = repo.join("assets").join("windows").join("terminal.ico");
        emit_version_resource("unterm", "Unterm Terminal", Some(&ico));
    }
}
