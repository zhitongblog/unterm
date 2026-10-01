//! Installing a newer Unterm from inside Unterm.
//!
//! The update check has long said "v0.71.x is available" and left the rest to
//! the user: find the release, download the right one of nine files, quit,
//! replace, start again. With a release most days, most people were running
//! something weeks old. This does the rest, the same way from every surface
//! -- `unterm-cli update`, the command palette, Web Settings:
//!
//! 1. Ask GitHub for the latest release and pick this platform's package.
//! 2. Download it and check its SHA-256 against the digest GitHub recorded
//!    when the file was uploaded. A file that does not match is deleted.
//! 3. On macOS, check the new app's code signature, and that it is signed by
//!    the same team as the app it replaces.
//! 4. Hand the swap to a small helper that outlives us: it waits for Unterm's
//!    processes to exit, puts the new version in place (or runs the MSI), and
//!    starts it again.
//!
//! What it does not do: replace a `.deb` install (that is the package
//! manager's job, and needs root) or a portable zip (there is no telling
//! what else lives in that folder). Those get the release page.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

const REPO: &str = "zhitongblog/unterm";

/// A published release, as far as updating needs to know it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Release {
    pub tag: String,
    pub url: String,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Hex SHA-256 from GitHub's `digest` field, when the release has one.
    pub sha256: Option<String>,
}

/// How this copy of Unterm was installed, which decides how it is replaced.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Install {
    /// `…/Unterm.app` on macOS.
    MacApp { bundle: PathBuf },
    /// The MSI's per-machine install on Windows.
    WindowsMsi { dir: PathBuf },
    /// A Linux AppImage, replaced in place.
    AppImage { path: PathBuf },
    /// Anything else, and why it is left to the user.
    Manual { reason: String },
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("unterm-updater/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600))
        .build()?)
}

/// The latest release.
///
/// The GitHub API first -- with `GITHUB_TOKEN`/`GH_TOKEN` when set, since the
/// anonymous allowance is 60 requests an hour per address and an office or a
/// VPN shares one address between everybody. When the API will not answer,
/// the release page's redirect still names the newest tag, and the release's
/// `SHA256SUMS` file supplies what the API's digests would have.
///
/// `UNTERM_UPDATE_API` replaces the API URL (a `file://` path is read), for
/// tests.
pub fn latest() -> Result<Release> {
    let api = std::env::var("UNTERM_UPDATE_API")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    if let Some(path) = api.strip_prefix("file://") {
        let body: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        return parse_release(&body);
    }
    match from_api(&api) {
        Ok(release) => Ok(release),
        Err(api_error) => from_release_page().with_context(|| {
            format!("the GitHub API did not answer ({api_error:#}), and neither did the release page")
        }),
    }
}

fn from_api(url: &str) -> Result<Release> {
    let mut request = client()?
        .get(url)
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = ["GITHUB_TOKEN", "GH_TOKEN"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
    {
        request = request.bearer_auth(token);
    }
    let body: serde_json::Value = request
        .send()
        .with_context(|| format!("ask {url} for the latest release"))?
        .error_for_status()?
        .json()?;
    parse_release(&body)
}

/// The release the page `releases/latest` redirects to, with its files named
/// the way every release names them and checksums from `SHA256SUMS`.
fn from_release_page() -> Result<Release> {
    let no_redirects = reqwest::blocking::Client::builder()
        .user_agent(concat!("unterm-updater/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let page = format!("https://github.com/{REPO}/releases/latest");
    let response = no_redirects.get(&page).send()?;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| anyhow!("{page} did not redirect to a release"))?;
    let tag = location
        .rsplit("/tag/")
        .next()
        .filter(|tag| tag.starts_with('v') && !tag.contains('/'))
        .ok_or_else(|| anyhow!("could not read a tag from {location}"))?
        .to_string();
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let sums = client()?
        .get(format!("{base}/SHA256SUMS"))
        .send()
        .ok()
        .filter(|response| response.status().is_success())
        .and_then(|response| response.text().ok())
        .unwrap_or_default();
    let digests = parse_sums(&sums);
    let version = tag.trim_start_matches('v');
    let names = [
        format!("Unterm-macos-{tag}.dmg"),
        format!("Unterm-{version}-x64.msi"),
        format!("Unterm-{version}-arm64.msi"),
        format!("Unterm-{tag}-x86_64.AppImage"),
        format!("Unterm-{tag}-aarch64.AppImage"),
    ];
    Ok(Release {
        url: location.to_string(),
        assets: names
            .into_iter()
            .map(|name| Asset {
                url: format!("{base}/{name}"),
                size: 0,
                sha256: digests.get(&name).cloned(),
                name,
            })
            .collect(),
        tag,
    })
}

/// `sha256sum` output: `<hex>  <name>` per line.
fn parse_sums(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hex = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| (name.to_string(), hex.to_ascii_lowercase()))
        })
        .collect()
}

fn parse_release(body: &serde_json::Value) -> Result<Release> {
    let tag = body
        .get("tag_name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("the release has no tag"))?
        .to_string();
    let assets = body
        .get("assets")
        .and_then(|value| value.as_array())
        .map(|assets| {
            assets
                .iter()
                .filter_map(|asset| {
                    Some(Asset {
                        name: asset.get("name")?.as_str()?.to_string(),
                        url: asset.get("browser_download_url")?.as_str()?.to_string(),
                        size: asset.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                        sha256: asset
                            .get("digest")
                            .and_then(|v| v.as_str())
                            .and_then(|digest| digest.strip_prefix("sha256:"))
                            .map(|hex| hex.to_ascii_lowercase()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        url: body
            .get("html_url")
            .and_then(|value| value.as_str())
            .unwrap_or("https://github.com/zhitongblog/unterm/releases/latest")
            .to_string(),
        tag,
        assets,
    })
}

/// Is `tag` (`v0.71.17`) newer than `current` (`0.71.16`)?
pub fn is_newer(tag: &str, current: &str) -> bool {
    fn parts(text: &str) -> Option<(u64, u64, u64)> {
        let text = text.trim().trim_start_matches('v');
        let mut it = text.split('.');
        Some((
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next().unwrap_or("0").parse().ok()?,
        ))
    }
    matches!((parts(tag), parts(current)), (Some(new), Some(old)) if new > old)
}

/// Work out how the copy that `exe` belongs to was installed.
pub fn install_of(exe: &Path) -> Install {
    if cfg!(target_os = "macos") {
        // …/Unterm.app/Contents/MacOS/<exe>
        let bundle = exe
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .filter(|bundle| bundle.extension().is_some_and(|ext| ext == "app"));
        return match bundle {
            Some(bundle) => Install::MacApp {
                bundle: bundle.to_path_buf(),
            },
            None => Install::Manual {
                reason: "this Unterm is not running from an app bundle".into(),
            },
        };
    }
    if cfg!(windows) {
        let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        let program_files = ["ProgramFiles", "ProgramW6432"]
            .iter()
            .filter_map(|key| std::env::var_os(key).map(PathBuf::from))
            .any(|root| dir.starts_with(&root));
        return if program_files {
            Install::WindowsMsi { dir }
        } else {
            Install::Manual {
                reason: "this is a portable copy; replace its folder with the new zip".into(),
            }
        };
    }
    match std::env::var_os("APPIMAGE").map(PathBuf::from) {
        Some(path) if path.is_file() => Install::AppImage { path },
        _ => Install::Manual {
            reason: "installed from a package; update it with your package manager".into(),
        },
    }
}

fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

/// The release file that replaces `install`.
pub fn asset_for<'a>(release: &'a Release, install: &Install) -> Option<&'a Asset> {
    let tag = &release.tag;
    let version = tag.trim_start_matches('v');
    let wanted = match install {
        Install::MacApp { .. } => format!("Unterm-macos-{tag}.dmg"),
        Install::WindowsMsi { .. } => format!("Unterm-{version}-{}.msi", arch()),
        Install::AppImage { .. } => format!(
            "Unterm-{tag}-{}.AppImage",
            if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" }
        ),
        Install::Manual { .. } => return None,
    };
    release.assets.iter().find(|asset| asset.name == wanted)
}

/// Download `asset` into `dir` and check it. Returns the file.
pub fn download(asset: &Asset, dir: &Path, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};

    let Some(expected) = asset.sha256.clone() else {
        bail!(
            "GitHub recorded no checksum for {}; refusing to install a file nothing can vouch for",
            asset.name
        );
    };
    std::fs::create_dir_all(dir)?;
    let path = dir.join(&asset.name);
    let partial = dir.join(format!("{}.part", asset.name));
    let mut response = client()?
        .get(&asset.url)
        .send()
        .with_context(|| format!("download {}", asset.name))?
        .error_for_status()?;
    let total = response.content_length().unwrap_or(asset.size);
    let mut file = std::fs::File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        done += read as u64;
        progress(done, total);
    }
    file.flush()?;
    drop(file);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(&partial);
        bail!("{} does not match its published checksum (got {actual}, expected {expected})", asset.name);
    }
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

/// What `schedule` set up, in words a person can read.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Scheduled {
    pub summary: String,
    /// The helper's own log, for when something goes wrong after we exit.
    pub log: PathBuf,
}

/// Put the downloaded `file` in place once every process in `wait_for` has
/// exited, then start Unterm again if `relaunch`.
///
/// The caller is responsible for making those processes exit -- the helper
/// gives them two minutes and then proceeds anyway, so a hung process
/// cannot leave an update half done forever.
pub fn schedule(install: &Install, file: &Path, wait_for: &[u32], relaunch: bool) -> Result<Scheduled> {
    let work = file
        .parent()
        .ok_or_else(|| anyhow!("download has no folder"))?
        .to_path_buf();
    let log = work.join("update.log");
    match install {
        Install::MacApp { bundle } => schedule_mac(bundle, file, wait_for, relaunch, &work, &log),
        Install::WindowsMsi { dir } => schedule_msi(dir, file, wait_for, relaunch, &work, &log),
        Install::AppImage { path } => schedule_appimage(path, file, wait_for, relaunch, &work, &log),
        Install::Manual { reason } => bail!("{reason}"),
    }
}

fn run(program: &str, args: &[&str]) -> Result<std::process::Output> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("run {program}"))?;
    Ok(output)
}

/// `TeamIdentifier=…` from `codesign -dv`, if the code is signed by a team.
#[cfg(target_os = "macos")]
fn team_of(app: &Path) -> Option<String> {
    let output = run("codesign", &["-dv", &app.to_string_lossy()]).ok()?;
    let text = String::from_utf8_lossy(&output.stderr);
    text.lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_string)
}

#[allow(unused_variables)]
fn schedule_mac(
    bundle: &Path,
    dmg: &Path,
    wait_for: &[u32],
    relaunch: bool,
    work: &Path,
    log: &Path,
) -> Result<Scheduled> {
    #[cfg(not(target_os = "macos"))]
    bail!("an app bundle is only replaced on macOS");
    #[cfg(target_os = "macos")]
    {
        let mount = work.join("mount");
        let _ = run("hdiutil", &["detach", "-quiet", &mount.to_string_lossy()]);
        std::fs::create_dir_all(&mount)?;
        let attached = run(
            "hdiutil",
            &[
                "attach",
                "-nobrowse",
                "-readonly",
                "-noautoopen",
                "-mountpoint",
                &mount.to_string_lossy(),
                &dmg.to_string_lossy(),
            ],
        )?;
        if !attached.status.success() {
            bail!("could not open {}: {}", dmg.display(), String::from_utf8_lossy(&attached.stderr));
        }
        let result = (|| -> Result<Scheduled> {
            // The image carries a helper app beside Unterm's own (the Finder
            // integration repair tool): the first `.app` found is not
            // necessarily the one to install. The same name as the bundle
            // being replaced, or failing that the one that holds `unterm`.
            let apps: Vec<PathBuf> = std::fs::read_dir(&mount)?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "app"))
                .collect();
            let holds_unterm = |app: &Path| app.join("Contents").join("MacOS").join("unterm").is_file();
            let app = apps
                .iter()
                .find(|app| app.file_name() == bundle.file_name() && holds_unterm(app))
                .or_else(|| apps.iter().find(|app| holds_unterm(app)))
                .cloned()
                .ok_or_else(|| anyhow!("the disk image holds no Unterm app"))?;
            let verified = run("codesign", &["--verify", "--deep", "--strict", &app.to_string_lossy()])?;
            if !verified.status.success() {
                bail!(
                    "the new app's signature does not verify: {}",
                    String::from_utf8_lossy(&verified.stderr).trim()
                );
            }
            // The same publisher, or nothing: a valid signature by somebody
            // else is still somebody else.
            if let Some(current) = team_of(bundle) {
                match team_of(&app) {
                    Some(new) if new == current => {}
                    other => bail!(
                        "the new app is signed by {} rather than {current}",
                        other.unwrap_or_else(|| "nobody".into())
                    ),
                }
            }
            let staged = bundle.with_extension("app.update-new");
            let _ = std::fs::remove_dir_all(&staged);
            let copied = run("ditto", &[&app.to_string_lossy(), &staged.to_string_lossy()])?;
            if !copied.status.success() {
                bail!(
                    "could not copy the new app next to {} (is that folder writable?): {}",
                    bundle.display(),
                    String::from_utf8_lossy(&copied.stderr).trim()
                );
            }
            let old = bundle.with_extension("app.update-old");
            let script = format!(
                "#!/bin/sh\n\
                 exec >>{log} 2>&1\n\
                 echo \"$(date) waiting for {pids}\"\n\
                 {wait}\n\
                 rm -rf {old}\n\
                 if [ ! -x {staged}/Contents/MacOS/unterm ]; then\n\
                 \x20 echo \"$(date) the staged app holds no unterm; leaving the installed one alone\"\n\
                 \x20 exit 1\n\
                 fi\n\
                 if mv {bundle} {old} && mv {staged} {bundle}; then\n\
                 \x20 rm -rf {old}\n\
                 \x20 echo \"$(date) installed\"\n\
                 else\n\
                 \x20 echo \"$(date) swap failed; putting the old app back\"\n\
                 \x20 [ -d {old} ] && [ ! -d {bundle} ] && mv {old} {bundle}\n\
                 fi\n\
                 {relaunch}\n",
                log = shell_quote(log),
                pids = wait_for.iter().map(u32::to_string).collect::<Vec<_>>().join(" "),
                wait = unix_wait(wait_for),
                old = shell_quote(&old),
                bundle = shell_quote(bundle),
                staged = shell_quote(&staged),
                relaunch = if relaunch {
                    format!("open {}", shell_quote(bundle))
                } else {
                    String::new()
                },
            );
            spawn_unix_helper(work, &script)?;
            Ok(Scheduled {
                summary: format!("{} will be replaced once Unterm has quit", bundle.display()),
                log: log.to_path_buf(),
            })
        })();
        let _ = run("hdiutil", &["detach", "-quiet", &mount.to_string_lossy()]);
        result
    }
}

#[allow(unused_variables)]
fn schedule_appimage(
    path: &Path,
    file: &Path,
    wait_for: &[u32],
    relaunch: bool,
    work: &Path,
    log: &Path,
) -> Result<Scheduled> {
    #[cfg(not(unix))]
    bail!("an AppImage is only replaced on Linux");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let staged = path.with_extension("AppImage.update-new");
        std::fs::copy(file, &staged)
            .with_context(|| format!("copy the new AppImage next to {}", path.display()))?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        let script = format!(
            "#!/bin/sh\nexec >>{log} 2>&1\n{wait}\nmv -f {staged} {path} && echo \"$(date) installed\"\n{relaunch}\n",
            log = shell_quote(log),
            wait = unix_wait(wait_for),
            staged = shell_quote(&staged),
            path = shell_quote(path),
            relaunch = if relaunch {
                format!("nohup {} >/dev/null 2>&1 &", shell_quote(path))
            } else {
                String::new()
            },
        );
        spawn_unix_helper(work, &script)?;
        Ok(Scheduled {
            summary: format!("{} will be replaced once Unterm has quit", path.display()),
            log: log.to_path_buf(),
        })
    }
}

#[allow(unused_variables)]
fn schedule_msi(
    dir: &Path,
    msi: &Path,
    wait_for: &[u32],
    relaunch: bool,
    work: &Path,
    log: &Path,
) -> Result<Scheduled> {
    #[cfg(not(windows))]
    bail!("an MSI is only installed on Windows");
    #[cfg(windows)]
    {
        let quote = |path: &Path| path.display().to_string().replace('\'', "''");
        let pids = wait_for
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        // Waits, installs with the progress bar only (no questions), and
        // starts the new version. msiexec's own log goes beside ours.
        let script = format!(
            "$ErrorActionPreference = 'Continue'\r\n\
             Start-Transcript -Append -Path '{log}' | Out-Null\r\n\
             foreach ($id in @({pids})) {{ Wait-Process -Id $id -Timeout 120 -ErrorAction SilentlyContinue }}\r\n\
             $p = Start-Process msiexec.exe -ArgumentList @('/i', '\"{msi}\"', '/passive', '/norestart', '/l*v', '\"{msilog}\"') -Wait -PassThru\r\n\
             \"msiexec exited $($p.ExitCode)\"\r\n\
             {relaunch}\r\n\
             Stop-Transcript | Out-Null\r\n",
            log = quote(log),
            pids = if pids.is_empty() { "0".to_string() } else { pids },
            msi = quote(msi),
            msilog = quote(&work.join("msiexec.log")),
            relaunch = if relaunch {
                format!(
                    "if ($p.ExitCode -eq 0 -or $p.ExitCode -eq 3010) {{ Start-Process '{}' }}",
                    quote(&dir.join("unterm.exe"))
                )
            } else {
                String::new()
            },
        );
        let helper = work.join("update.ps1");
        std::fs::write(&helper, script)?;
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
            .arg(&helper)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if command.spawn().is_err() {
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            command.spawn().context("start the update helper")?;
        }
        Ok(Scheduled {
            summary: format!("the installer will run once Unterm has quit ({})", dir.display()),
            log: log.to_path_buf(),
        })
    }
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy())
}

/// A loop that returns once none of `pids` is alive, or after two minutes.
#[allow(dead_code)]
fn unix_wait(pids: &[u32]) -> String {
    if pids.is_empty() {
        return String::new();
    }
    let checks = pids
        .iter()
        .map(|pid| format!("kill -0 {pid} 2>/dev/null"))
        .collect::<Vec<_>>()
        .join(" || ");
    format!("n=0; while {{ {checks}; }} && [ $n -lt 600 ]; do sleep 0.2; n=$((n+1)); done")
}

#[cfg(unix)]
fn spawn_unix_helper(work: &Path, script: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let helper = work.join("update.sh");
    std::fs::write(&helper, script)?;
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg(&helper)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own session: it must outlive the terminal it may have been started
    // from, which is about to quit.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().context("start the update helper")?;
    Ok(())
}

/// Where downloads and the helper live for one update.
pub fn work_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("unterm-update-{tag}"))
}

/// What `update_this` found or did.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    UpToDate { current: String, latest: String },
    /// Downloaded, checked and handed to the helper. The caller now makes
    /// every process in `waiting_for` quit.
    Scheduled {
        current: String,
        latest: String,
        waiting_for: Vec<u32>,
        scheduled: Scheduled,
    },
    /// This install is not one Unterm replaces itself; the release page is.
    Manual { current: String, latest: String, reason: String, url: String },
}

/// The processes that belong to `install`: windows and Cores running from
/// it, found by where their executable lives. These are what the helper
/// waits for before it touches the files.
pub fn processes_of(install: &Install) -> Vec<u32> {
    let root = match install {
        Install::MacApp { bundle } => bundle.clone(),
        Install::WindowsMsi { dir } => dir.clone(),
        Install::AppImage { .. } | Install::Manual { .. } => return vec![std::process::id()],
    };
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = canonical(&root);
    let inside = |pid: u32| {
        crate::process_lifetime::executable_path(pid)
            .map(|path| canonical(&path).starts_with(&root))
            .unwrap_or(false)
    };
    let mut pids: Vec<u32> = crate::process_lifetime::running_unterm_processes()
        .into_iter()
        .map(|process| process.pid)
        .filter(|pid| inside(*pid))
        .collect();
    // The Core is found through its discovery record off Windows.
    if let Some(core) = unterm_protocol::core_discovery_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|record| record.get("pid")?.as_u64())
    {
        let core = core as u32;
        if inside(core) && !pids.contains(&core) {
            pids.push(core);
        }
    }
    let own = std::process::id();
    if inside(own) && !pids.contains(&own) {
        pids.push(own);
    }
    pids.retain(|pid| *pid != 0);
    pids
}

/// Update the Unterm that `exe` belongs to, up to the point of quitting it.
///
/// `current` is that install's version -- the caller's own when it is part
/// of the install, read from the install otherwise.
pub fn update(
    exe: &Path,
    current: &str,
    force: bool,
    relaunch: bool,
    progress: impl FnMut(u64, u64),
) -> Result<Outcome> {
    let release = latest()?;
    let latest_version = release.tag.trim_start_matches('v').to_string();
    if !force && !is_newer(&release.tag, current) {
        return Ok(Outcome::UpToDate {
            current: current.to_string(),
            latest: latest_version,
        });
    }
    let install = install_of(exe);
    let Some(asset) = asset_for(&release, &install) else {
        let reason = match &install {
            Install::Manual { reason } => reason.clone(),
            _ => format!("release {} has no package for this platform", release.tag),
        };
        return Ok(Outcome::Manual {
            current: current.to_string(),
            latest: latest_version,
            reason,
            url: release.url.clone(),
        });
    };
    let file = download(asset, &work_dir(&release.tag), progress)?;
    let waiting_for = processes_of(&install);
    let scheduled = schedule(&install, &file, &waiting_for, relaunch)?;
    Ok(Outcome::Scheduled {
        current: current.to_string(),
        latest: latest_version,
        waiting_for,
        scheduled,
    })
}

/// `update`, for the process asking: the install it is part of, at its own
/// version.
pub fn update_this(relaunch: bool, progress: impl FnMut(u64, u64)) -> Result<Outcome> {
    let exe = std::env::current_exe().context("find this executable")?;
    update(&exe, unterm_protocol::PRODUCT_VERSION, false, relaunch, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_files_are_read_the_way_sha256sum_writes_them() {
        let hex = "ab".repeat(32);
        let sums = parse_sums(&format!("{hex}  Unterm-macos-v1.dmg\n{hex} *Unterm-1-x64.msi\njunk line\n"));
        assert_eq!(sums.get("Unterm-macos-v1.dmg"), Some(&hex));
        assert_eq!(sums.get("Unterm-1-x64.msi"), Some(&hex));
        assert_eq!(sums.len(), 2);
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("v0.71.17", "0.71.16"));
        assert!(is_newer("v0.72.0", "0.71.99"));
        assert!(is_newer("v0.71.10", "0.71.9"));
        assert!(!is_newer("v0.71.16", "0.71.16"));
        assert!(!is_newer("v0.71.15", "0.71.16"));
        assert!(!is_newer("nonsense", "0.71.16"));
    }

    #[test]
    fn each_install_gets_its_own_file() {
        let release = parse_release(&serde_json::json!({
            "tag_name": "v0.71.17",
            "html_url": "https://example.invalid/r",
            "assets": [
                {"name": "Unterm-macos-v0.71.17.dmg", "browser_download_url": "u1", "size": 1, "digest": "sha256:AB"},
                {"name": "Unterm-0.71.17-x64.msi", "browser_download_url": "u2", "size": 2},
                {"name": "Unterm-0.71.17-arm64.msi", "browser_download_url": "u3", "size": 3},
                {"name": "Unterm-v0.71.17-x86_64.AppImage", "browser_download_url": "u4", "size": 4},
                {"name": "Unterm-v0.71.17-aarch64.AppImage", "browser_download_url": "u5", "size": 5}
            ]
        }))
        .unwrap();
        let mac = asset_for(&release, &Install::MacApp { bundle: "/Applications/Unterm.app".into() }).unwrap();
        assert_eq!(mac.name, "Unterm-macos-v0.71.17.dmg");
        assert_eq!(mac.sha256.as_deref(), Some("ab"));
        let msi = asset_for(&release, &Install::WindowsMsi { dir: "C:/Program Files/Unterm".into() }).unwrap();
        assert!(msi.name.ends_with(".msi"));
        assert!(asset_for(&release, &Install::Manual { reason: "deb".into() }).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_app_bundle_is_found_from_its_executable() {
        assert_eq!(
            install_of(Path::new("/Applications/Unterm.app/Contents/MacOS/unterm-cli")),
            Install::MacApp { bundle: "/Applications/Unterm.app".into() }
        );
        assert!(matches!(
            install_of(Path::new("/Users/me/code/unterm/target/release/unterm-cli")),
            Install::Manual { .. }
        ));
    }

    #[test]
    fn a_download_that_does_not_match_its_checksum_is_refused() {
        // Served from a local socket so the test needs no network.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                use std::io::{Read, Write};
                let mut stream = stream.unwrap();
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let body = b"hello";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let good = Asset {
            name: "file.bin".into(),
            url: format!("http://127.0.0.1:{port}/file.bin"),
            size: 5,
            // sha256("hello")
            sha256: Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into()),
        };
        let path = download(&good, dir.path(), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        let bad = Asset {
            name: "other.bin".into(),
            sha256: Some("00".repeat(32)),
            ..good.clone()
        };
        assert!(download(&bad, dir.path(), |_, _| {}).is_err());
        assert!(!dir.path().join("other.bin").exists());
        let unsigned = Asset { name: "x.bin".into(), sha256: None, ..good };
        assert!(download(&unsigned, dir.path(), |_, _| {}).is_err());
    }
}
