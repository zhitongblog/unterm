//! Serving the Unzoo One console out of the settings HTTP server.
//!
//! The settings SPA is `include_str!`-ed into the binary because it is a
//! handful of hand-written files. The console is a built artifact: ~1.4 MB
//! across three dozen files whose names carry content hashes that change on
//! every build. Baking that in would grow the binary, and would force a Rust
//! rebuild for a front-end-only change. So it is read from disk instead.
//!
//! Where we look, in order:
//!
//! 1. `UNTERM_CONSOLE_DIR` — point it at `unzoo-one/dist/client` while
//!    developing and the console reloads with a plain browser refresh.
//! 2. `<exe dir>/console` — where the installer puts it.
//!
//! The console reaches the API the same way the settings page does: fetch
//! `/bootstrap.json` once for the auth token, then send it as a bearer on
//! every `/api/*` call. Same origin, so no CORS and nothing to configure.

use std::path::{Component, Path, PathBuf};

/// Where the console's files live, if we can find them.
pub fn console_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("UNTERM_CONSOLE_DIR") {
        let path = PathBuf::from(dir);
        if path.join("index.html").is_file() {
            return Some(path);
        }
    }
    let beside_exe = std::env::current_exe()
        .ok()?
        .parent()?
        .join("console");
    beside_exe.join("index.html").is_file().then_some(beside_exe)
}

/// Content type for a file we are about to serve. Unknown extensions get
/// `application/octet-stream` rather than a guess.
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        Some("map") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Resolve a request path under the console root.
///
/// Rejects anything that is not a plain relative path: absolute paths, `..`,
/// Windows drive prefixes. A request can only ever reach files inside the
/// console directory.
fn resolve(root: &Path, request_path: &str) -> Option<PathBuf> {
    let trimmed = request_path.trim_start_matches('/');
    let relative = if trimmed.is_empty() { "index.html" } else { trimmed };

    let mut resolved = root.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            // Anything else is either a traversal attempt or meaningless here.
            _ => return None,
        }
    }

    // Directory requests get that directory's index.html.
    if resolved.is_dir() {
        resolved.push("index.html");
    }
    resolved.is_file().then_some(resolved)
}

/// Read the file a `/console/...` request maps to.
///
/// `None` means "no such file"; the caller turns that into a 404. An
/// unreadable file that exists is also `None` — we would rather 404 than
/// leak a filesystem error.
pub fn lookup(request_path: &str) -> Option<(&'static str, Vec<u8>)> {
    let root = console_dir()?;
    let file = resolve(&root, request_path)?;
    let body = std::fs::read(&file).ok()?;
    Some((content_type(&file), body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points `UNTERM_CONSOLE_DIR` at a directory for as long as it lives,
    /// then puts the old value back.
    struct ConsoleDirGuard {
        original: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl ConsoleDirGuard {
        fn set(dir: &Path) -> Self {
            let lock = crate::test_env::lock();
            let original = std::env::var_os("UNTERM_CONSOLE_DIR");
            std::env::set_var("UNTERM_CONSOLE_DIR", dir);
            Self {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for ConsoleDirGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var("UNTERM_CONSOLE_DIR", value),
                None => std::env::remove_var("UNTERM_CONSOLE_DIR"),
            }
        }
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("index.html"), b"<!doctype html>console").unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("assets/app-abc123.js"), b"export{}").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"not reachable from outside").unwrap();
        dir
    }

    #[test]
    fn empty_path_serves_index() {
        let dir = fixture();
        let resolved = resolve(dir.path(), "").expect("index");
        assert!(resolved.ends_with("index.html"));
    }

    #[test]
    fn hashed_asset_resolves() {
        let dir = fixture();
        let resolved = resolve(dir.path(), "/assets/app-abc123.js").expect("asset");
        assert!(resolved.ends_with("app-abc123.js"));
    }

    #[test]
    fn traversal_is_refused() {
        let dir = fixture();
        assert!(resolve(dir.path(), "../secret.txt").is_none());
        assert!(resolve(dir.path(), "assets/../../secret.txt").is_none());
        assert!(resolve(dir.path(), "/../../../etc/passwd").is_none());
    }

    #[test]
    fn absolute_paths_are_refused() {
        let dir = fixture();
        assert!(resolve(dir.path(), "//etc/passwd").is_none());
        if cfg!(windows) {
            assert!(resolve(dir.path(), "C:/Windows/System32/config/SAM").is_none());
        }
    }

    #[test]
    fn missing_file_is_none() {
        let dir = fixture();
        assert!(resolve(dir.path(), "nope.js").is_none());
    }

    #[test]
    fn content_types_cover_the_build_output() {
        assert_eq!(content_type(Path::new("index.html")), "text/html; charset=utf-8");
        assert_eq!(content_type(Path::new("a/b-1a2b.js")), "application/javascript; charset=utf-8");
        assert_eq!(content_type(Path::new("a/b-1a2b.css")), "text/css; charset=utf-8");
        assert_eq!(content_type(Path::new("geist-98bbbccb.woff2")), "font/woff2");
        assert_eq!(content_type(Path::new("mark.webp")), "image/webp");
        assert_eq!(content_type(Path::new("weird.bin")), "application/octet-stream");
    }

    #[test]
    fn env_var_points_at_the_console_and_lookup_reads_it() {
        let dir = fixture();
        let _guard = ConsoleDirGuard::set(dir.path());

        assert_eq!(console_dir().as_deref(), Some(dir.path()), "UNTERM_CONSOLE_DIR wins");

        let (ct, body) = lookup("").expect("index.html");
        assert_eq!(ct, "text/html; charset=utf-8");
        assert!(String::from_utf8_lossy(&body).contains("console"));

        let (ct, body) = lookup("/assets/app-abc123.js").expect("hashed asset");
        assert_eq!(ct, "application/javascript; charset=utf-8");
        assert_eq!(body, b"export{}");

        assert!(lookup("/../secret.txt").is_none(), "traversal stays refused through lookup");
        assert!(lookup("/nope.js").is_none());
    }

    #[test]
    fn a_directory_without_index_is_not_a_console() {
        let empty = tempfile::tempdir().expect("tempdir");
        let _guard = ConsoleDirGuard::set(empty.path());
        // No index.html means the env var is ignored, and with no console
        // beside the test binary either, there is nothing to serve.
        assert!(console_dir().is_none() || lookup("").is_none());
    }
}
