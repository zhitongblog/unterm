// Shared by every shipped Windows executable's build script via `include!`.
//
// A version resource is not cosmetic on Windows: Windows Installer decides
// whether to replace a file by comparing versions, and for a file that
// carries none it falls back to timestamps -- where a file that looks
// modified is treated as user data and left alone. `unterm-core.exe` and
// `unterm-cli.exe` shipped without one for a long time, so an upgrade could
// replace `unterm.exe` and silently skip them, leaving a new front end
// talking to an old Core.
//
// Not a crate: a build script cannot depend on a workspace member without
// making it a real package, and this is fifty lines of string formatting.
#[cfg(windows)]
fn emit_version_resource(binary: &str, description: &str, icon: Option<&std::path::Path>) {
    use std::io::Write;

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    // The product these programs ship in. A standalone build is Unterm; a
    // product that bundles the runtime (Unzoo One) builds it with
    // UNTERM_BRAND_PRODUCT set, so Task Manager, the file's Properties and
    // antivirus reputation see one product from one publisher, not an
    // installer for one name full of programs called another.
    println!("cargo:rerun-if-env-changed=UNTERM_BRAND_PRODUCT");
    let product = std::env::var("UNTERM_BRAND_PRODUCT")
        .ok()
        .map(|name| name.trim().replace(['"', '\\'], ""))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Unterm".to_string());
    let description = match description.strip_prefix("Unterm") {
        Some(rest) => format!("{product}{rest}"),
        None => description.to_string(),
    };
    let mut parts: Vec<&str> = version.split('.').collect();
    while parts.len() < 4 {
        parts.push("0");
    }
    let commas = parts.join(",");

    let icon_line = match icon {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", path.display());
            format!(
                "#define IDI_ICON 0x101\nIDI_ICON ICON \"{}\"\n",
                path.display().to_string().replace('\\', "\\\\")
            )
        }
        None => String::new(),
    };

    let rc_path = std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("resource.rc");
    let mut rc = std::fs::File::create(&rc_path).unwrap();
    write!(
        rc,
        r#"
{icon_line}1 VERSIONINFO
FILEVERSION     {commas}
PRODUCTVERSION  {commas}
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904E4"
        BEGIN
            VALUE "CompanyName",      "Unzoo\0"
            VALUE "FileDescription",  "{description}\0"
            VALUE "FileVersion",      "{version}\0"
            VALUE "LegalCopyright",   "Unzoo, MIT licensed\0"
            VALUE "InternalName",     "{product}\0"
            VALUE "OriginalFilename", "{binary}.exe\0"
            VALUE "ProductName",      "{product}\0"
            VALUE "ProductVersion",   "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1252
    END
END
"#
    )
    .unwrap();
    drop(rc);
    embed_resource::compile_for(&rc_path, [binary]);
}
