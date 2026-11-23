//! Static assets bundled into the unterm binary via `include_str!`.
//!
//! The SPA itself is plain HTML + Tailwind (Play CDN, vendored offline) +
//! Alpine.js (vendored offline). No build step, no JS framework with a
//! compile pass — editing `unterm-settings/assets/settings/*` and rebuilding
//! the binary is the entire workflow.

pub const INDEX_HTML: &str = include_str!("../assets/settings/index.html");
pub const APP_JS: &str = include_str!("../assets/settings/app.js");
pub const STYLE_CSS: &str = include_str!("../assets/settings/style.css");
pub const TAILWIND_JS: &str = include_str!("../assets/settings/tailwind.js");
pub const ALPINE_JS: &str = include_str!("../assets/settings/alpine.js");
pub const LOGO_SVG: &str = include_str!("../assets/settings/logo.svg");

/// The page as this product shows it: a product shipping the runtime names
/// itself in the page's own text. Only the HTML: the script names themes
/// and TERM_PROGRAM values that must stay as they are.
pub fn index_html() -> std::borrow::Cow<'static, str> {
    if unterm_protocol::is_standalone() {
        std::borrow::Cow::Borrowed(INDEX_HTML)
    } else {
        std::borrow::Cow::Owned(
            INDEX_HTML
                .replace(UNTERM_PALETTE, PRODUCT_PALETTE)
                .replace("</head>", PRODUCT_BACKDROP)
                .replace("Unterm", &unterm_protocol::product_name()),
        )
    }
}

/// The page's colours, one block in its Tailwind config.
const UNTERM_PALETTE: &str = r##"bg: "#191919", surface: "#232322", sunken: "#141413", ink: "#f2f1ee",
        muted: "#b8b7b1", faint: "#8a8983", border: "#33322f", borderStrong: "#494841",
        green: "#4dab9a", teal: "#6fccb8", blue: "#7bb4dd", selbg: "#2c2c2c""##;
/// style.css paints the page itself (a near-black with a cyan glow); a
/// product's page sits on its own night instead.
const PRODUCT_BACKDROP: &str = "<style>body{background:radial-gradient(900px 500px at 75% -10%, rgba(255, 211, 107, .06), transparent 65%), radial-gradient(700px 480px at 25% 110%, rgba(122, 162, 247, .08), transparent 70%), #121a2e;}</style>
</head>";
/// A product's night: the deep blue of its windows, cream text.
const PRODUCT_PALETTE: &str = r##"bg: "#121a2e", surface: "#1a2440", sunken: "#0e1526", ink: "#ece7dc",
        muted: "#b9b6ab", faint: "#8b91a6", border: "#2a3654", borderStrong: "#3a4a72",
        green: "#7cc49a", teal: "#9be0b5", blue: "#9dbcff", selbg: "#24304f""##;

/// The product's own logo, installed beside its programs as `brand-logo.svg`.
pub fn product_logo() -> Option<Vec<u8>> {
    if unterm_protocol::is_standalone() {
        return None;
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    std::fs::read(dir.join("brand-logo.svg")).ok()
}

#[cfg(test)]
mod brand_tests {
    use super::*;

    #[test]
    fn the_palette_block_is_found_so_a_product_can_recolour_the_page() {
        assert!(INDEX_HTML.contains(UNTERM_PALETTE), "index.html's Tailwind colours moved; update UNTERM_PALETTE");
        assert_eq!(INDEX_HTML.matches("</head>").count(), 1, "the backdrop goes before the one </head>");
    }
}

/// Match a `/static/<name>` request to a `(content_type, body)` pair.
pub fn lookup_static(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "app.js" => Some(("application/javascript; charset=utf-8", APP_JS)),
        "style.css" => Some(("text/css; charset=utf-8", STYLE_CSS)),
        "tailwind.js" => Some(("application/javascript; charset=utf-8", TAILWIND_JS)),
        "alpine.js" => Some(("application/javascript; charset=utf-8", ALPINE_JS)),
        "logo.svg" => Some(("image/svg+xml; charset=utf-8", LOGO_SVG)),
        _ => None,
    }
}
