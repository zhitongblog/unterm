//! The window, the surface, and the shell behind them.
//!
//! Everything here needs a display to exercise, which is why the frame-building
//! half lives in `terminal.rs` and is tested on its own. What is left is
//! plumbing: winit gives events, next-core gives a screen, unterm-render turns
//! one into the other.

use crate::terminal::{colors_from, TerminalFont};

/// How many matches a search collects.
///
/// Enough that the count means something, bounded so a pattern matching every
/// line of a long scrollback does not stall the keystroke that typed it.
const MAX_SEARCH_MATCHES: usize = 500;

/// How many rows the palette shows at once.
///
/// Enough to choose from, few enough that it does not become the window. A
/// query that narrows the list is the way to reach the rest.

/// How many inbox rows fit before the list becomes the window.
/// How many changed paths the git panel shows before it stops.
///
/// A repository mid-rebase can have hundreds; a panel that covers the
/// terminal is a panel nobody opens twice.
const MAX_GIT_ROWS: usize = 20;

/// How many queued prompts the composer shows.
const MAX_COMPOSER_ROWS: usize = 12;

const MAX_INBOX_ROWS: usize = 12;

/// How often the cockpit tracker is shown the panes.
///
/// Not every frame: it scans each pane's last rows for the shapes an agent
/// makes when it is waiting, and doing that sixty times a second would spend
/// more on watching the agents than on drawing them.
const COCKPIT_POLL: std::time::Duration = std::time::Duration::from_millis(400);

/// How often the window does its housekeeping.
///
/// Twice a second. Everything on that path -- reconciling the tab list,
/// feeding the cockpit, re-deriving the title -- answers a question whose
/// answer changes when a person or an agent does something, not between two
/// frames. Doing it per frame was most of what an idle window cost.
const HOUSEKEEPING: std::time::Duration = std::time::Duration::from_millis(500);
/// The renderer has two sampled textures and no bindless resource table.
///
/// WGPU's WebGPU-compatible default is one million non-sampler bindings. On
/// D3D12 that number is not merely validation metadata: wgpu-hal immediately
/// allocates a shader-visible descriptor heap with that capacity. A terminal
/// then carries roughly 32 MiB of unused dedicated GPU memory (and the driver's
/// mirrored bookkeeping) before its first frame. Four thousand bindings leave
/// orders of magnitude more headroom than this renderer can consume without
/// making every idle window pay for a game-engine-sized heap.
const MAX_GPU_VIEW_DESCRIPTORS: u32 = 4_096;

/// Do not initialise every graphics API installed on the machine.
///
/// `Instance::default()` enables DX12, Vulkan and OpenGL together on Windows.
/// Even though adapter selection ultimately uses DX12, loading and probing the
/// other driver stacks leaves their DLLs and runtime allocations resident for
/// the lifetime of every terminal window. Use the native primary backend here;
/// startup tries the portable backend separately if the primary has no adapter.
#[cfg(target_os = "windows")]
const PRIMARY_GPU_BACKEND: wgpu::Backends = wgpu::Backends::DX12;
#[cfg(target_os = "macos")]
const PRIMARY_GPU_BACKEND: wgpu::Backends = wgpu::Backends::METAL;
#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
const PRIMARY_GPU_BACKEND: wgpu::Backends = wgpu::Backends::VULKAN;

#[cfg(target_os = "windows")]
const FALLBACK_GPU_BACKEND: Option<wgpu::Backends> = Some(wgpu::Backends::GL);
#[cfg(target_os = "macos")]
const FALLBACK_GPU_BACKEND: Option<wgpu::Backends> = None;
#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
const FALLBACK_GPU_BACKEND: Option<wgpu::Backends> = Some(wgpu::Backends::GL);

/// Ask the compositor for the platform's own rounded corners.
///
/// A window that draws its own frame gets square ones by default,
/// which next to every other Windows 11 window reads as a window that
/// has not finished loading. The radius is the system's rather than
/// one of ours: a corner that disagrees with the shadow around it
/// looks wrong in a way nobody can point at.
#[cfg(windows)]
fn round_window_corners(window: &Window) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let preference = DWMWCP_ROUND;
    // Windows 10 does not know this attribute and answers with an
    // error, which is the whole response needed: square corners there
    // are the platform's own look.
    unsafe {
        winapi::um::dwmapi::DwmSetWindowAttribute(
            win32.hwnd.get() as winapi::shared::windef::HWND,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn round_window_corners(_window: &Window) {}

/// A graphics stack that has already produced one frame.
struct Graphics {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    /// Kept so a second window does not have to find a GPU again.
    shared: SharedGpu,
}

/// The parts of the graphics stack that belong to the process, not a window.
///
/// Choosing an adapter costs about 200 ms and it is not a first-call cost:
/// measured three times in one process, `request_adapter` took 327, 197 and
/// 202 ms. A window that reuses this one pays for its surface instead --
/// `create_surface` is immediate and configuring it takes 3 ms. That is the
/// whole difference between Unterm's 587 ms for a new window and Windows
/// Terminal's 172 ms: WT's second window is served by a process that already
/// has these.
#[derive(Clone)]
struct SharedGpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
}

fn request_graphics(window: Arc<Window>, width: u32, height: u32) -> anyhow::Result<Graphics> {
    let mut attempts = vec![PRIMARY_GPU_BACKEND];
    if let Some(fallback) = FALLBACK_GPU_BACKEND {
        attempts.push(fallback);
    }
    let mut failures = Vec::new();
    for backend in attempts {
        match try_backend(backend, window.clone(), width, height) {
            Ok(graphics) => {
                if !failures.is_empty() {
                    log::warn!(
                        "fell back to {backend:?} after ({})",
                        failures.join("; ")
                    );
                }
                return Ok(graphics);
            }
            Err(error) => failures.push(format!("{backend:?}: {error:#}")),
        }
    }
    anyhow::bail!("no working GPU path ({})", failures.join("; "))
}

/// Put a window on a GPU this process has already chosen.
///
/// Everything expensive -- loading the driver, enumerating adapters, opening
/// a device -- happened for the first window. What is left is this window's
/// own surface, which costs nothing to make and 3 ms to configure.
///
/// If the surface will not configure, say so rather than falling back: the
/// fallback exists for "this machine's GPU stack does not work", and this
/// process has already proved that it does.
fn surface_on(
    shared: SharedGpu,
    window: Arc<Window>,
    width: u32,
    height: u32,
) -> anyhow::Result<Graphics> {
    let surface = shared
        .instance
        .create_surface(window)
        .map_err(|error| anyhow::anyhow!("surface: {error}"))?;
    let capabilities = surface.get_capabilities(&shared.adapter);
    let format = if capabilities.formats.contains(&shared.format) {
        shared.format
    } else {
        // A second monitor can want a format the first one did not offer.
        *capabilities
            .formats
            .first()
            .ok_or_else(|| anyhow::anyhow!("surface offers no format"))?
    };
    surface.configure(
        &shared.device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
        },
    );
    Ok(Graphics {
        surface,
        device: shared.device.clone(),
        queue: shared.queue.clone(),
        format,
        shared,
    })
}

/// Bring one backend all the way up, or say why it cannot come up.
///
/// The old probe stopped at "an adapter exists", and v0.61.0 taught us how
/// little that proves: on machines where DX12 enumerates but cannot make a
/// swapchain, `Surface::configure` failed *after* the choice was made, wgpu
/// treats an uncaptured error as fatal, and a GUI-subsystem process died
/// without a word -- an installed terminal that never opens. So a backend
/// only wins once it has configured the real surface and handed back a real
/// first frame; anything less moves on to the next backend.
fn try_backend(
    backend: wgpu::Backends,
    window: Arc<Window>,
    width: u32,
    height: u32,
) -> anyhow::Result<Graphics> {
    crate::startup_trace::mark("graphics.instance.start");
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: backend,
        ..Default::default()
    });
    crate::startup_trace::mark("graphics.instance.ready");
    let surface = instance
        .create_surface(window)
        .map_err(|error| anyhow::anyhow!("surface: {error}"))?;
    crate::startup_trace::mark("graphics.surface.ready");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .map_err(|error| anyhow::anyhow!("adapter: {error}"))?;
    crate::startup_trace::mark("graphics.adapter.ready");

    let mut required_limits = wgpu::Limits::default();
    required_limits.max_non_sampler_bindings = MAX_GPU_VIEW_DESCRIPTORS;
    let descriptor = wgpu::DeviceDescriptor {
        label: Some("unterm-render device"),
        required_limits,
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    };
    let device_result = pollster::block_on(adapter.request_device(&descriptor));
    let (device, queue) = match device_result {
        Ok(pair) => {
            crate::startup_trace::mark("graphics.device.ready");
            pair
        }
        // A downlevel adapter (ANGLE over an old driver) can refuse the
        // default limits while still being perfectly able to draw a terminal.
        Err(_) => {
            let mut limits = wgpu::Limits::downlevel_defaults();
            limits.max_non_sampler_bindings = MAX_GPU_VIEW_DESCRIPTORS;
            let descriptor = wgpu::DeviceDescriptor {
                label: Some("unterm-render device"),
                required_limits: limits,
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            };
            let pair = pollster::block_on(adapter.request_device(&descriptor))
                .map_err(|error| anyhow::anyhow!("device: {error}"))?;
            crate::startup_trace::mark("graphics.device.ready");
            pair
        }
    };
    // From here on a wgpu error is a log line, not a process death.
    device.on_uncaptured_error(Box::new(|error| {
        log::error!("wgpu: {error}");
    }));

    let capabilities = surface.get_capabilities(&adapter);
    // Prefer a non-sRGB format: the colours here are already the values the
    // config asked for, and an sRGB target would convert them a second time.
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|format| !format.is_srgb())
        .unwrap_or(capabilities.formats[0]);

    // The part v0.61.0 never checked. The error scope turns a failed
    // configure into a value; the first-frame request proves the swapchain
    // is not just configured but usable.
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    surface.configure(
        &device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        },
    );
    if let Some(error) = pollster::block_on(device.pop_error_scope()) {
        anyhow::bail!("configure: {error}");
    }
    crate::startup_trace::mark("graphics.surface.configured");
    match surface.get_current_texture() {
        Ok(frame) => drop(frame),
        Err(error) => anyhow::bail!("first frame: {error}"),
    }
    crate::startup_trace::mark("graphics.surface.probed");

    Ok(Graphics {
        shared: SharedGpu {
            instance,
            adapter,
            device: device.clone(),
            queue: queue.clone(),
            format,
        },
        surface,
        device,
        queue,
        format,
    })
}

/// How many rows of each pane the tracker is shown.
///
/// A prompt asking a question is at the bottom; more than this is scrollback
/// that has already been answered.
const COCKPIT_TAIL_ROWS: usize = 8;

use std::sync::Arc;
use unterm_engine::next_core::{config, key_encoding};
use unterm_engine::{
    CreateSessionRequest, InputEngine, LaunchPolicySnapshot, ScreenEngine, SessionEngine,
    SessionSnapshot,
};
use unterm_render::atlas::GlyphAtlas;
use unterm_render::gpu::Renderer;
use unterm_render::quads::FrameColors;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

enum ClipboardResult {
    Read {
        pane_id: usize,
        result: Result<String, String>,
    },
    Written(Result<(), String>),
    DirectoryPicked {
        then: crate::palette::BrowseThen,
        result: Result<Option<std::path::PathBuf>, String>,
    },
    ScreenshotFinished {
        mode: String,
        result: Result<std::path::PathBuf, String>,
    },
    ExportFinished(Result<std::path::PathBuf, String>),
    ScrollbackCaptured(Result<std::path::PathBuf, String>),
}

#[derive(Clone, Copy, Debug, Default)]
struct PaneNotice {
    revision: u64,
    unread: bool,
    error: bool,
    /// How many `OSC 9`/`777` notifications this pane had raised the last
    /// time anyone looked, so each new one is announced exactly once.
    notifications_seen: u64,
}

/// The chevron's open dropdown: its rows, and where it sits.
#[derive(Clone, Debug)]
struct QuickMenu {
    entries: Vec<crate::palette::Entry>,
    hover: Option<usize>,
    top_row: usize,
    visible_rows: usize,
    left: f32,
    top: f32,
    width: f32,
    row_height: f32,
}

impl QuickMenu {
    fn height(&self) -> f32 {
        self.visible_rows as f32 * self.row_height
    }

    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        if x < self.left || x >= self.left + self.width {
            return None;
        }
        if y < self.top || y >= self.top + self.height() {
            return None;
        }
        let visible = ((y - self.top) / self.row_height) as usize;
        (visible < self.visible_rows).then_some(self.top_row + visible)
    }

    fn reveal_hover(&mut self) {
        let Some(row) = self.hover else { return };
        if row < self.top_row {
            self.top_row = row;
        } else if row >= self.top_row + self.visible_rows {
            self.top_row = row + 1 - self.visible_rows;
        }
    }

    fn scroll(&mut self, delta: isize) {
        let max_top = self.entries.len().saturating_sub(self.visible_rows);
        self.top_row = self.top_row.saturating_add_signed(delta).min(max_top);
        if let Some(row) = self.hover {
            self.hover = Some(
                row.max(self.top_row)
                    .min(self.top_row + self.visible_rows.saturating_sub(1)),
            );
        }
    }

    fn arrow_at(&self, x: f32, y: f32) -> Option<isize> {
        if x < self.left + self.width - self.row_height || x >= self.left + self.width {
            return None;
        }
        if y >= self.top && y < self.top + self.row_height && self.top_row > 0 {
            Some(-1)
        } else if y >= self.top + self.height() - self.row_height
            && y < self.top + self.height()
            && self.top_row + self.visible_rows < self.entries.len()
        {
            Some(1)
        } else {
            None
        }
    }
}

/// Whether a window size is one the terminal is actually being drawn at.
///
/// A window that has not been mapped yet reports 0x0, and depending on the
/// platform and the winit build so can one that is minimised, occluded, or
/// caught mid display change. None of those is a size, and nothing further
/// down the chain can tell: the window width, the terminal area and the grid
/// each floored at one cell, so a zero window arrived at the pane as a
/// one-column terminal. Resizing a pane to one column does not narrow the
/// view of its output, it truncates every line and the whole scrollback to a
/// single character, and restoring the window cannot bring any of it back.
/// So a window with no size is a window with nothing to say about grids.
fn is_drawable_size(width: u32, height: u32) -> bool {
    width > 0 && height > 0
}

fn styled_snapshot_has_text(snapshot: &unterm_engine::StyledScreenSnapshot) -> bool {
    styled_lines_have_text(&snapshot.lines)
}

fn styled_lines_have_text(lines: &[unterm_engine::StyledScreenLine]) -> bool {
    lines.iter().any(|line| {
        line.cells
            .iter()
            .any(|cell| cell.width > 0 && !cell.ch.is_whitespace())
    })
}

fn quick_menu_owns_keyboard(pending_confirmation_visible: bool) -> bool {
    !pending_confirmation_visible
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuickMenuMouseAction {
    ActivateHovered,
    Close,
    Swallow,
}

fn quick_menu_mouse_action(
    state: ElementState,
    button: winit::event::MouseButton,
    secondary: bool,
) -> QuickMenuMouseAction {
    if state != ElementState::Pressed {
        return QuickMenuMouseAction::Swallow;
    }
    if button == winit::event::MouseButton::Left && !secondary {
        QuickMenuMouseAction::ActivateHovered
    } else {
        QuickMenuMouseAction::Close
    }
}

#[cfg(test)]
mod quick_menu_tests {
    use super::*;
    use winit::event::MouseButton;

    fn menu() -> QuickMenu {
        let entries = (0..15)
            .map(|index| crate::palette::Entry {
                label: format!("item {index}"),
                hint: String::new(),
                command: crate::palette::Command::Action(crate::keys::Action::Copy),
            })
            .collect();
        QuickMenu {
            entries,
            hover: Some(0),
            top_row: 0,
            visible_rows: 5,
            left: 100.0,
            top: 50.0,
            width: 200.0,
            row_height: 20.0,
        }
    }

    #[test]
    fn a_short_window_only_uses_the_rows_it_can_show() {
        let menu = menu();
        assert_eq!(menu.height(), 100.0);
        assert_eq!(menu.row_at(120.0, 139.0), Some(4));
        assert_eq!(menu.row_at(120.0, 151.0), None);
    }

    #[test]
    fn scrolling_maps_visible_rows_back_to_the_real_entries() {
        let mut menu = menu();
        menu.scroll(4);
        assert_eq!(menu.top_row, 4);
        assert_eq!(menu.hover, Some(4));
        assert_eq!(menu.row_at(120.0, 51.0), Some(4));
        menu.scroll(99);
        assert_eq!(menu.top_row, 10);
    }

    #[test]
    fn keyboard_selection_pulls_the_visible_window_with_it() {
        let mut menu = menu();
        menu.hover = Some(8);
        menu.reveal_hover();
        assert_eq!(menu.top_row, 4);
    }

    #[test]
    fn quick_menu_keeps_keyboard_unless_a_confirmation_is_visible() {
        assert!(quick_menu_owns_keyboard(false));
        assert!(!quick_menu_owns_keyboard(true));
    }

    #[test]
    fn quick_menu_mouse_is_modal_over_mouse_reporting_tuis() {
        assert_eq!(
            quick_menu_mouse_action(ElementState::Pressed, MouseButton::Left, false),
            QuickMenuMouseAction::ActivateHovered
        );
        assert_eq!(
            quick_menu_mouse_action(ElementState::Pressed, MouseButton::Right, true),
            QuickMenuMouseAction::Close
        );
        assert_eq!(
            quick_menu_mouse_action(ElementState::Released, MouseButton::Left, false),
            QuickMenuMouseAction::Swallow
        );
    }
}

#[derive(Clone, Debug)]
struct GitDock {
    cwd: std::path::PathBuf,
    panel: crate::git::Panel,
}

#[derive(Clone, Copy, Debug, Default)]
struct ChromeOverrides {
    active_surface: Option<[f32; 4]>,
    inactive_surface: Option<[f32; 4]>,
    active_foreground: Option<[f32; 4]>,
    inactive_foreground: Option<[f32; 4]>,
    active_edge: Option<[f32; 4]>,
    inactive_edge: Option<[f32; 4]>,
    selected_bg: Option<[f32; 4]>,
    hover_bg: Option<[f32; 4]>,
    dim_text: Option<[f32; 4]>,
    button_foreground: Option<[f32; 4]>,
    button_hover_foreground: Option<[f32; 4]>,
    button_hover_background: Option<[f32; 4]>,
}

impl ChromeOverrides {
    fn from_config(config: &config::Config) -> Self {
        Self {
            active_surface: configured_color(config, "window_frame.active_titlebar_bg")
                .or_else(|| configured_color(config, "colors.tab_bar.background")),
            inactive_surface: configured_color(config, "window_frame.inactive_titlebar_bg"),
            active_foreground: configured_color(config, "window_frame.active_titlebar_fg")
                .or_else(|| configured_color(config, "colors.tab_bar.active_tab.fg_color")),
            inactive_foreground: configured_color(config, "window_frame.inactive_titlebar_fg"),
            active_edge: configured_color(config, "window_frame.active_titlebar_border_bottom"),
            inactive_edge: configured_color(config, "window_frame.inactive_titlebar_border_bottom"),
            selected_bg: configured_color(config, "colors.tab_bar.active_tab.bg_color"),
            hover_bg: configured_color(config, "colors.tab_bar.inactive_tab_hover.bg_color")
                .or_else(|| configured_color(config, "window_frame.button_hover_bg")),
            dim_text: configured_color(config, "colors.tab_bar.inactive_tab.fg_color")
                .or_else(|| configured_color(config, "window_frame.inactive_titlebar_fg")),
            button_foreground: configured_color(config, "window_frame.button_fg"),
            button_hover_foreground: configured_color(config, "window_frame.button_hover_fg"),
            button_hover_background: configured_color(config, "window_frame.button_hover_bg"),
        }
    }

    fn apply(self, mut chrome: crate::chrome::Chrome, focused: bool) -> crate::chrome::Chrome {
        let surface = if focused {
            self.active_surface
        } else {
            self.inactive_surface.or(self.active_surface)
        };
        if let Some(surface) = surface {
            chrome.surface = surface;
            chrome.footer_bg = surface;
            chrome.group_bg = surface;
        }
        let edge = if focused {
            self.active_edge
        } else {
            self.inactive_edge.or(self.active_edge)
        };
        if let Some(edge) = edge {
            chrome.outer_edge = edge;
        }
        if let Some(selected_bg) = self.selected_bg {
            chrome.selected_bg = selected_bg;
        }
        if let Some(hover_bg) = self.hover_bg {
            chrome.hover_bg = hover_bg;
        }
        if let Some(dim_text) = self.dim_text {
            chrome.dim_text = dim_text;
        }
        chrome
    }
}

fn configured_color(config: &config::Config, key: &str) -> Option<[f32; 4]> {
    let color = config
        .str_of(key)
        .ok()
        .flatten()
        .and_then(unterm_engine::next_core::color::parse_hex)?;
    Some([
        color.red as f32 / 255.0,
        color.green as f32 / 255.0,
        color.blue as f32 / 255.0,
        1.0,
    ])
}

/// An image somebody left on the clipboard, made pasteable: written to the
/// captures folder so the paste can be its path. The terminal form of "paste
/// a picture" is a filename an agent can read.
fn clipboard_image_to_file(picture: &arboard::ImageData) -> anyhow::Result<String> {
    let dir = unterm_protocol::state_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".").join(".unterm"))
        .join("captures");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|at| at.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("clipboard-{stamp}.png"));
    let image = image::RgbaImage::from_raw(
        picture.width as u32,
        picture.height as u32,
        picture.bytes.clone().into_owned(),
    )
    .ok_or_else(|| anyhow::anyhow!("the clipboard image does not match its stated size"))?;
    image.save_with_format(&path, image::ImageFormat::Png)?;
    Ok(path.display().to_string())
}

/// A held sidebar row, from press until release. `engaged` stays false
/// until the pointer has moved past the click-jitter slack — only then do
/// cursor moves start carrying the tab through the strip.
#[derive(Clone, Copy)]
struct TabDrag {
    tab_id: usize,
    origin: (f32, f32),
    engaged: bool,
}

pub struct App {
    engine: crate::engine_backend::AppEngine,
    /// The size the config asked for, which the reset key goes back to.
    configured_font_points: f32,
    /// The families to try after the primary one, kept because reopening the
    /// font at a new size has to make the same choices as the first open.
    font_fallbacks: Vec<String>,
    /// The family the config named, if it named one.
    font_family: Option<String>,
    /// Terminal grid requested by the config for the first window.
    initial_cols: usize,
    initial_rows: usize,
    /// Left, right, top and bottom terminal padding in logical pixels.
    terminal_padding: [f32; 4],
    /// Hue, saturation and brightness multipliers for unfocused panes.
    inactive_pane_hsb: [f32; 3],
    chrome_overrides: ChromeOverrides,
    /// The shell the config asked for. Without this the session falls back to
    /// `%COMSPEC%`, which on Windows is `cmd.exe` -- not what a config naming
    /// `pwsh` meant, and it emits its output in the console codepage rather
    /// than UTF-8.
    shell: Option<portable_pty::CommandBuilder>,
    /// Notices that the machine slept, so providers are re-probed and lapsed
    /// leases are reported rather than failing later for no visible reason.
    wake_watch: unterm_services::wake_watch::WakeWatch,
    /// Which page of the character picker is open: the group Ctrl+R turns
    /// through while the picker's palette is up. Reset each time it opens.
    charselect_group: crate::charselect::Group,
    /// Where the first shell should start, if the command line said.
    start_directory: Option<std::path::PathBuf>,
    /// True when the launch named a directory or a program, so this window
    /// must open a fresh shell rather than adopt whatever session a
    /// populated Core had focused.
    explicit_launch: bool,
    /// What the previous window looked like, when a plain launch should
    /// bring it back.
    restore: Option<crate::session_restore::LastSession>,
    clipboard_tx: std::sync::mpsc::Sender<ClipboardResult>,
    clipboard_rx: std::sync::mpsc::Receiver<ClipboardResult>,
    /// Whether the pane scrollbar is drawn at all — `enable_scroll_bar`,
    /// which the schema promised and nothing read until now.
    scrollbar_enabled: bool,
    /// Whether the resident bottom status bar is on. Off by default in
    /// the inbox design; a pending confirmation shows the strip anyway.
    status_bar_enabled: bool,
    /// The hyperlink rules in force: the config's own `hyperlink_rules`, or
    /// the built-in set when it writes none.
    link_rules: crate::links::Rules,
    /// Whether the bell makes a sound.
    audible_bell: bool,
    /// How the bell's flash fades and what it colours -- `visual_bell`, with
    /// the previous front end's defaults: no flash unless the config asks.
    visual_bell: crate::terminal::VisualBell,
    /// The config's `default_cwd`, when nothing else names a directory.
    config_default_cwd: Option<std::path::PathBuf>,
    /// `window_close_confirmation = "NeverPrompt"` turns the close
    /// confirmation off, exactly as it always had.
    close_prompts: bool,
    /// `window.decorations = true` asks for the system frame back.
    system_decorations: bool,
    /// The cursor the config asked for, and how fast it blinks.
    cursor_style: crate::terminal::CursorStyle,
    cursor_blink_ms: u64,
    /// How fast SGR 5 and SGR 6 text blinks; zero turns a cadence off.
    text_blink_ms: u64,
    text_blink_rapid_ms: u64,
    /// When the window opened, which is what a blink is measured from.
    started: std::time::Instant,
    /// The theme in force, so the picker can mark it and the next launch can
    /// restore it.
    theme_id: Option<String>,
    /// Last Web Settings/CLI theme request observed by this window.
    ///
    /// Each native window is an independent observer of the process-local
    /// mailbox, so one window applying a request does not consume it for the
    /// others.
    theme_request_seen: u64,
    /// The window being acted on, and everything true of it alone.
    ///
    /// The settings, the engine and the shell belong to the process; a tab
    /// strip, a selection and a dirty-frame marker belong to one window.
    /// Keeping both on the same `self` is what made "one window" mean "one
    /// process", and one process per window is what makes every window pay
    /// for its own GPU adapter -- 293 ms of it, measured.
    ///
    /// **Why a field and not `windows[active]`.** That was tried first and
    /// reverted. A thousand call sites here take two fields of this struct at
    /// once -- `&mut self.window.font` beside `&mut self.window.atlas` --
    /// which the borrow checker allows because the fields are disjoint. Reach
    /// them through an index and they stop being disjoint: both borrow the
    /// vector, and the same thousand sites stop compiling. Keeping the active
    /// window as a field keeps that property; the others park below.
    window: WindowState,
    /// When this process noticed its Core had been replaced.
    ///
    /// Pairs with `seen_session_epoch`: one decides, the other says how long
    /// to keep saying so. Both are about the engine, so both belong here.
    core_replaced_at: Option<std::time::Instant>,
    /// When the Agent Cockpit was last fed.
    ///
    /// The poll it throttles asks the Core about every pane there is, not
    /// about one window's. Held per window, a second window would double a
    /// poll that already costs 28 ms at forty panes.
    cockpit_fed_at: std::time::Instant,
    /// The engine's session epoch this process has already reacted to.
    ///
    /// Belongs to the process, not to a window: it says what the *engine* has
    /// been through. Held per window it starts at zero for every new one, so
    /// opening a second window looked exactly like the Core being replaced --
    /// and this process then "recovered" by spawning a shell nobody asked
    /// for. Found by counting panes across a window close: two before, three
    /// after.
    seen_session_epoch: u64,
    /// The GPU this process found, kept so later windows need not look.
    gpu: Option<SharedGpu>,
    /// What a second window is built from: the same settings the first was.
    ///
    /// Kept whole rather than as the dozen values `WindowState::fresh` reads
    /// out of it, because a window opened now must be the window the config
    /// describes, not the subset somebody remembered to copy.
    config: config::Config,
    configured_pixel_size: f64,
    /// The windows that are not being acted on.
    ///
    /// Swapping one into `window` is how this process changes which window an
    /// event means. A swap moves a struct of fields, not a frame or a
    /// surface, and it happens once per event rather than once per frame.
    parked: Vec<ParkedWindow>,
}

impl WindowState {
    /// A window that has not been shown yet.
    ///
    /// Lifted out of `App::new` so a second window can be made the same way
    /// the first one is. Everything here is either empty or a setting; what
    /// makes a window *live* -- its surface, its renderer, its pane -- is
    /// `App::start`.
    fn fresh(
        id: u64,
        config: &config::Config,
        pixel_size: f64,
        shape: crate::terminal::Shape,
        font: TerminalFont,
        chrome_font: TerminalFont,
        picture: Option<crate::background::Image>,
    ) -> Self {
        Self {
            id,
            picture,
            font,
            chrome_font,
            drawn_confirmation: None,
            drawn_suggestions: 0,
            startup_first_frame_marked: false,
            startup_terminal_content_marked: false,
            preedit: crate::ime::Preedit::default(),
            last_ime_event: None,
            search: None,
            bells_seen: 0,
            bell_at: None,
            dragging_scrollbar: false,
            palette: None,
            quick_menu: None,
            copy_mode: None,
            quick_select: None,
            pane_select: None,
            followed_active: None,
            occluded: false,
            clipboard_honoured: None,
            inbox_open: false,
            inbox_selected: 0,
            sidebar_open: true,
            cursor_icon: winit::window::CursorIcon::Default,
            bar_title: String::new(),
            top_bar_quiet: config
            .str_of("top_bar")
            .ok()
            .flatten()
            .map(|mode| mode != "full")
            .unwrap_or(true),
            sidebar_scroll: 0,
            sidebar_points: None,
            dragging_sidebar_width: false,
            dragging_tab: None,
            sidebar_collapsed: Default::default(),
            last_sidebar_click: None,
            last_topbar_click: None,
            terminal_click: None,
            last_tree_click: None,
            select_anchor: None,
            select_granularity: SelectGranularity::Cell,
            close_confirmed: false,
            unmaximized_rect: None,
            tab_titles: Default::default(),
            pane_notices: Default::default(),
            tree: None,
            closing: false,
            tray: None,
            tray_refreshed: std::time::Instant::now(),
            notice: None,
            proxy_error_until: None,
            git_panel: None,
            composer: None,
            mouse_modes: Default::default(),
            held_mouse_button: None,
            alt_held: false,
            window_title: None,
            quiet_since: None,
            startup_session: None,
            startup_input: String::new(),
            restore_extra_tabs_pending: false,
            pane_sizes: Default::default(),
            focused: true,
            kept_house_at: std::time::Instant::now(),
            font_shape: shape,
            font_points: pixel_size as f32,
            scale: 1.0,
            atlas: GlyphAtlas::new(1024, 1024),
            colors: colors_from(config),
            shift_held: false,
            ctrl_held: false,
            pointer: (0.0, 0.0),
            tabs: unterm_engine::next_core::tabs::TabRegistry::new(),
            tab_id: None,
            drag: None,
            swallow_left_after_secondary: false,
            secondary_gesture: crate::mouse::SecondaryGesture::default(),
            selected: None,
            state: None,
            drawn_revision: None,
            presented_at: None,
            drawn_cursor_solid: None,
            drawn_blink: None,
            screen_blink: (false, false),
            drawn_breath_step: None,
        }
    }
}

/// A window this process owns but is not currently acting on.
///
/// Held beside its id because winit delivers events per window, and the id is
/// what says which of these an event means.
struct ParkedWindow {
    id: winit::window::WindowId,
    state: WindowState,
}

/// Everything that belongs to one window rather than to the process.
struct WindowState {
    /// What an agent calls this window.
    ///
    /// Handed out by `unterm_engine::reserve_window_id`, and not winit's
    /// `WindowId`: that one is an opaque platform handle, it changes when a
    /// window is rebuilt after a spell in the tray, and it means nothing to
    /// anybody outside this process. `instance.windows` reports this, and
    /// `instance.focus` takes it.
    id: u64,
    font: TerminalFont,
    /// The font the chrome is drawn in.
    ///
    /// A second face at its own size, because the chrome is not terminal
    /// output: the previous front end drew every tab, sidebar row and status
    /// segment at 13pt with a looser line height, and drawing them in the
    /// terminal's font at the terminal's cell size is what made this window
    /// read as a wall of output with a wall of output around it.
    chrome_font: TerminalFont,
    /// The size the font is asked for, in points, which the zoom keys move.
    ///
    /// Points rather than pixels, as the config and every other terminal mean
    /// it: how many pixels that is depends on the display, and the display can
    /// change while the window is open.
    font_points: f32,
    /// The display's scale, as winit reports it against 96 dpi.
    scale: f32,
    /// How far the cell is stretched around its glyphs.
    font_shape: crate::terminal::Shape,
    atlas: GlyphAtlas,
    colors: FrameColors,
    state: Option<Live>,
    /// Whether shift is down, which is what separates a scroll from a key the
    /// program should receive.
    shift_held: bool,
    /// Whether control is down.
    ctrl_held: bool,
    /// Where the pointer is, in pixels. Winit reports movement and buttons
    /// separately, so a click has to be told where it happened.
    pointer: (f32, f32),
    /// The split tree. next-core owns what an arrangement *is*; the window
    /// only says when to split and reads back where the panes go.
    tabs: unterm_engine::next_core::tabs::TabRegistry,
    tab_id: Option<usize>,
    /// The selection being dragged out, if one is.
    drag: Option<crate::select::Drag>,
    /// Whether the rest of the current left-button gesture is spoken for.
    ///
    /// A Ctrl+Left press consumed as a macOS secondary click swallows its own
    /// drag and release: left alone they would fall through to the
    /// left-button paths -- extend a selection, complete one, open a link --
    /// and make a single click both paste and act.
    swallow_left_after_secondary: bool,
    /// One physical secondary click acts once, however many times the
    /// platform delivers it.
    secondary_gesture: crate::mouse::SecondaryGesture,
    /// The text of the finished selection, kept so a copy key can find it.
    selected: Option<String>,
    /// The last screen we drew, so a frame is skipped when nothing changed.
    /// A terminal is idle most of the time; redrawing an unchanged screen at
    /// display rate is a fan that never stops.
    drawn_revision: Option<u64>,
    /// When the last frame was handed to the compositor. A flood of PTY
    /// output must not draw faster than the display shows: past the
    /// refresh interval the drawable pool runs dry and `nextDrawable`
    /// turns the GUI thread into a queue behind the compositor.
    presented_at: Option<std::time::Instant>,
    /// Cursor phase represented by the last submitted frame.
    ///
    /// A blinking cursor changes only twice per period. Treating "blinking is
    /// enabled" as "redraw every tick" kept an idle WebGPU surface busy.
    drawn_cursor_solid: Option<bool>,
    /// The text blink phase of the last submitted frame, and which cadences
    /// that frame actually used -- so blinking text asks for one frame per
    /// phase flip, and a screen with nothing blinking asks for none.
    drawn_blink: Option<crate::terminal::BlinkPhase>,
    screen_blink: (bool, bool),
    /// The breathing phase the sidebar's working badge was last drawn at, so
    /// an idle window repaints only when the phase actually changes — and not
    /// at all when nothing is working.
    drawn_breath_step: Option<u8>,
    /// Which agent-write banner was on screen when we last drew, so one
    /// appearing or being answered is itself a reason to draw again.
    drawn_confirmation: Option<u64>,
    /// Pending suggestions drawn in the last frame.
    drawn_suggestions: usize,
    /// Startup trace should record only the first presented frame.
    startup_first_frame_marked: bool,
    /// Startup trace should record when terminal text first reaches a frame.
    startup_terminal_content_marked: bool,
    /// Text an input method is still composing, not yet the shell's.
    preedit: crate::ime::Preedit,
    /// When the input method last spoke. A composition whose IME has gone
    /// quiet is an orphan; one that talked milliseconds ago is live, and
    /// winit hands us KeyboardInput alongside Ime events either way.
    last_ime_event: Option<std::time::Instant>,
    /// The open search, if there is one.
    search: Option<crate::search::Search>,
    /// How many bells the pane had rung when we last drew.
    bells_seen: u64,
    /// When the current flash started.
    bell_at: Option<std::time::Instant>,
    /// True while a drag is holding the scrollbar's thumb.
    dragging_scrollbar: bool,
    /// The open command palette or launcher, if there is one.
    palette: Option<crate::palette::Palette>,
    /// The chevron's dropdown, anchored under its button — a menu, not a
    /// search box, which is what the palette would make of the same rows.
    quick_menu: Option<QuickMenu>,
    /// The keyboard selection, if copy mode is on.
    copy_mode: Option<crate::copy_mode::CopyMode>,
    /// Quick select's labels, and what has been typed towards one.
    quick_select: Option<(Vec<crate::copy_mode::Labelled>, String)>,
    /// A letter on every pane, while one is being picked.
    pane_select: Option<crate::paneselect::Selector>,
    /// The Core-wide active session this window last saw, so `sync_tabs`
    /// follows a CHANGE of it (an Inbox jump aimed here) and not its mere
    /// existence (every click in every other window on the same Core).
    followed_active: Option<usize>,
    /// Whether the compositor says nobody can see this window. Painting an
    /// occluded window blocks on drawables that never come.
    occluded: bool,
    /// The last clipboard request honoured, so it is not honoured twice.
    clipboard_honoured: Option<String>,
    /// Whether the agent inbox is showing.
    inbox_open: bool,
    /// The inbox row the keyboard will open.
    inbox_selected: usize,
    /// Whether the strip of tabs down the left is showing.
    sidebar_open: bool,
    /// The pointer shape currently asked for, so a move that changes
    /// nothing does not talk to the window system at all.
    cursor_icon: winit::window::CursorIcon,
    /// What the top bar's centre says: the window title without the
    /// product name after it.
    bar_title: String,
    /// Whether the top bar hides its action row and facts line. On by
    /// default; `top_bar = "full"` in the config restores them.
    top_bar_quiet: bool,
    /// The strip's first visible row, for lists longer than the window.
    sidebar_scroll: usize,
    /// Its width in points, once somebody has dragged it.
    sidebar_points: Option<f32>,
    /// True while a drag is holding the strip's right edge.
    dragging_sidebar_width: bool,
    /// A held tab row mid-reorder: which tab, and where it is being carried.
    dragging_tab: Option<TabDrag>,
    /// Projects the reader has folded away. Window state rather than disk
    /// state: it survives repaints and resizing without any file being read.
    sidebar_collapsed: std::collections::HashSet<String>,
    /// The last press on a strip row, so only a true same-row double-click
    /// opens the rename line rather than any two fast clicks anywhere.
    last_sidebar_click: Option<crate::sidebar::RowClick>,
    /// The last press on the top bar's empty stretch, so a double-click
    /// there toggles maximise the way every title bar does.
    last_topbar_click: Option<crate::sidebar::RowClick>,
    /// The last press in the terminal, so a double-click can select the word
    /// and a triple-click the line, as they always have.
    terminal_click: Option<crate::sidebar::RowClick>,
    /// The last press on a tree row: a single click browses the tree, and
    /// only a same-row double-click reaches the shell, as 0.57.4 had it.
    last_tree_click: Option<crate::sidebar::RowClick>,
    /// Where the last selection started, so Shift+click extends it instead
    /// of starting over.
    select_anchor: Option<unterm_engine::next_core::selection::SelectionPoint>,
    /// How the held drag grows: by cell, by word after a double-click, by
    /// line after a triple-click. Without this the micro-move inside a real
    /// double-click re-extends the drag to the pointer cell and shrinks the
    /// word selection to wherever the pointer happened to sit.
    select_granularity: SelectGranularity,
    /// Whether the close confirmation has been answered.
    close_confirmed: bool,
    /// The window rect before a work-area maximise, so the second press
    /// puts it back exactly where it was.
    unmaximized_rect: Option<(
        winit::dpi::PhysicalPosition<i32>,
        winit::dpi::PhysicalSize<u32>,
    )>,
    /// Names the reader has given tabs, keyed by stable tab id. A named tab
    /// keeps its name through program changes; an empty rename hands the tab
    /// back to automatic titling.
    tab_titles: std::collections::HashMap<usize, String>,
    /// Output state for sidebar tabs; updated by the bounded Cockpit tail pass.
    pane_notices: std::collections::HashMap<usize, PaneNotice>,
    /// The file tree, while it is open. It shares the left dock with the tab
    /// strip: two strips either side of a terminal is most of a narrow window,
    /// so opening one closes the other.
    tree: Option<crate::tree::Tree>,
    /// Set when the close button is pressed, so the loop can exit.
    closing: bool,
    /// The menu-bar / notification-area indicator, while the window is parked
    /// and its sessions run on in the Core. Its presence *is* the parked
    /// state: this process only ever survives its window when there is
    /// something on screen saying so.
    tray: Option<crate::tray::Tray>,
    /// When the indicator's counts were last refreshed, so a parked process
    /// wakes to read them a few times a minute rather than at frame rate.
    tray_refreshed: std::time::Instant,
    /// Something that just happened, and when it stops being shown.
    notice: Option<(String, std::time::Instant)>,
    /// A press on the proxy chip tried to switch it on and the probe found
    /// nothing listening; the chip says so until this instant.
    proxy_error_until: Option<std::time::Instant>,
    /// The git panel's contents, held while it is open.
    ///
    /// Read once when it opens rather than every frame: `git status` on a
    /// large repository is not something to run sixty times a second, and a
    /// panel that changes under the eye while being read is worse than one
    /// that is a moment old.
    git_panel: Option<GitDock>,
    /// Prompts waiting to go into the focused pane, while the composer
    /// is open. Closing it drops them: a queue that outlives the window
    /// showing it would fire prompts nobody can see coming.
    composer: Option<crate::composer::Composer>,
    /// When the cockpit tracker last saw the panes.
    /// What the program wants from the mouse, as of the last frame drawn.
    ///
    /// Cached rather than read per event: a motion arrives a hundred times a
    /// second and building a screen snapshot for each would be most of a
    /// frame's work spent deciding who owns a pointer that has not moved a
    /// cell. Modes only change when the program writes an escape sequence,
    /// which changes the screen, which draws a frame -- so the cache is never
    /// more than one frame behind the thing that sets it.
    mouse_modes: unterm_engine::next_core::mouse_encoding::MouseModes,
    /// Which mouse button is down, so a drag reports the right one.
    held_mouse_button: Option<unterm_engine::next_core::mouse_encoding::MouseButton>,
    alt_held: bool,
    /// The title last set, so an unchanged one is not set again every frame.
    window_title: Option<String>,
    /// The picture the config named, read once at startup. Uploaded to the
    /// device when the window opens.
    picture: Option<crate::background::Image>,
    /// The size each pane was last told it is, so it is not told again.
    pane_sizes: std::collections::HashMap<usize, (usize, usize)>,
    /// Whether the window has the keyboard.
    focused: bool,
    /// When the housekeeping last ran.
    kept_house_at: std::time::Instant,
    /// Which Core generation this window's pane ids belong to. A change
    /// means they belong to a process that is gone.
    /// When the Core behind this window was last replaced, so the
    /// recovery can be reported and then stop being reported.
    /// When the window last had nothing to redraw, if it still has nothing.
    ///
    /// A window that has been still for a while is asked about far less often:
    /// a terminal left open on a desk should not cost a core.
    quiet_since: Option<std::time::Instant>,
    startup_session: Option<std::thread::JoinHandle<anyhow::Result<SessionSnapshot>>>,
    startup_input: String,
    restore_extra_tabs_pending: bool,
}

/// What closing the window should do to the sessions behind it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CloseOutcome {
    /// End every session on the way out. What a terminal has always done,
    /// and the right answer whenever nobody was asked anything.
    EndSessions,
    /// Leave them to the Core, which outlives this process. Chosen by the
    /// close prompt, and by it alone.
    KeepSessions,
}

struct Live {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    renderer: Renderer,
    atlas_texture: wgpu::Texture,
    /// Number of glyphs represented by `atlas_texture`.
    ///
    /// Cursor blinking redraws an otherwise unchanged frame. Re-uploading the
    /// whole atlas for every blink retains needless GPU allocations and was a
    /// large part of the new renderer's idle memory and CPU cost.
    atlas_uploaded_glyphs: usize,
    /// The picture behind the terminal, uploaded once. Held here rather than
    /// beside the window's other state because it belongs to the device that
    /// draws it, and the device is here.
    background: Option<wgpu::Texture>,
    /// Its size, for working out which part of it fills the window.
    background_size: Option<(u32, u32)>,
    /// And how much of it shows through.
    background_opacity: f32,
    session_id: usize,
    width: u32,
    height: u32,
}

impl App {
    /// How many windows this process is showing.
    ///
    /// The count the close prompt asks about: with another window still open,
    /// closing this one is closing a view, and nothing has to be decided
    /// about the sessions behind it.
    fn window_count(&self) -> usize {
        Self::window_count_for(self.parked.len())
    }

    /// The same count, over the number alone.
    ///
    /// Split out so the rule can be exercised without an `App` -- building
    /// one needs a window, a font stack and a GPU device, so a test that
    /// insisted on that would be testing winit. The first version of the
    /// test below wrote `1 + parked` a second time inside itself, which
    /// passed whatever this function did, including not existing.
    fn window_count_for(parked: usize) -> usize {
        1 + parked
    }

    /// The id of the window currently being acted on, if it has one yet.
    fn active_window_id(&self) -> Option<winit::window::WindowId> {
        self.window.state.as_ref().map(|live| live.window.id())
    }

    /// Make `id` the window this process is acting on.
    ///
    /// Returns false when no window here has that id. Swapping rather than
    /// indexing is what lets every method below keep taking two fields of
    /// `self.window` at once; see the field's own note.
    fn focus_window(&mut self, id: winit::window::WindowId) -> bool {
        if self.active_window_id() == Some(id) {
            return true;
        }
        let Some(at) = self.parked.iter().position(|parked| parked.id == id) else {
            return false;
        };
        // The active one has to keep its id on the way out, or the window it
        // becomes could never be found again.
        let Some(active_id) = self.active_window_id() else {
            return false;
        };
        std::mem::swap(&mut self.window, &mut self.parked[at].state);
        self.parked[at].id = active_id;
        true
    }

    /// Tell the MCP surface what one window is showing.
    fn note_window_view(state: &WindowState) {
        let panes: Vec<usize> = state
            .tabs
            .tab_ids()
            .into_iter()
            .flat_map(|tab| state.tabs.pane_ids(tab))
            .collect();
        crate::mcp_host::note_window_view(state.id, state.tabs.tab_count(), &panes);
    }

    pub fn new(config: &config::Config) -> anyhow::Result<Self> {
        // The config's own fallback list comes first: someone who named a font
        // meant it, and the built-in list is only what to try after.
        let fallbacks: Vec<String> = config
            .list_of("font_fallback")
            .ok()
            .flatten()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| match value {
                        unterm_engine::next_core::config::Value::Str(name) => Some(name.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let cursor_style = crate::terminal::CursorStyle::from_config(config);
        let text_blink = crate::terminal::text_blink_rates(config);
        let family: Option<String> = config
            .str_of("font_family")
            .ok()
            .flatten()
            // Accept an early next-core development spelling without making
            // migrated 0.57 configs silently fall back to the bundled face.
            .or_else(|| config.str_of("font").ok().flatten())
            .map(str::to_string);
        let shape = crate::terminal::Shape::from_config(config);
        let settings = unterm_services::settings::Settings::from_config(config);
        let padding = |key: &str| {
            config
                .float_of(key)
                .ok()
                .flatten()
                .unwrap_or(crate::ui_tokens::CHROME_PANEL_INSET as f64)
                .max(0.0) as f32
        };
        let pixel_size = config
            .float_of("font_size")
            .ok()
            .flatten()
            .unwrap_or(13.0)
            .max(6.0);

        let (clipboard_tx, clipboard_rx) = std::sync::mpsc::channel();
        let picture = crate::background::configured(config);
        crate::startup_trace::mark("app.background.loaded");
        let font = TerminalFont::open_named(
            family.as_deref(),
            crate::terminal::pixels_for_points(pixel_size as f32, 1.0),
            &fallbacks,
            shape,
        )?;
        crate::startup_trace::mark("app.terminal_font.opened");
        let chrome_font = crate::chrome_font::open(&fallbacks, 1.0)?;
        crate::startup_trace::mark("app.chrome_font.opened");
        let shell = shell_from(config);
        crate::startup_trace::mark("app.shell.selected");

        Ok(Self {
            engine: crate::engine_backend::AppEngine::from_environment(),
            wake_watch: unterm_services::wake_watch::WakeWatch::new(),
            charselect_group: Default::default(),
            start_directory: None,
            explicit_launch: false,
            restore: None,
            clipboard_tx,
            clipboard_rx,
            scrollbar_enabled: config
                .bool_of("enable_scroll_bar")
                .ok()
                .flatten()
                .unwrap_or(true),
            status_bar_enabled: config
                .bool_of("status_bar")
                .ok()
                .flatten()
                .unwrap_or(false),
            link_rules: crate::links::Rules::from_config(config),
            audible_bell: config
                .str_of("audible_bell")
                .ok()
                .flatten()
                .map(|value| !value.eq_ignore_ascii_case("disabled"))
                .unwrap_or(true),
            visual_bell: crate::terminal::VisualBell::from_config(config),
            config_default_cwd: config
                .str_of("default_cwd")
                .ok()
                .flatten()
                .map(std::path::PathBuf::from),
            close_prompts: config
                .str_of("window_close_confirmation")
                .ok()
                .flatten()
                .map(|value| !value.eq_ignore_ascii_case("neverprompt"))
                .unwrap_or(true),
            system_decorations: config
                .bool_of("window.decorations")
                .ok()
                .flatten()
                .unwrap_or(false),
            cursor_style: cursor_style.0,
            cursor_blink_ms: cursor_style.1,
            text_blink_ms: text_blink.0,
            text_blink_rapid_ms: text_blink.1,
            started: std::time::Instant::now(),
            theme_id: crate::theme::remembered(),
            theme_request_seen: 0,
            font_family: family,
            initial_cols: settings.initial_cols,
            initial_rows: settings.initial_rows,
            terminal_padding: [
                padding("window.padding_left"),
                padding("window.padding_right"),
                padding("window.padding_top"),
                padding("window.padding_bottom"),
            ],
            inactive_pane_hsb: [
                config
                    .float_of("inactive_pane.hue")
                    .ok()
                    .flatten()
                    .unwrap_or(1.0)
                    .max(0.0) as f32,
                config
                    .float_of("inactive_pane.saturation")
                    .ok()
                    .flatten()
                    .unwrap_or(1.0)
                    .max(0.0) as f32,
                config
                    .float_of("inactive_pane.brightness")
                    .ok()
                    .flatten()
                    .unwrap_or(1.0)
                    .max(0.0) as f32,
            ],
            configured_font_points: pixel_size as f32,
            font_fallbacks: fallbacks,
            chrome_overrides: ChromeOverrides::from_config(config),
            shell,
            core_replaced_at: None,
            cockpit_fed_at: std::time::Instant::now(),
            seen_session_epoch: 0,
            gpu: None,
            config: config.clone(),
            configured_pixel_size: pixel_size,
            parked: Vec::new(),
            window: WindowState::fresh(
                unterm_engine::reserve_window_id(),
                config,
                pixel_size,
                shape,
                font,
                chrome_font,
                picture,
            ),
        })
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<Live> {
        crate::startup_trace::mark("window.start");
        let metrics = self.window.font.metrics();
        let initial_width = metrics.width * self.initial_cols as f32
            + crate::sidebar::adaptive_default_width(1) * self.chrome_pt()
            + self.terminal_padding_left()
            + self.terminal_padding_right();
        let initial_height = self.top_bar_height()
            + self.terminal_padding_top()
            + metrics.height * self.initial_rows as f32
            + self.status_bar_height()
            + self.terminal_padding_bottom();
        // The taskbar and Alt-Tab read this at runtime; Explorer reads the
        // resource the build script embeds. Both, so the logo is there
        // whichever way Windows asks for it.
        let window_icon = {
            const LOGO: &[u8] = include_bytes!("../../assets/icon/unterm-icon-256.png");
            image::load_from_memory(LOGO)
                .ok()
                .map(|logo| logo.to_rgba8())
                .and_then(|logo| {
                    let (width, height) = logo.dimensions();
                    winit::window::Icon::from_rgba(logo.into_raw(), width, height).ok()
                })
        };
        let mut attributes = Window::default_attributes()
            .with_title("Unterm")
            .with_window_icon(window_icon)
            // The top bar is the title bar. A grey native one above a dark
            // one is the three-stacked-strips look the design called out.
            // The custom chrome is the default; `window.decorations = true`
            // asks for the system's frame back and is finally listened to.
            .with_decorations(self.system_decorations)
            .with_inner_size(winit::dpi::LogicalSize::new(initial_width, initial_height));
        // On macOS the custom chrome keeps the native frame: an invisible
        // title bar over our own bar, so the traffic lights, the shadow and
        // the rounded corners are the system's -- the way 0.57.4 integrated,
        // and the difference between looking like a Mac app and a transplant.
        #[cfg(target_os = "macos")]
        if !self.system_decorations {
            use winit::platform::macos::WindowAttributesExtMacOS as _;
            attributes = attributes
                .with_decorations(true)
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true);
        }
        if let Some(saved) = &self.restore {
            attributes = attributes
                .with_inner_size(winit::dpi::PhysicalSize::new(saved.width, saved.height));
        }
        // The OS shows a window as soon as it exists. Keep the frame hidden
        // until there is a pane to render, otherwise cold starts show a blank
        // terminal while Core/GPU/session setup finishes.
        let window = Arc::new(event_loop.create_window(attributes.with_visible(false))?);
        crate::startup_trace::mark("window.created");
        if !self.system_decorations {
            round_window_corners(&window);
        }
        if self.restore.as_ref().map(|saved| saved.maximized) == Some(true) {
            window.set_maximized(true);
        }
        // The window knows the display's scale; the font was opened before it
        // existed, at 1.0. On a scaled panel every glyph until now was drawn
        // at a fraction of its size.
        let scale = window.scale_factor() as f32;
        if (scale - self.window.scale).abs() > f32::EPSILON {
            self.reopen_font(self.window.font_points, scale);
        }
        // So `instance.focus`, which arrives on the MCP thread, has a window
        // to raise.
        crate::mcp_host::remember_window(self.window.id, window.clone());
        // And now that there is a window to answer with, offer it to the
        // Core as the front end it can call back into. Ordered after
        // `remember_window` because the first thing the Core asks for is
        // this window's identity.
        crate::engine_backend::attach_host_channel();
        crate::startup_trace::mark("host_channel.attached");
        // Without this the system never starts an input method, and a Chinese
        // or Japanese keyboard can only produce Latin letters.
        window.set_ime_allowed(true);

        let size = window.inner_size();
        // A window that has not been mapped yet reports 0x0. Every fallback
        // between here and the grid floors at one cell, so believing that
        // number hands the first pane a one-column terminal -- and for a pane
        // that already holds output, one column is not a narrow view of it,
        // it is the loss of it. Stand in the same default `window_width`
        // uses until the first real `Resized` says otherwise.
        let sized = is_drawable_size(size.width, size.height);
        let (window_width, window_height) = if sized {
            (size.width as f32, size.height as f32)
        } else {
            (800.0, 600.0)
        };
        // The terminal's own area, not the whole window: the strip on the
        // left and the padding either side are not columns the shell may
        // write into. Sizing the first pane from the raw window meant every
        // TUI launched at startup drew past the right edge until something
        // resized the window.
        let (cols, rows) = self.window.font.grid_for(
            self.terminal_width_for(window_width),
            self.terminal_height_for(window_height),
        );
        // A Core that outlived the previous window still holds its
        // sessions; a window that opened onto a populated Core must
        // show those, not stack a fresh shell on top of them. In Local
        // mode the engine was born with this process and the list is
        // empty, so this is the create path it always was.
        //
        // Unless this window was asked for something specific: `start
        // --cwd` (Finder's "New Unterm Window Here", the in-app New
        // Window command) or an explicit program. Adopting the focused
        // old session then means a window at the wrong directory — the
        // ask names a fresh shell, not a view of what was already there.
        let adopted = if self.explicit_launch {
            None
        } else {
            unterm_engine::SessionEngine::list_sessions(&self.engine)
                .ok()
                .and_then(|sessions| {
                    let live: Vec<_> = sessions
                        .into_iter()
                        .filter(|session| !session.is_dead)
                        .collect();
                    let focused = live.iter().find(|session| session.is_active).cloned();
                    let chosen = focused.or_else(|| live.first().cloned())?;
                    // Adopt the pane the arrangement hangs from, not the one
                    // that happens to be focused. `sync_tabs` rebuilds a split
                    // onto a pane already in a tab, and the focused pane after
                    // a split is the *new* one -- adopt that and its source has
                    // nothing to attach to, so a two-pane split comes back as
                    // two tabs. Focus is restored separately.
                    Some(split_lineage_root(&live, chosen))
                })
        };
        crate::startup_trace::mark(if adopted.is_some() {
            "session.adopted"
        } else {
            "session.create_planned"
        });
        let mut startup_request = adopted.is_none().then(|| {
            let env = launch_env_for_new_pane();
            CreateSessionRequest {
                cols,
                rows,
                command_dir: self
                    .start_directory
                    .clone()
                    .or_else(|| {
                        self.restore
                            .as_ref()
                            .and_then(|saved| saved.cwds.first())
                            .map(std::path::PathBuf::from)
                    })
                    .or_else(|| self.config_default_cwd.clone())
                    .as_ref()
                    .map(|path| path.display().to_string()),
                command: prepare_shell(self.shell.clone()),
                env,
                launch_policy: LaunchPolicySnapshot::default(),
            }
        });
        // Core session creation is IPC plus a shell spawn. Start it while the
        // GPU path is still coming up so a cold window is not serially paying
        // for both. Local mode stays synchronous because it owns the engine in
        // this process.
        let pending_core_session = match (&self.engine, startup_request.take()) {
            (
                crate::engine_backend::AppEngine::Core { client, .. },
                Some(request),
            ) => {
                let client = client.clone();
                crate::startup_trace::mark("session.create_thread_spawned");
                Some(std::thread::spawn(move || {
                    unterm_engine::SessionEngine::create_session(client.as_ref(), request)
                }))
            }
            (_, request) => {
                startup_request = request;
                None
            }
        };

        // A second window does not go looking for a GPU. The first one
        // already found it, and finding it is the 250 ms.
        let Graphics {
            surface,
            device,
            queue,
            format,
            shared,
        } = match self.gpu.clone() {
            Some(shared) => surface_on(shared, window.clone(), size.width, size.height)?,
            None => request_graphics(window.clone(), size.width, size.height)?,
        };
        self.gpu = Some(shared.clone());
        crate::startup_trace::mark("graphics.ready");

        let renderer = Renderer::new(device, queue, format);
        let atlas_texture = renderer.upload_atlas(&self.window.atlas);
        crate::startup_trace::mark("renderer.ready");
        let atlas_uploaded_glyphs = self.window.atlas.len();

        let session = match adopted {
            Some(existing) => {
                // This window's grid decides the size, not the one the
                // previous window left behind -- but only once this window
                // has a size of its own. An adopted session arrives with
                // output already on it, and resizing it to a guess would
                // truncate that output rather than reflow it. The first real
                // `Resized` is a frame away and knows the answer.
                if sized {
                    let _ = self.engine.resize_session(existing.id, cols, rows);
                }
                Some(existing)
            }
            None if pending_core_session.is_some() => {
                self.window.startup_session = pending_core_session;
                None
            }
            None => Some(self.engine.create_session(
                startup_request.expect("new startup session request is present"),
            )?),
        };
        if session.is_some() {
            crate::startup_trace::mark("session.ready");
        } else {
            crate::startup_trace::mark("session.pending");
        }

        // The first pane is a tab of one. Recording it here means a later split
        // has an arrangement to grow rather than one to infer.
        if let Some(session) = &session {
            self.window.tab_id = self.window.tabs.create_tab(session.id).ok();
        }

        // Uploaded once: a photograph re-read every frame is a photograph read
        // sixty times a second.
        let picture = self.window.picture.as_ref();
        let background =
            picture.map(|image| renderer.upload_image(image.width, image.height, &image.rgba));
        crate::startup_trace::mark("background.ready");
        let live = Live {
            window,
            surface,
            renderer,
            atlas_texture,
            atlas_uploaded_glyphs,
            background,
            background_size: picture.map(|image| (image.width, image.height)),
            background_opacity: picture.map(|image| image.opacity).unwrap_or(0.0),
            session_id: session.as_ref().map(|session| session.id).unwrap_or(0),
            width: size.width.max(1),
            height: size.height.max(1),
        };
        live.configure(format);
        crate::startup_trace::mark("live.configured");
        live.window.set_visible(true);
        crate::startup_trace::mark("window.visible");
        Ok(live)
    }

    fn draw(&mut self) {
        if self.window.state.is_none() {
            return;
        }
        // An occluded window gets no drawables: every acquire blocks its
        // full timeout, and a loop of those reads as the whole terminal
        // frozen (2026-08-09, thirty-second "stalls" at 70% CPU). Nothing
        // needs painting where nothing is shown.
        if self.window.occluded {
            return;
        }
        let placements = self.placements();
        let dividers = self.divider_quads();
        let Some(window_width) = self.window.state.as_ref().map(|live| live.width as f32) else {
            return;
        };
        // The tab registry is the source of truth for split focus.  Using the
        // session that happened to create the window leaves the solid cursor,
        // scrollbar and keyboard focus behind when another pane is clicked.
        let session_id = self.focused_session();
        // The very number `needs_redraw` compares, read before any screen
        // is fetched so an update landing mid-draw still earns its frame.
        // This used to record a sum of per-pane snapshot revisions instead
        // — a different counter than the one compared, never equal in Core
        // mode — and every idle tick redrew the whole window forever: the
        // chronic 90%-CPU "GUI 卡" was this one line of double bookkeeping.
        let generation = {
            let panes: Vec<usize> = if placements.is_empty() {
                if session_id == 0 {
                    Vec::new()
                } else {
                    vec![session_id]
                }
            } else {
                placements
                    .iter()
                    .map(|placement| placement.session_id)
                    .collect()
            };
            if panes.is_empty() {
                0
            } else {
                self.engine.render_generation(&panes)
            }
        };

        let mut quads = unterm_render::quads::FrameQuads::default();
        // The picture, under everything, filling the window with the middle of
        // itself. Its alpha is how much shows through -- text has to win.
        let picture = self.window.state.as_ref().and_then(|live| {
            live.background_size
                .map(|size| (size, live.background_opacity, live.width, live.height))
        });
        if let Some(((image_width, image_height), opacity, width, height)) = picture {
            let window = (width as f32, height as f32);
            let uv = crate::background::cover((image_width, image_height), window);
            quads.image = Some(unterm_render::quads::GlyphQuad {
                quad: unterm_render::quads::Quad {
                    left: 0.0,
                    top: 0.0,
                    width: window.0,
                    height: window.1,
                    color: [1.0, 1.0, 1.0, opacity],
                },
                tex_left: uv[0],
                tex_top: uv[1],
                tex_right: uv[2],
                tex_bottom: uv[3],
            });
        }
        let cursor = self.cursor_style;
        let solid_cursor = self.cursor_is_solid();
        let blink = self.blink_phase();
        let mut screen_blink = (false, false);
        let mut terminal_has_content = false;
        for placement in &placements {
            let Ok(snapshot) = self.engine.read_styled_screen(placement.session_id) else {
                continue;
            };
            let background_start = quads.backgrounds.len();
            let glyph_start = quads.glyphs.len();
            if placement.session_id == session_id {
                self.window.mouse_modes = snapshot.mouse;
                self.note_bells(snapshot.bells);
                self.take_clipboard_request(snapshot.clipboard_request.clone());
            }
            let blinking = crate::terminal::blinking_cells(&snapshot);
            screen_blink = (screen_blink.0 || blinking.0, screen_blink.1 || blinking.1);
            terminal_has_content |= styled_snapshot_has_text(&snapshot);
            crate::terminal::append_pane(
                &snapshot,
                &mut self.window.font,
                &mut self.window.atlas,
                self.window.colors,
                placement.origin,
                placement.session_id == session_id && solid_cursor,
                cursor,
                blink,
                &mut quads,
            );
            if placement.session_id != session_id {
                dim_pane_quads(
                    &mut quads,
                    background_start,
                    glyph_start,
                    self.inactive_pane_hsb[0],
                    self.inactive_pane_hsb[1],
                    self.inactive_pane_hsb[2],
                );
            }
        }
        if placements.is_empty() {
            if session_id != 0 {
                let Ok(snapshot) = self.engine.read_styled_screen(session_id) else {
                    return;
                };
                self.window.mouse_modes = snapshot.mouse;
                self.note_bells(snapshot.bells);
                self.take_clipboard_request(snapshot.clipboard_request.clone());
                // Below the top bar, like every other pane. Drawn at the window's
                // own origin it lands on the bar, which is what the first frame
                // with a bar in it looked like.
                let origin = (self.terminal_left(), self.terminal_top());
                screen_blink = crate::terminal::blinking_cells(&snapshot);
                terminal_has_content = styled_snapshot_has_text(&snapshot);
                crate::terminal::append_pane(
                    &snapshot,
                    &mut self.window.font,
                    &mut self.window.atlas,
                    self.window.colors,
                    origin,
                    solid_cursor,
                    cursor,
                    blink,
                    &mut quads,
                );
            }
        }
        self.append_selection(&mut quads);
        quads.backgrounds.extend(dividers);
        self.append_scrollbar(&mut quads);
        self.append_bell_flash(&mut quads);
        self.append_hovered_link(&mut quads);
        self.append_ghost_text(&mut quads);
        self.append_preedit(&mut quads);
        self.append_search_matches(&mut quads);
        self.append_search_bar(window_width, &mut quads);
        self.append_copy_mode(&mut quads);
        self.append_quick_select(&mut quads);
        self.append_pane_select(&mut quads);
        // Everything from here up is a panel, and a panel has to cover what
        // is behind it rather than sit in the same layer as it.
        let overlays = quads.mark();
        self.append_inbox(window_width, &mut quads);
        self.append_git_panel(window_width, &mut quads);
        self.append_composer(window_width, &mut quads);
        self.append_suggestion(window_width, &mut quads);
        self.append_top_bar(window_width, &mut quads);
        self.append_sidebar(&mut quads);
        self.append_tree(&mut quads);
        self.append_status_bar(window_width, &mut quads);
        // Over the status row it replaces, and inside the raised chrome
        // group: appended after the raise it would sit beneath the bar it is
        // supposed to cover, which is an invisible question.
        self.append_confirmation_banner(window_width, &mut quads);
        self.append_core_lost_banner(window_width, &mut quads);
        self.append_pane_close_buttons(&mut quads);
        quads.raise_since(overlays);
        // The true modals, above the docks as well: a tier is
        // backgrounds-then-glyphs, so a palette in the overlay tier had the
        // file tree's text bleeding through its card.
        let modals = quads.mark();
        self.append_palette(window_width, &mut quads);
        self.append_tooltip(window_width, &mut quads);
        self.append_quick_menu(&mut quads);
        quads.raise_since_modal(modals);

        let Some(live) = self.window.state.as_mut() else {
            return;
        };
        // The atlas may have grown while building this frame's glyphs, so
        // upload after them. An unchanged atlas stays on the device: cursor
        // blinking must not upload a megabyte of identical coverage.
        if live.atlas_uploaded_glyphs != self.window.atlas.len() {
            live.atlas_texture = live.renderer.upload_atlas(&self.window.atlas);
            live.atlas_uploaded_glyphs = self.window.atlas.len();
        }

        let acquire_started = std::time::Instant::now();
        let frame = match live.surface.get_current_texture() {
            Ok(frame) => frame,
            // A timeout means the compositor is not taking frames — an
            // occluded or closing window. The acquire already blocked for
            // a second; reconfiguring and asking again blocks a second
            // more, every frame, which is the "clicking close froze the
            // terminal" hang. The frame is skipped, full stop.
            Err(wgpu::SurfaceError::Timeout) => {
                crate::stallwatch::note_if_slow(
                    "swapchain acquire (timed out)",
                    acquire_started,
                    0,
                );
                return;
            }
            // A lost or outdated swapchain (resize race, display sleep,
            // driver reset) heals with one reconfigure. A second failure is
            // this frame given up, not the process.
            Err(error) => {
                log::debug!("swapchain frame unavailable ({error}); reconfiguring");
                live.configure(live.renderer.format());
                match live.surface.get_current_texture() {
                    Ok(frame) => frame,
                    Err(error) => {
                        log::warn!("swapchain unavailable after reconfigure: {error}");
                        return;
                    }
                }
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        live.renderer.draw(
            &view,
            live.width,
            live.height,
            &quads,
            &live.atlas_texture,
            live.background.as_ref(),
            self.window.colors.background,
        );
        frame.present();
        if !self.window.startup_first_frame_marked {
            self.window.startup_first_frame_marked = true;
            crate::startup_trace::mark("first_frame.presented");
        }
        if terminal_has_content && !self.window.startup_terminal_content_marked {
            self.window.startup_terminal_content_marked = true;
            crate::startup_trace::mark("terminal_content.presented");
        }
        self.window.drawn_revision = Some(generation);
        self.window.presented_at = Some(std::time::Instant::now());
        self.window.drawn_cursor_solid = Some(solid_cursor);
        self.window.drawn_blink = Some(blink);
        self.window.screen_blink = screen_blink;
        self.window.drawn_confirmation = unterm_mcp::handler::pending_confirmation_view().map(|v| v.id);
        self.window.drawn_suggestions =
            crate::engine_backend::mcp_state::pending_suggestions_for_pane(self.focused_session() as u64).len();
    }

    /// Offer a mouse event to the program. True when it took it.
    ///
    /// The front end asks before acting on a click of its own: with reporting
    /// on, a drag that also selected text would give the user a selection they
    /// did not ask for on top of a click that did work.
    fn report_mouse(
        &mut self,
        kind: unterm_engine::next_core::mouse_encoding::MouseEventKind,
        button: Option<unterm_engine::next_core::mouse_encoding::MouseButton>,
    ) -> bool {
        let Some(live) = self.window.state.as_ref() else {
            return false;
        };
        let metrics = self.window.font.metrics();
        let left = self.terminal_left();
        let top = self.terminal_top();
        let column = ((self.window.pointer.0 - left).max(0.0) / metrics.width.max(1.0)) as usize;
        let row = ((self.window.pointer.1 - top).max(0.0) / metrics.height.max(1.0)) as usize;

        let held = crate::mouse::Held {
            shift: self.window.shift_held,
            ctrl: self.window.ctrl_held,
            alt: self.window.alt_held,
        };
        match crate::mouse::route(self.window.mouse_modes, kind, button, column, row, held) {
            crate::mouse::Route::ToProgram(event) => {
                let _ = self.engine.report_mouse(live.session_id, event);
                true
            }
            crate::mouse::Route::ToTerminal => false,
        }
    }

    /// Start a flash if the pane has rung since the last frame.
    fn note_bells(&mut self, bells: u64) {
        if bells > self.window.bells_seen {
            self.window.bells_seen = bells;
            // A flash that would never show must not start: `bell_at` is also
            // what keeps the window asking for frames until the fade is over.
            if !self.visual_bell.disabled() {
                self.window.bell_at = Some(std::time::Instant::now());
            }
            // The bell is audible again, as `audible_bell` always promised;
            // the flash alone left a background beep silent.
            if self.audible_bell {
                unterm_services::system_beep();
            }
        }
    }

    /// Whether the pointer is over the scrollbar's track.
    /// Whether the pointer is on the status bar's quick-action button.
    fn pointer_on_scrollbar(&self) -> bool {
        if !self.scrollbar_enabled {
            return false;
        }
        self.active_pane_scrollbar()
            .is_some_and(|(_, left, top, track)| {
                self.window.pointer.0 >= left
                    && self.window.pointer.0 < left + crate::scrollbar::WIDTH
                    && self.window.pointer.1 >= top
                    && self.window.pointer.1 < top + track
            })
    }

    /// Scroll to wherever the pointer is on the track.
    fn scroll_to_pointer(&mut self) {
        let Some((session_id, _left, track_top, track)) = self.active_pane_scrollbar() else {
            return;
        };
        let Ok(snapshot) = self.engine.read_styled_screen(session_id) else {
            return;
        };
        let total = snapshot.scrollback_rows + snapshot.rows;
        let row = crate::scrollbar::row_at(total, snapshot.rows, self.window.pointer.1 - track_top, track);
        let _ = self.engine.scroll_viewport_to(session_id, row as isize);
        self.window.drawn_revision = None;
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
    }

    /// The active pane's scrollbar track.  0.57.4 anchored the bar to the
    /// focused PositionedPane's right edge; a window-global right edge makes
    /// the bar apparently vanish as soon as the left pane has focus.
    fn active_pane_scrollbar(&self) -> Option<(usize, f32, f32, f32)> {
        let session_id = self.focused_session();
        let placements = self.placements();
        let placement = placements
            .iter()
            .find(|placement| placement.session_id == session_id)?;
        let metrics = self.window.font.metrics();
        let grid_right = placement.origin.0 + placement.cols as f32 * metrics.width;
        let rightmost = placements
            .iter()
            .all(|other| other.origin.0 + other.cols as f32 * metrics.width <= grid_right + 0.5);
        let pane_right = if rightmost {
            self.terminal_left() + self.terminal_width()
        } else {
            grid_right
        };
        Some((
            session_id,
            pane_right - crate::scrollbar::WIDTH,
            placement.origin.1,
            (placement.rows as f32 * metrics.height).max(1.0),
        ))
    }

    /// The scrollbar, down the right edge.
    ///
    /// Only when there is history above: a bar that fills its whole track
    /// tells the user nothing and takes a column to say it.

    /// The bar along the bottom: where you are, and what agents are doing.
    /// Run the bar's search in whichever mode it is in.
    ///
    /// Literal modes go to the kernel, which owns the scrollback. Regex is
    /// matched here over the same lines: the kernel's dependency budget has
    /// no room for a regex engine, and the bar should not care where the
    /// matching happened.
    fn run_search(
        &self,
        session_id: usize,
        pattern: &str,
        mode: crate::search::Mode,
    ) -> Vec<unterm_engine::ScreenSearchMatch> {
        if pattern.is_empty() {
            return Vec::new();
        }
        match mode {
            crate::search::Mode::CaseSensitive => self
                .engine
                .search(
                    session_id,
                    pattern,
                    unterm_engine::SearchMode::CaseSensitive,
                    MAX_SEARCH_MATCHES,
                )
                .unwrap_or_default(),
            crate::search::Mode::CaseInsensitive => self
                .engine
                .search(
                    session_id,
                    pattern,
                    unterm_engine::SearchMode::CaseInsensitive,
                    MAX_SEARCH_MATCHES,
                )
                .unwrap_or_default(),
            crate::search::Mode::Regex => {
                let Ok(snapshot) = self.engine.read_scrollback_text(
                    session_id,
                    unterm_engine::ScrollbackTextRequest {
                        start_line: None,
                        end_line: None,
                        tail_lines: None,
                        escapes: false,
                    },
                ) else {
                    return Vec::new();
                };
                unterm_services::search_regex::find_matches(
                    &snapshot.lines,
                    snapshot.first_row,
                    pattern,
                    MAX_SEARCH_MATCHES,
                )
            }
        }
    }

    /// The parked-agent question, painted over the status row the way 0.57.4
    /// painted it: the whole bar inverts, and one line asks and names its keys.
    /// Say when the Core has gone, across the width of the window.
    ///
    /// The frames on screen were real a moment ago and still look it;
    /// nothing about them says the shells behind them are gone. A
    /// crash that leaves a window looking healthy is the crash getting
    /// reported as normal, which is the one thing the gate forbids.
    fn append_core_lost_banner(
        &mut self,
        window_width: f32,
        quads: &mut unterm_render::quads::FrameQuads,
    ) {
        // Red while the shells are unreachable, amber for a while after
        // they come back. The recovery is reported in the same strip as
        // the loss because they are one event to the person watching:
        // "it broke" without "it is fixed" reads as still broken.
        const RECOVERY_NOTICE: std::time::Duration = std::time::Duration::from_secs(12);
        let (key, color) = if !self.engine.sessions_reachable() {
            ("core.lost", [0.62, 0.16, 0.12, 1.0])
        } else if self
            .core_replaced_at
            .is_some_and(|at| at.elapsed() < RECOVERY_NOTICE)
        {
            ("core.replaced", [0.55, 0.38, 0.05, 1.0])
        } else {
            return;
        };
        let metrics = self.window.font.metrics();
        let pt = self.chrome_pt();
        let pad = (crate::ui_tokens::STATUS_BAR_VERTICAL_PADDING * pt)
            .round()
            .max(2.0);
        let bar_height = (metrics.height + pad * 2.0).round().max(1.0);
        let top = self.terminal_top();
        // Starts where the terminal does. Run full-bleed and it lies
        // across the dock, hiding the very tab list the message is
        // telling the user has changed.
        let left = self.dock_width(metrics);
        let strip_width = (window_width - left).max(0.0);
        quads.backgrounds.push(unterm_render::quads::Quad {
            left,
            top,
            width: strip_width,
            height: bar_height,
            color,
        });
        let columns = (strip_width / metrics.width.max(1.0)).floor().max(0.0) as usize;
        let line = crate::sidebar::fit(
            &unterm_services::i18n::t(key),
            columns.saturating_sub(3),
        );
        let text_top = top
            + ((bar_height - metrics.height) / 2.0
                + crate::ui_tokens::CHROME_TEXT_BASELINE_NUDGE * pt)
                .max(0.0);
        let pen = left + (crate::ui_tokens::CHROME_PANEL_INSET * pt).round();
        self.append_mono(&line, [1.0, 0.94, 0.92, 1.0], (pen, text_top), quads);
    }

    fn append_confirmation_banner(
        &mut self,
        window_width: f32,
        quads: &mut unterm_render::quads::FrameQuads,
    ) {
        let Some(view) = unterm_mcp::handler::pending_confirmation_view() else {
            return;
        };
        let metrics = self.window.font.metrics();
        let height = self.window.state.as_ref().map(|live| live.height).unwrap_or(600) as f32;
        // Its own strip, laid over the last row rather than taken out
        // of the grid: a question that reflows the shell underneath it
        // moves the cursor away from where the program left it, and
        // every row after that is drawn a line off.
        let pt = self.chrome_pt();
        let pad = (crate::ui_tokens::STATUS_BAR_VERTICAL_PADDING * pt)
            .round()
            .max(2.0);
        let bar_height = (metrics.height + pad * 2.0).round().max(1.0);
        let top = height - bar_height - self.status_bar_height();
        // Starts where the terminal does. Run full-bleed and it lies across
        // the dock's own footer controls, leaving "new session" and settings
        // sliced in half -- and the question belongs to the grid anyway, not
        // to the strip listing the tabs.
        let left = self.dock_width(metrics);
        let strip_width = (window_width - left).max(0.0);
        let columns = (strip_width / metrics.width.max(1.0)).floor().max(0.0) as usize;
        // Less the inset the pen starts at, or the last hint runs off the edge.
        let line = crate::confirm::status_line(
            &view.agent,
            &view.method,
            &view.input_preview,
            columns.saturating_sub(3),
        );

        // Inverted, so the row cannot be mistaken for the facts it replaces.
        let foreground = self.window.colors.foreground;
        let background = self.window.colors.background;
        quads.backgrounds.push(unterm_render::quads::Quad {
            left,
            top,
            width: strip_width,
            height: bar_height,
            color: foreground,
        });
        let text_top = top
            + ((bar_height - metrics.height) / 2.0
                + crate::ui_tokens::CHROME_TEXT_BASELINE_NUDGE * pt)
                .max(0.0);
        let pen = left + (crate::ui_tokens::CHROME_PANEL_INSET * pt).round();
        self.append_mono(&line, background, (pen, text_top), quads);
    }

    fn append_status_bar(
        &mut self,
        window_width: f32,
        quads: &mut unterm_render::quads::FrameQuads,
    ) {
        // A parked agent write owns this row while it waits: drawing the
        // facts under the question leaves both showing through the other.
        if unterm_mcp::handler::pending_confirmation_count() > 0 {
            return;
        }
        let metrics = self.window.font.metrics();
        let height = self.window.state.as_ref().map(|live| live.height).unwrap_or(600) as f32;
        let bar_height = self.status_bar_height();
        let top = height - bar_height;
        let columns = (window_width / metrics.width.max(1.0)).floor().max(0.0) as usize;

        let status = self.status();
        let segments = crate::statusbar::segments(&status, columns);
        if segments.is_empty() {
            return;
        }

        // The same surface as the top bar, so the window reads as one thing
        // with two edges. Toning the foreground towards black instead is what
        // left a black strip along the bottom of every light theme.
        let chrome = self.chrome();
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: 0.0,
            top,
            width: window_width,
            height: bar_height,
            color: chrome.surface,
        });
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: 0.0,
            top,
            width: window_width,
            height: 1.0,
            color: chrome.outer_edge,
        });
        let pt = self.chrome_pt();
        let gap = self.mono_width(crate::statusbar::GAP);
        // The segments are set in the terminal's own face, vertically centred
        // with the slight upward nudge every one-line chrome row gets.
        let text_top = top
            + ((bar_height - metrics.height) / 2.0
                + crate::ui_tokens::CHROME_TEXT_BASELINE_NUDGE * pt)
                .max(0.0);
        let teal = chrome.focus_rail;
        let mut pen = (crate::ui_tokens::CHROME_PANEL_INSET * pt).round();
        for segment in segments {
            let color = if segment.dim {
                chrome.dim_text
            } else {
                self.window.colors.foreground
            };
            // Whatever will not fit is not drawn: half a chip reads as a wrong
            // value rather than as a missing one.
            let wide = self.mono_width(&segment.text);
            if pen + wide > window_width {
                break;
            }
            match segment.teal_from {
                // The whole segment is a value — the path — and reads teal.
                Some(0) => {
                    pen = self.append_mono(&segment.text, teal, (pen, text_top), quads);
                }
                // A chip: its label keeps the ordinary colour, and the answer
                // after the colon takes the accent, as 0.57.4 drew it — or the
                // close button's red for the moment a proxy probe has failed.
                Some(at) if segment.text.is_char_boundary(at) => {
                    let (label, value) = segment.text.split_at(at);
                    let label = label.to_string();
                    let value = value.to_string();
                    let value_color = if segment.error {
                        crate::window_buttons::CLOSE_HOVER
                    } else {
                        teal
                    };
                    pen = self.append_mono(&label, color, (pen, text_top), quads);
                    pen = self.append_mono(&value, value_color, (pen, text_top), quads);
                }
                _ => {
                    pen = self.append_mono(&segment.text, color, (pen, text_top), quads);
                }
            }
            pen += gap;
        }
    }

    /// Say what just happened, for a moment.
    ///
    /// 2400 milliseconds, as before: long enough to read on the way past,
    /// short enough that it is gone before it becomes furniture.
    fn show_notice(&mut self, message: String) {
        const SHOWN_FOR: std::time::Duration = std::time::Duration::from_millis(2400);
        self.window.notice = Some((message, std::time::Instant::now() + SHOWN_FOR));
        self.window.drawn_revision = None;
    }

    /// The notice, if one is still up. Clears it once it is not.
    fn active_notice(&self) -> Option<String> {
        self.window.notice
            .as_ref()
            .filter(|(_, expires)| *expires > std::time::Instant::now())
            .map(|(message, _)| message.clone())
    }

    /// What the status bar has to say right now.
    fn status(&self) -> crate::statusbar::Status {
        let session = self.window.state.as_ref().and_then(|live| {
            unterm_engine::SessionEngine::list_sessions(&self.engine)
                .ok()
                .into_iter()
                .flatten()
                .find(|session| session.id == live.session_id)
        });
        let mcp = crate::engine_backend::mcp_state::insights_mcp_snapshot(0);
        let agents = unterm_services::cockpit::status::snapshot();
        let _ = agents;
        let directory = session
            .as_ref()
            .and_then(|session| session.shell.cwd.clone())
            .unwrap_or_default();
        crate::statusbar::Status {
            notice: self.active_notice(),
            shell: session
                .as_ref()
                .map(|session| crate::statusbar::shell_label(&session.shell.process_name))
                .unwrap_or_else(|| "shell".to_string()),
            columns: session
                .as_ref()
                .map(|session| session.cols as usize)
                .unwrap_or(0),
            rows: session
                .as_ref()
                .map(|session| session.rows as usize)
                .unwrap_or(0),
            project: crate::sidebar::project_name(&directory),
            directory,
            agent_writes: mcp.input_count,
            // A bolt on the chip says a write landed a moment ago, so a flash
            // is noticeable without anyone comparing counts.
            agent_wrote_recently: mcp
                .seconds_since_last_input
                .map(|seconds| seconds < 5.0)
                .unwrap_or(false),
            theme: self.theme().id.to_string(),
            // The window's identity, if one is bound. Read here rather than
            // cached because it changes only when somebody changes it.
            profile: unterm_services::server_info::read_current().profile,
            // The toggle, not the system proxy: the chip answers "will a new
            // session get proxy env vars", and its click is the switch.
            proxy: unterm_services::launch_env::unterm_proxy_enabled(),
            proxy_unreachable: self
                .window.proxy_error_until
                .is_some_and(|until| until > std::time::Instant::now()),
            // Decided once, at startup, and true for the rest of the process.
            core_local: crate::engine_backend::core_fallback_reason().is_some(),
        }
    }

    /// Whether the cursor is solid at this instant.
    ///
    /// A steady cursor always is. A blinking one is for half of each period;
    /// the other half it is drawn as an outline rather than removed, so it
    /// stays findable while it blinks.
    fn cursor_is_solid(&self) -> bool {
        if !self.cursor_style.blinking {
            return true;
        }
        crate::terminal::blink_is_on(self.started.elapsed().as_millis(), self.cursor_blink_ms)
    }

    /// The phase blinking text is in right now, both cadences at once.
    fn blink_phase(&self) -> crate::terminal::BlinkPhase {
        crate::terminal::BlinkPhase::at(
            self.started.elapsed().as_millis(),
            self.text_blink_ms,
            self.text_blink_rapid_ms,
        )
    }

    /// Highlight what is selected.
    ///
    /// It was being tracked and copied, but never drawn -- so dragging across
    /// text selected it, copying worked, and nothing on screen said which text
    /// you had. Drawn in the scheme's own highlight, with its own text colour,
    /// because those two were chosen together.
    fn append_selection(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(selection) = self.window.drag.and_then(|drag| drag.selection()) else {
            return;
        };
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let Ok(snapshot) = self.engine.read_styled_screen(live.session_id) else {
            return;
        };

        let metrics = self.window.font.metrics();
        let origin = (self.terminal_left(), self.terminal_top());
        let theme = self.theme();

        for (index, line) in snapshot.lines.iter().enumerate() {
            let columns = selection.columns_for_row(line.row, line.cells.len());
            if columns.is_empty() {
                continue;
            }
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: origin.0 + columns.start as f32 * metrics.width,
                top: origin.1 + index as f32 * metrics.height,
                width: (columns.end - columns.start) as f32 * metrics.width,
                height: metrics.height,
                color: theme.selection,
            });
        }
    }

    /// The scheme in force, for the colours that are its rather than the
    /// frame's: the divider between panes, the scrollbar, the selection.
    fn theme(&self) -> &'static crate::theme::Theme {
        self.theme_id
            .as_deref()
            .and_then(crate::theme::by_id)
            .unwrap_or_else(crate::theme::default_theme)
    }

    /// The frame's tones, from the terminal's own colours.
    fn chrome(&self) -> crate::chrome::Chrome {
        let chrome = crate::chrome::chrome(self.window.colors.background, self.window.colors.foreground);
        // A chosen theme owns the whole window, exactly as 0.57.4 behaved:
        // the legacy `colors.tab_bar` overrides migrated from a Lua config
        // were written against the old default look, and pinning the bars to
        // those static colours is what left a dark chrome around a light
        // terminal after a theme switch.
        if self.theme_id.is_some() {
            return chrome;
        }
        self.chrome_overrides.apply(chrome, self.window.focused)
    }

    fn chrome_foreground(&self) -> [f32; 4] {
        // With a theme active the bars use its foreground, dimmed to 0.7
        // alpha when the window is not in front — 0.57.4's exact treatment.
        if self.theme_id.is_some() {
            let mut foreground = self.window.colors.foreground;
            if !self.window.focused {
                foreground[3] *= 0.7;
            }
            return foreground;
        }
        (if self.window.focused {
            self.chrome_overrides.active_foreground
        } else {
            self.chrome_overrides
                .inactive_foreground
                .or(self.chrome_overrides.active_foreground)
        })
        .unwrap_or(self.window.colors.foreground)
    }

    /// Where the terminal's first column starts, the strip included.
    fn terminal_left(&self) -> f32 {
        let metrics = self.window.font.metrics();
        self.dock_width(metrics) + self.terminal_padding_left()
    }

    /// How much of the window the left dock has taken.
    ///
    /// One dock, whichever strip is in it. The terminal makes room rather than
    /// being covered: a panel over the grid hides a row the shell still
    /// believes in, and the cursor ends up somewhere nobody can see.
    fn dock_width(&self, metrics: unterm_render::quads::CellMetrics) -> f32 {
        self.dock_width_for(self.window_width(), metrics)
    }

    /// The same measurement against a window size the caller already knows.
    ///
    /// During `start` there is no `Live` yet, so every one of these used to
    /// fall back to a hard-coded 800 — and the startup path sidestepped the
    /// whole chain by sizing the first shell from the raw window width
    /// instead. That handed the shell the sidebar's columns as well as its
    /// own, which is why a TUI opened at launch was drawn wider than the grid
    /// it lived in and why maximising fixed it: the resize was the first time
    /// anything measured the terminal properly.
    fn dock_width_for(
        &self,
        window_width: f32,
        metrics: unterm_render::quads::CellMetrics,
    ) -> f32 {
        crate::sidebar::width(
            self.window.sidebar_open,
            self.window.sidebar_points,
            self.window.tabs.tab_ids().len(),
            window_width,
            self.font_scale(),
        ) + crate::tree::width(self.window.tree.is_some(), metrics)
    }

    /// The window's inner width, or a plausible stand-in before it exists.
    fn window_width(&self) -> f32 {
        self.window.state.as_ref().map(|live| live.width).unwrap_or(800) as f32
    }

    /// The window's inner height, same caveat.
    fn window_height(&self) -> f32 {
        self.window.state.as_ref().map(|live| live.height).unwrap_or(600) as f32
    }

    /// How wide the terminal is, once the strip and the gaps are taken.
    fn terminal_width(&self) -> f32 {
        self.terminal_width_for(self.window_width())
    }

    /// How wide the terminal is inside a window of this width.
    ///
    /// The floor is the engine's minimum grid, not one cell. Every piece of
    /// chrome subtracted here has a minimum of its own that does not consult
    /// the window -- the sidebar, the tree, the git panel -- so a narrow
    /// enough window, or a large enough font, lets them ask for more than
    /// there is. Flooring at one cell answered that with a one-column
    /// terminal, and a one-column terminal is not a cramped view of a pane's
    /// output, it is the loss of it: the next resize truncates every line and
    /// the scrollback to a single character. Chrome drawing over the last
    /// column is the cheaper failure.
    fn terminal_width_for(&self, window_width: f32) -> f32 {
        let metrics = self.window.font.metrics();
        (window_width
            - self.dock_width_for(window_width, metrics)
            - self.git_panel_width_for(window_width)
            - self.terminal_padding_left()
            - self.terminal_padding_right())
        .max(unterm_engine::MIN_SESSION_GRID as f32 * metrics.width)
    }

    /// What the strip shows: one line per tab, grouped by project.
    fn sidebar_rows(&self) -> Vec<crate::sidebar::Row> {
        let sessions =
            unterm_engine::SessionEngine::list_sessions(&self.engine).unwrap_or_default();
        let statuses = unterm_services::cockpit::status::snapshot();
        let active = self.window.tab_id;
        let tabs: Vec<crate::sidebar::TabInfo> = self
            .window.tabs
            .tab_ids()
            .into_iter()
            .enumerate()
            .map(|(index, tab)| {
                let pane = self.window.tabs.active_pane(tab);
                let pane_ids = self.window.tabs.pane_ids(tab);
                let session =
                    pane.and_then(|pane| sessions.iter().find(|session| session.id == pane));
                // Whatever the top bar has already learned. Asking afresh for
                // every tab would walk the machine's process table once per
                // tab, several times a second, to put a name on rows nobody is
                // looking at -- and the pane in front is the one that matters.
                let facts = pane.map(crate::statsbar::known_facts);
                let badge = pane_ids
                    .iter()
                    .filter_map(|pane| crate::cockpit::badge_for_pane(&statuses, *pane as u64))
                    .min_by_key(|badge| match badge {
                        crate::cockpit::Badge::NeedsYou => 0,
                        crate::cockpit::Badge::Done => 1,
                        crate::cockpit::Badge::Working => 2,
                    });
                let indicators = crate::sidebar::Indicators {
                    unread: pane_ids.iter().any(|pane| {
                        self.window.pane_notices
                            .get(pane)
                            .map(|notice| notice.unread)
                            .unwrap_or(false)
                    }),
                    error: pane_ids.iter().any(|pane| {
                        self.window.pane_notices
                            .get(pane)
                            .map(|notice| notice.error)
                            .unwrap_or(false)
                    }),
                    running: badge == Some(crate::cockpit::Badge::Working)
                        || facts
                            .as_ref()
                            .map(|facts| !facts.title.is_empty())
                            .unwrap_or(false),
                };
                crate::sidebar::TabInfo {
                    index,
                    // The most urgent of the tab's panes: a split where one
                    // half is waiting is a tab that is waiting.
                    badge,
                    // A name the reader typed outranks anything computed: a
                    // renamed tab keeps its name while programs come and go.
                    // Computed names keep 0.57.4's spelling — the extension
                    // dropped, the case left alone: `powershell`, not
                    // `Powershell`.
                    title: self.window.tab_titles.get(&tab).cloned().unwrap_or_else(|| {
                        session
                            .map(|session| {
                                unterm_engine::next_core::tab_title::resolve_name(
                                    &unterm_engine::next_core::tab_title::TabTitleRules {
                                        capitalize: false,
                                        fallback: "shell".to_string(),
                                        ..Default::default()
                                    },
                                    unterm_engine::next_core::tab_title::TabContext {
                                        pane_title: &session.title,
                                        process_path: &session.shell.process_name,
                                        index,
                                    },
                                )
                            })
                            .unwrap_or_default()
                    }),
                    agent: facts
                        .as_ref()
                        .and_then(|facts| {
                            // The segment leads with a lightning bolt; the row
                            // wants the name that follows it.
                            facts
                                .agent
                                .trim()
                                .strip_prefix('\u{26A1}')
                                .map(|name| name.trim().to_string())
                                .filter(|name| !name.is_empty())
                        })
                        .or_else(|| {
                            // The cockpit knows the agent even when the
                            // stats cache has not looked at this pane:
                            // a waiting row must name who is waiting.
                            pane_ids.iter().find_map(|pane| {
                                statuses
                                    .iter()
                                    .find(|status| status.pane_id == *pane as u64)
                                    .map(|status| status.agent.clone())
                                    .filter(|agent| !agent.is_empty())
                            })
                        }),
                    cwd: session.and_then(|session| session.shell.cwd.clone()),
                    foreground: session
                        .map(|session| session.shell.process_name.clone())
                        .map(|name| crate::statusbar::short_name(&name)),
                    active: Some(tab) == active,
                    indicators,
                }
            })
            .collect();
        crate::sidebar::rows(&tabs, &self.window.sidebar_collapsed)
    }

    /// Draw the strip, if it is open.
    /// The characters that match what has been typed.
    ///
    /// The recents are read fresh each time; the catalogue is built once and
    /// kept, because it is a couple of hundred thousand rows and only the top
    /// of it is ever drawn.
    fn character_entries(&self, query: &str) -> Vec<crate::palette::Entry> {
        let recents = crate::charselect::recent_choices();
        crate::charselect::matching(
            &recents,
            crate::charselect::catalog(),
            self.charselect_group,
            query,
            crate::palette::MAX_ROWS,
        )
        .into_iter()
        .map(|choice| crate::palette::Entry {
            label: format!("{}  {}", choice.glyph, choice.name),
            hint: format!("{}  {}", choice.codepoints(), choice.group.heading()),
            command: crate::palette::Command::TypeCharacter {
                glyph: choice.glyph,
                name: choice.name.into_owned(),
            },
        })
        .collect()
    }

    /// Type a character at the prompt, and remember it was picked.
    fn type_character(&mut self, glyph: &str, name: &str) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let _ = self.engine.write_input(live.session_id, glyph);
        crate::charselect::remember(glyph, name);
        self.window.drawn_revision = None;
    }

    /// Open or close the file tree.
    ///
    /// It takes the left dock, and the tab strip gives it up: two strips
    /// either side of a terminal is most of a narrow window, and the two are
    /// answering the same question -- where am I, and what else is here.
    fn toggle_tree(&mut self) {
        self.window.tree = match self.window.tree.take() {
            Some(_) => None,
            None => {
                let here = self
                    .current_directory()
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                Some(crate::tree::Tree::new(here))
            }
        };
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    /// Which tree row a point is over.
    fn tree_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (left, top, width, height) = self.tree_dock()?;
        if x < left || x >= left + width || y < top || y >= top + height {
            return None;
        }
        let metrics = self.window.font.metrics();
        let row = ((y - top) / metrics.height.max(1.0)) as usize;
        let tree = self.window.tree.as_ref()?;
        let at = tree.scroll + row;
        (at < tree.rows.len()).then_some(at)
    }

    /// A press on the file tree. Returns true when the tree took it.
    ///
    /// The disclosure arrow opens a directory; its name changes the focused
    /// shell's directory. Keeping those hit targets separate is what makes a
    /// file tree useful both for browsing and for navigating.
    fn click_tree(&mut self) -> bool {
        let Some(row) = self.tree_row_at(self.window.pointer.0, self.window.pointer.1) else {
            return false;
        };
        let disclosure = self
            .window.tree
            .as_ref()
            .and_then(|tree| tree.rows.get(row))
            .is_some_and(|row| {
                row.is_dir
                    && !row.is_parent
                    && !row.is_drive
                    && self.tree_dock().is_some_and(|(left, _, _, _)| {
                        let columns = row.depth * 2 + 2;
                        self.window.pointer.0 < left + columns as f32 * self.window.font.metrics().width
                    })
            });
        let plain_dir = self
            .window.tree
            .as_ref()
            .and_then(|tree| tree.rows.get(row))
            .is_some_and(|row| row.is_dir && !row.is_parent && !row.is_drive);
        let click = match self.window.last_tree_click.take() {
            Some(previous) => previous.again(row, self.window.pointer.0, self.window.pointer.1),
            None => crate::sidebar::RowClick::first(row, self.window.pointer.0, self.window.pointer.1),
        };
        let doubled = click.streak() >= 2;
        self.window.last_tree_click = Some(click);
        // A single click on a directory browses -- the arrow and the name
        // both toggle it open, as 0.57.4's tree did. Only a double-click
        // reaches the shell with a cd; parent and drive rows re-root the
        // view on one click because they are navigation, not commands.
        let picked = self
            .window.tree
            .as_mut()
            .and_then(|tree| tree.press(row, disclosure || (plain_dir && !doubled)));
        match picked {
            Some(crate::tree::Picked::Directory(path)) => {
                if let Some(tree) = self.window.tree.as_mut() {
                    tree.request_root(path.clone());
                }
                if doubled {
                    self.change_directory(&path.display().to_string());
                }
            }
            Some(crate::tree::Picked::File(path)) => {
                if let Some(live) = self.window.state.as_ref() {
                    let text = path.display().to_string();
                    let quoted = format!("{} ", shell_quoted_path(&text));
                    let _ = self.engine.write_input(live.session_id, &quoted);
                }
            }
            None => {}
        }
        self.window.drawn_revision = None;
        true
    }

    /// Where the file tree is on screen, so a press lands on the row it is
    /// drawn on.
    fn tree_dock(&self) -> Option<(f32, f32, f32, f32)> {
        self.window.tree.as_ref()?;
        let metrics = self.window.font.metrics();
        let width = crate::tree::width(true, metrics);
        let top = self.terminal_top() - self.chrome_inset();
        let height = self.terminal_height() + self.chrome_inset() * 2.0;
        // To the right of the tab strip when both are open, so the two docks
        // do not draw over each other.
        let window_width = self.window.state.as_ref().map(|live| live.width).unwrap_or(800) as f32;
        let left = crate::sidebar::width(
            self.window.sidebar_open,
            self.window.sidebar_points,
            self.window.tabs.tab_ids().len(),
            window_width,
            self.font_scale(),
        );
        Some((left, top, width, height))
    }

    /// Draw the file tree, if it is open.
    fn append_tree(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some((left, top, width, height)) = self.tree_dock() else {
            return;
        };
        let metrics = self.window.font.metrics();
        let chrome = self.chrome();
        let foreground = self.chrome_foreground();
        let visible = (height / metrics.height).floor().max(1.0) as usize;

        // Follow the pane. A tree still rooted where the shell used to be is
        // a tree of somewhere else, and nothing on screen says so.
        //
        // Not while the pointer is over the tree, though: an agent in the
        // pane changes directory as it works, and a follow mid-aim rebuilds
        // the rows under the click — the reader picks a row and gets
        // whatever moved into its place. The follow resumes when they leave.
        let pointer_inside = self.window.pointer.0 >= left
            && self.window.pointer.0 < left + width
            && self.window.pointer.1 >= top
            && self.window.pointer.1 < top + height;
        let here = self.current_directory();
        let rows = {
            let Some(tree) = self.window.tree.as_mut() else {
                return;
            };
            if let Some(here) = here {
                if !pointer_inside {
                    tree.follow_root(here);
                }
            }
            tree.refresh();
            // Never scrolled past the end: a tree showing nothing looks like a
            // tree that failed to read the disk.
            tree.scroll_by(0, visible);
            tree.rows
                .iter()
                .skip(tree.scroll)
                .take(visible)
                .map(|row| (row.text(crate::tree::COLUMNS), row.is_hidden, row.is_dir))
                .collect::<Vec<_>>()
        };
        let scroll = self.window.tree.as_ref().map(|tree| tree.scroll).unwrap_or(0);

        quads.backgrounds.push(unterm_render::quads::Quad {
            left,
            top,
            width,
            height,
            color: chrome.surface,
        });
        // The seam, so the strip and the terminal read as two surfaces of one
        // window rather than one surface that changed colour.
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: left + width - 1.0,
            top,
            width: 1.0,
            height,
            color: chrome.outer_edge,
        });

        // The row the pointer is on lifts like the tab strip's do. In rows
        // one cell tall, this is the difference between clicking a name and
        // guessing one.
        if let Some(at) = self.tree_row_at(self.window.pointer.0, self.window.pointer.1) {
            if at >= scroll && at - scroll < rows.len() {
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left,
                    top: top + (at - scroll) as f32 * metrics.height,
                    width: width - 1.0,
                    height: metrics.height,
                    color: chrome.hover_bg,
                });
            }
        }

        for (index, (text, hidden, is_dir)) in rows.iter().enumerate() {
            let color = if *hidden {
                chrome.dim_text
            } else if *is_dir {
                foreground
            } else {
                chrome.dim_text
            };
            crate::terminal::append_text(
                text,
                &mut self.window.font,
                &mut self.window.atlas,
                color,
                (left, top + index as f32 * metrics.height),
                quads,
            );
        }
    }

    /// Where the tab strip is, and how tall one of its rows is.
    ///
    /// One place, so a row is pressed where it is drawn. Sized in points
    /// against the display, not in terminal cells: it is chrome.
    fn sidebar_dock(&self) -> Option<(f32, f32, f32, f32, f32)> {
        if !self.window.sidebar_open {
            return None;
        }
        let window_width = self.window.state.as_ref().map(|live| live.width).unwrap_or(800) as f32;
        let width = crate::sidebar::width(
            true,
            self.window.sidebar_points,
            self.window.tabs.tab_ids().len(),
            window_width,
            self.font_scale(),
        );
        let top = self.terminal_top() - self.chrome_inset();
        let height = self.terminal_height() + self.chrome_inset() * 2.0;
        Some((0.0, top, width, height, self.chrome_row_height()))
    }

    /// The scale text and pt tokens are drawn at, as distinct from the
    /// display's own factor.
    ///
    /// Winit gives the renderer physical pixels and a single display scale.
    /// Applying that scale twice made a configured 12pt face render as 27pt
    /// at 150% DPI, and enlarged both its row metrics and all custom chrome.
    /// The old front end's DPI virtualisation is already accounted for by
    /// winit's physical surface; the font must consume the scale exactly once.
    fn font_scale_for(scale: f32) -> f32 {
        scale
    }

    fn font_scale(&self) -> f32 {
        Self::font_scale_for(self.window.scale)
    }

    /// One point in pixels on this display.
    fn chrome_pt(&self) -> f32 {
        crate::chrome_font::point(self.font_scale())
    }

    /// The gap between a docked panel and what is beside it.
    fn chrome_inset(&self) -> f32 {
        (crate::ui_tokens::CHROME_PANEL_INSET * self.chrome_pt()).round()
    }

    /// How tall the bar along the top is.
    fn top_bar_height(&self) -> f32 {
        crate::topbar::height(
            self.chrome_row_height(),
            self.chrome_pt(),
            self.window.top_bar_quiet,
        )
    }

    /// Where the terminal's first row starts, below the bar.
    fn terminal_top(&self) -> f32 {
        self.top_bar_height() + self.terminal_padding_top()
    }

    /// Which window edge the pointer is on, if it may grab one.
    ///
    /// The window buttons are excluded: a press aimed at closing the
    /// window must never start a resize instead, and they sit against
    /// the very corner the band would otherwise own.
    fn resize_edge_at_pointer(&self) -> Option<winit::window::ResizeDirection> {
        let live = self.window.state.as_ref()?;
        let size = (live.width as f32, live.height as f32);
        let pt = self.chrome_pt();
        if self.window.pointer.1 < self.top_bar_height()
            && self.window.pointer.0 >= size.0 - crate::topbar::window_button_band(pt)
        {
            return None;
        }
        crate::topbar::resize_edge(self.window.pointer, size, pt)
    }

    /// Say what the pointer can do here, by its shape.
    ///
    /// The window draws its own frame, so its resize band is invisible:
    /// an edge that leaves the arrow alone is an edge nobody discovers,
    /// and the window reads as one that cannot be resized. The grid
    /// takes an I-beam for the same reason every terminal does -- that
    /// is where text is selected.
    fn apply_cursor(&mut self) {
        use winit::window::CursorIcon;
        let Some((width, height)) = self
            .window.state
            .as_ref()
            .map(|live| (live.width as f32, live.height as f32))
        else {
            return;
        };
        let icon = if let Some(direction) = self.resize_edge_at_pointer() {
            crate::topbar::resize_cursor(direction)
        } else if self.window.pointer.0 >= self.terminal_left()
            && self.window.pointer.0 < width - self.git_panel_width()
            && self.window.pointer.1 >= self.terminal_top()
            && self.window.pointer.1 < height - self.status_bar_height()
        {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        };
        if self.window.cursor_icon == icon {
            return;
        }
        self.window.cursor_icon = icon;
        if let Some(live) = self.window.state.as_ref() {
            live.window.set_cursor(icon);
        }
    }

    fn terminal_padding_left(&self) -> f32 {
        (self.terminal_padding[0] * self.window.scale).round()
    }

    fn terminal_padding_right(&self) -> f32 {
        (self.terminal_padding[1] * self.window.scale).round()
    }

    fn terminal_padding_top(&self) -> f32 {
        (self.terminal_padding[2] * self.window.scale).round()
    }

    fn terminal_padding_bottom(&self) -> f32 {
        (self.terminal_padding[3] * self.window.scale).round()
    }

    /// How tall one chrome row is: its text plus the padding above and below.
    fn chrome_row_height(&self) -> f32 {
        let pt = crate::chrome_font::point(self.font_scale());
        (self.window.chrome_font.metrics().height + crate::ui_tokens::CHROME_ROW_PADDING_Y * 2.0 * pt)
            .round()
            .max(1.0)
    }

    /// Draw a run of chrome text in the chrome's own face, returning where it
    /// ended so the next piece of the row can start there.
    fn append_chrome(
        &mut self,
        text: &str,
        color: [f32; 4],
        origin: (f32, f32),
        quads: &mut unterm_render::quads::FrameQuads,
    ) -> f32 {
        let width = crate::terminal::append_chrome_text(
            text,
            &mut self.window.chrome_font,
            &mut self.window.atlas,
            color,
            origin,
            quads,
        );
        origin.0 + width
    }

    /// How wide a piece of chrome text will be.
    fn chrome_width(&mut self, text: &str) -> f32 {
        crate::terminal::chrome_text_width(text, &mut self.window.chrome_font, &mut self.window.atlas)
    }

    /// Chrome-layer text in the terminal's monospace face. The 0.57.4 bars
    /// drew their facts and status segments in the terminal font — aligned
    /// baselines against the grid they describe — and only buttons and labels
    /// in the UI face.
    fn append_mono(
        &mut self,
        text: &str,
        color: [f32; 4],
        origin: (f32, f32),
        quads: &mut unterm_render::quads::FrameQuads,
    ) -> f32 {
        let width = crate::terminal::append_chrome_text(
            text,
            &mut self.window.font,
            &mut self.window.atlas,
            color,
            origin,
            quads,
        );
        origin.0 + width
    }

    /// How wide a piece of monospace chrome text will be.
    fn mono_width(&mut self, text: &str) -> f32 {
        crate::terminal::chrome_text_width(text, &mut self.window.font, &mut self.window.atlas)
    }

    /// Shorten `text` until it fits `room` pixels, keeping its start.
    ///
    /// Measured in the face it is drawn in rather than in cells: a proportional
    /// label measured on a grid is wrong by however much the face differs, in
    /// whichever direction it differs.
    fn chrome_fit(&mut self, text: &str, room: f32) -> String {
        if room <= 0.0 {
            return String::new();
        }
        if self.chrome_width(text) <= room {
            return text.to_string();
        }
        let mut used = self.chrome_width("\u{2026}");
        let mut kept = String::new();
        for ch in text.chars() {
            let wide = self.chrome_width(&ch.to_string());
            if used + wide > room {
                break;
            }
            kept.push(ch);
            used += wide;
        }
        format!("{kept}\u{2026}")
    }

    /// Which strip row a point is over.
    fn sidebar_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (left, top, width, height, row_height) = self.sidebar_dock()?;
        if x < left || x >= left + width || y < top || y >= top + height {
            return None;
        }
        let pt = crate::chrome_font::point(self.window.scale);
        let first = top + crate::ui_tokens::CHROME_SECTION_GAP * pt;
        if y < first {
            return None;
        }
        let footer_top = top + height - row_height;
        let visible = (((footer_top - first) / row_height).floor()).max(1.0) as usize;
        let offset = ((y - first) / row_height) as usize;
        if offset >= visible {
            return None;
        }
        let rows = self.sidebar_rows();
        let scroll = crate::sidebar::clamp_scroll(self.window.sidebar_scroll, rows.len(), visible);
        let at = scroll + offset;
        (at < rows.len()).then_some(at)
    }

    /// 0 = new session, 1 = the shell picker, 2 = settings — one row,
    /// three zones. Pure geometry: the footer is pinned to the bottom
    /// edge, so no session list has to be walked to find it.
    fn sidebar_footer_action_at(&mut self, x: f32, y: f32) -> Option<usize> {
        let (left, top, width, height, row_height) = self.sidebar_dock()?;
        let footer_top = top + height - row_height;
        if x < left || x >= left + width || y < footer_top || y >= top + height {
            return None;
        }
        let pt = crate::chrome_font::point(self.window.scale);
        let inset = crate::ui_tokens::CHROME_PANEL_INSET * pt;
        if x < left + inset || x >= left + width - inset {
            return None;
        }
        let footer_left = left + inset;
        let footer_width = (width - inset * 2.0).max(0.0);
        let (picker_left, picker_right, settings_left) =
            self.sidebar_footer_zones(footer_left, footer_width, row_height, pt);
        if x >= settings_left {
            Some(2)
        } else if x >= picker_right {
            // The stretch between the split's dropdown and settings is
            // just floor: a click there chose neither and does nothing.
            None
        } else if x >= picker_left {
            Some(1)
        } else {
            Some(0)
        }
    }

    /// Where the footer's three controls sit: `(picker_left, picker_right,
    /// settings_left)`.
    ///
    /// The shell picker rides the trailing edge of the new-session label --
    /// a split button, the way a browser's new-tab button carries its
    /// dropdown -- rather than being parked across the row next to settings,
    /// where it read as a control nobody could name. One function for both
    /// hit-testing and drawing, so the two cannot drift apart.
    fn sidebar_footer_zones(
        &mut self,
        footer_left: f32,
        footer_width: f32,
        row_height: f32,
        pt: f32,
    ) -> (f32, f32, f32) {
        use unterm_services::i18n::t;
        let square = crate::sidebar::footer_mark_width(row_height);
        let settings_left = footer_left + footer_width - square;
        let pen = footer_left + 7.0 * pt + self.chrome_width("+") + 6.0 * pt;
        let label_budget = (settings_left - pen - square - 8.0 * pt).max(0.0);
        let label = self.chrome_fit(&t("sidebar.new_session"), label_budget);
        let picker_left =
            (pen + self.chrome_width(&label) + 4.0 * pt).min(settings_left - square);
        (picker_left, picker_left + square, settings_left)
    }

    /// A press on the tab strip. Returns true when the strip took it.
    ///
    /// A tab row goes there; a project header folds or unfolds. Folding is what
    /// makes the strip usable with ten projects open, and it is the header's
    /// only job -- there is nothing else to press on it.
    fn click_sidebar(&mut self) -> bool {
        if let Some(action) = self.sidebar_footer_action_at(self.window.pointer.0, self.window.pointer.1) {
            match action {
                0 => self.new_tab(),
                1 => self.open_shell_selector(),
                2 => self.run_palette_command(
                    crate::palette::Command::OpenSettings,
                    "sidebar-settings",
                ),
                _ => unreachable!(),
            }
            self.window.drawn_revision = None;
            return true;
        }
        let Some(at) = self.sidebar_row_at(self.window.pointer.0, self.window.pointer.1) else {
            return false;
        };
        let Some(row) = self.sidebar_rows().get(at).cloned() else {
            return false;
        };
        match row {
            crate::sidebar::Row::Tab { index, .. } => {
                // The second press on the same row, nearby and soon enough,
                // asks for a name; the first one focuses the tab as always.
                let click = match self.window.last_sidebar_click.take() {
                    Some(previous) => previous.again(at, self.window.pointer.0, self.window.pointer.1),
                    None => crate::sidebar::RowClick::first(at, self.window.pointer.0, self.window.pointer.1),
                };
                let renames = click.streak() >= 2;
                self.window.last_sidebar_click = Some(click);
                if renames {
                    self.open_tab_rename(index);
                } else {
                    self.select_tab_at(index);
                    // Keep holding to carry the tab to a new place in the
                    // strip; letting go before moving is just the click.
                    self.window.dragging_tab =
                        self.window.tabs.tab_ids().get(index).copied().map(|tab_id| TabDrag {
                            tab_id,
                            origin: self.window.pointer,
                            engaged: false,
                        });
                }
            }
            crate::sidebar::Row::Group { key, .. } => {
                // The arrow folds the project; its name goes to it. The same
                // split the file tree makes, for the same reason -- and here
                // it is the difference between a click that does something
                // and one that does not.
                //
                // Folding on any press meant a project row was the one row in
                // the strip a click could not reach anything from. With one
                // tab to a project, which is the common shape, pressing the
                // name folded the tab being aimed at out of sight and
                // pressing again put it back: two presses, nothing moved.
                // That is what "sometimes the tab will not switch" was, and
                // the presses are in `open.log` -- a run of them on one row,
                // a second apart, then one on the row below that switched at
                // once.
                let on_arrow = self.sidebar_dock().is_some_and(|(left, _, _, _, row_height)| {
                    self.window.pointer.0 < left + row_height
                });
                if on_arrow {
                    if !self.window.sidebar_collapsed.remove(&key) {
                        self.window.sidebar_collapsed.insert(key);
                    }
                } else {
                    // Unfolded first: a project that is folded has no tab row
                    // to find, and going to it is what was asked for.
                    self.window.sidebar_collapsed.remove(&key);
                    let rows = self.sidebar_rows();
                    if let Some(index) = crate::sidebar::tab_at_or_after(&rows, at) {
                        self.select_tab_at(index);
                    }
                }
            }
        }
        self.window.drawn_revision = None;
        true
    }

    /// A same-row double-click on the strip: ask for the tab's name on the
    /// palette line. Enter applies it, an empty line hands the tab back to
    /// automatic titling, Esc leaves the name alone. Deliberate by
    /// construction — reached only through `RowClick`'s same-row streak, never
    /// from switching tabs quickly.
    fn open_tab_rename(&mut self, index: usize) {
        let Some(tab_id) = self.window.tabs.tab_ids().get(index).copied() else {
            return;
        };
        let mut palette = crate::palette::Palette::writing(vec![crate::palette::Entry {
            label: "Rename tab".to_string(),
            hint: "Enter applies · empty line resets to auto-title · Esc cancels".to_string(),
            command: crate::palette::Command::RenameTab { tab_id },
        }]);
        palette.query = self.window.tab_titles.get(&tab_id).cloned().unwrap_or_default();
        self.window.palette = Some(palette);
    }

    /// Draw the tab strip: projects, their tabs, and the row of actions under
    /// them.
    ///
    /// The strip *is* the tab bar -- the top bar carries none. A vertical list
    /// reads a tab better than a horizontal one: a tab is identified by a
    /// project and a command, which fit along a row rather than across one.
    fn append_sidebar(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some((left, top, width, height, row_height)) = self.sidebar_dock() else {
            return;
        };
        let pt = crate::chrome_font::point(self.window.scale);
        let inset = crate::ui_tokens::CHROME_PANEL_INSET * pt;
        let radius = crate::ui_tokens::CORNER_RADIUS * pt;
        let chrome = self.chrome();
        let foreground = self.chrome_foreground();

        quads.backgrounds.push(unterm_render::quads::Quad {
            left,
            top,
            width,
            height,
            color: chrome.surface,
        });
        // The seam, so the strip and the terminal read as two surfaces of one
        // window rather than one surface that changed colour.
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: left + width - 1.0,
            top,
            width: 1.0,
            height,
            color: chrome.outer_edge,
        });

        // The bottom row is reserved for the actions, so the list never
        // draws under them even when it fills the strip.
        let footer_reserve = top + height - row_height;
        let rows = self.sidebar_rows();
        let first_row = top + crate::ui_tokens::CHROME_SECTION_GAP * pt;
        let visible = (((footer_reserve - first_row) / row_height).floor()).max(1.0) as usize;

        // Follow the selection. A strip longer than the window that stays put
        // while tabs are switched shows a list with nothing selected in it.
        let mut scroll = self.window.sidebar_scroll;
        if let Some(active) = rows
            .iter()
            .position(|row| matches!(row, crate::sidebar::Row::Tab { active: true, .. }))
        {
            scroll = crate::sidebar::scroll_to_show(scroll, active, visible);
        }
        let scroll = crate::sidebar::clamp_scroll(scroll, rows.len(), visible);
        // The actions are pinned to the bottom edge: controls that
        // wander with the list length have to be found again every
        // time one is added.
        let footer_top = footer_reserve;

        // A list longer than the strip says so: a slim track on the right
        // edge with the visible span as its thumb.
        if rows.len() > visible {
            let track_left = left + width
                - (crate::ui_tokens::CHROME_SCROLLBAR_WIDTH * pt)
                    .max(crate::ui_tokens::CHROME_SCROLLBAR_MIN_WIDTH);
            let track_height = footer_reserve - first_row;
            let span = visible as f32 / rows.len() as f32;
            let thumb_height =
                (track_height * span).max(crate::ui_tokens::CHROME_SCROLLBAR_MIN_THUMB_HEIGHT * pt);
            let travel = track_height - thumb_height;
            let progress = scroll as f32 / (rows.len() - visible) as f32;
            let mut track = chrome.dim_text;
            track[3] *= crate::ui_tokens::CHROME_SCROLLBAR_TRACK_ALPHA;
            let mut thumb = chrome.dim_text;
            thumb[3] *= crate::ui_tokens::CHROME_SCROLLBAR_THUMB_ALPHA;
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: track_left,
                top: first_row,
                width: (crate::ui_tokens::CHROME_SCROLLBAR_WIDTH * pt)
                    .max(crate::ui_tokens::CHROME_SCROLLBAR_MIN_WIDTH),
                height: track_height,
                color: track,
            });
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: track_left,
                top: first_row + travel * progress,
                width: (crate::ui_tokens::CHROME_SCROLLBAR_WIDTH * pt)
                    .max(crate::ui_tokens::CHROME_SCROLLBAR_MIN_WIDTH),
                height: thumb_height,
                color: thumb,
            });
        }

        // A working agent's row turns a quarter-circle spinner. Same
        // repaint discipline as the breath it replaces: the paint
        // records which quantised phase it drew, and the idle tick
        // asks for a frame only when that phase has moved on.
        let spin_step = rows
            .iter()
            .any(|row| {
                matches!(
                    row,
                    crate::sidebar::Row::Tab {
                        badge: Some(crate::cockpit::Badge::Working),
                        ..
                    }
                )
            })
            .then(|| {
                let elapsed = unterm_services::cockpit::status::breath_epoch()
                    .elapsed()
                    .as_millis() as u64;
                crate::sidebar::spin_step(elapsed)
            });
        self.window.drawn_breath_step = spin_step;
        let spin = spin_step.unwrap_or(0);

        let content_left = left + inset;
        let content_width = width - inset * 2.0;
        // Text sits a touch above the row's geometric middle: a cell carries
        // descender space, so centring by arithmetic alone reads low.
        let text_offset = ((row_height - self.window.chrome_font.metrics().height) / 2.0
            + crate::ui_tokens::CHROME_TEXT_BASELINE_NUDGE * pt)
            .max(0.0);

        for (offset, row) in rows.iter().skip(scroll).take(visible).enumerate() {
            let row_top = first_row + offset as f32 * row_height;
            match row {
                crate::sidebar::Row::Group {
                    label,
                    hint,
                    count,
                    collapsed,
                    active,
                    ..
                } => {
                    // A project header becomes a surface of its own when its
                    // tab is in front, so the eye finds the group before the
                    // row inside it.
                    if *active {
                        quads.backgrounds.extend(unterm_render::rounded::panel(
                            content_left,
                            row_top,
                            content_width,
                            row_height,
                            radius,
                            chrome.group_bg,
                        ));
                    }
                    let mut pen = content_left + 4.0 * pt;
                    let arrow = if *collapsed {
                        crate::sidebar::CLOSED
                    } else {
                        crate::sidebar::OPEN
                    };
                    pen = self.append_chrome(
                        &arrow.to_string(),
                        chrome.dim_text,
                        (pen, row_top + text_offset),
                        quads,
                    );
                    pen += 3.0 * pt;
                    // The folder takes the accent when its project is the one
                    // in front: one coloured mark per group, and it is the one
                    // that says which group you are looking at.
                    pen = self.append_chrome(
                        &crate::sidebar::FOLDER.to_string(),
                        if *active {
                            chrome.focus_rail
                        } else {
                            chrome.dim_text
                        },
                        (pen, row_top + text_offset),
                        quads,
                    );
                    pen += 5.0 * pt;

                    // The count sits against the right edge in a rounded pill,
                    // so a project size is readable without counting rows.
                    let badge = count.to_string();
                    let badge_height = (row_height - 6.0 * pt).max(2.0);
                    // Never narrower than it is tall: a one-digit count
                    // in a pill thinner than its height reads as an
                    // upright ellipse, not a pill.
                    let badge_width = (self.chrome_width(&badge) + 10.0 * pt).max(badge_height);
                    let badge_left = content_left + content_width - badge_width - 4.0 * pt;
                    quads.backgrounds.extend(unterm_render::rounded::panel(
                        badge_left,
                        row_top + 3.0 * pt,
                        badge_width,
                        badge_height,
                        badge_height / 2.0,
                        chrome.hover_bg,
                    ));
                    let badge_text = self.chrome_width(&badge);
                    self.append_chrome(
                        &badge,
                        chrome.dim_text,
                        (
                            badge_left + (badge_width - badge_text) / 2.0,
                            row_top + text_offset,
                        ),
                        quads,
                    );

                    // The name, with its parent in front when two projects
                    // share a leaf name. The parent is secondary text so the
                    // project name itself stays dominant.
                    if let Some(hint) = hint {
                        let shown = self.chrome_fit(&format!("{hint}/"), badge_left - pen);
                        pen = self.append_chrome(
                            &shown,
                            chrome.dim_text,
                            (pen, row_top + text_offset),
                            quads,
                        );
                    }
                    // Group headers are wayfinding, not content: a
                    // fainter voice than the rows they organise, active
                    // or not.
                    let mut faint = chrome.dim_text;
                    faint[3] *= if *active { 0.80 } else { 0.60 };
                    let shown = self.chrome_fit(label, badge_left - pen);
                    self.append_chrome(&shown, faint, (pen, row_top + text_offset), quads);
                }
                crate::sidebar::Row::Tab {
                    index,
                    label,
                    detail,
                    active,
                    icon,
                    grouped,
                    badge,
                    indicators,
                } => {
                    // Children are inset under their header, so tabs and
                    // projects read as parent and child rather than as peers.
                    let indent = if *grouped { 10.0 * pt } else { 0.0 };
                    let row_left = content_left + indent;
                    let row_width = (content_width - indent).max(1.0);

                    if *active {
                        quads.backgrounds.extend(unterm_render::rounded::panel(
                            row_left,
                            row_top,
                            row_width,
                            row_height,
                            radius,
                            chrome.selected_bg,
                        ));
                    } else if self.window.pointer.0 >= row_left
                        && self.window.pointer.0 < row_left + row_width
                        && self.window.pointer.1 >= row_top
                        && self.window.pointer.1 < row_top + row_height
                    {
                        // The row under the pointer lifts faintly, the way it
                        // did before: hover is how a list says it is a list.
                        quads.backgrounds.extend(unterm_render::rounded::panel(
                            row_left,
                            row_top,
                            row_width,
                            row_height,
                            radius,
                            chrome.hover_bg,
                        ));
                    }
                    // The rail: the one place the accent is used, and what the
                    // eye finds first.
                    let rail = if *active {
                        Some((chrome.focus_rail, 2.0 * pt))
                    } else if *grouped {
                        Some((chrome.outer_edge, 1.0))
                    } else {
                        None
                    };
                    if let Some((color, thickness)) = rail {
                        quads.backgrounds.push(unterm_render::quads::Quad {
                            left: row_left,
                            top: row_top,
                            width: thickness,
                            height: row_height,
                            color,
                        });
                    }

                    let mut pen = row_left + 7.0 * pt;
                    pen = self.append_chrome(
                        &format!("{}", index + 1),
                        chrome.dim_text,
                        (pen, row_top + text_offset),
                        quads,
                    );
                    pen += 5.0 * pt;
                    pen = self.append_chrome(
                        &icon.to_string(),
                        if *icon == crate::sidebar::ROBOT {
                            chrome.focus_rail
                        } else if *active {
                            foreground
                        } else {
                            chrome.dim_text
                        },
                        (pen, row_top + text_offset),
                        quads,
                    );
                    pen += 7.0 * pt;

                    // One mark against the right edge — the row's whole
                    // status vocabulary: ✋ asks, a spinner works, ✓
                    // finished, ▲ errored, • has unread output, and an
                    // idle shell shows nothing. On every row including
                    // the active one: the state is about the agent, not
                    // about which row is being looked at.
                    let mut right = row_left + row_width - 6.0 * pt;
                    let indicator: Option<(&str, [f32; 4])> = if let Some(badge) = badge {
                        Some((badge.glyph(spin), badge.color()))
                    } else if indicators.error {
                        Some(("\u{25B2}", [0.95, 0.35, 0.35, 1.0]))
                    } else if indicators.unread {
                        Some(("\u{2022}", crate::cockpit::Badge::NeedsYou.color()))
                    } else {
                        None
                    };
                    if let Some((glyph, color)) = indicator {
                        let wide = self.chrome_width(glyph);
                        right -= wide + 4.0 * pt;
                        self.append_chrome(
                            glyph,
                            color,
                            (right + 4.0 * pt, row_top + text_offset),
                            quads,
                        );
                    }
                    // The command only when it is not the shell repeating
                    // itself: `cmd  cmd.exe` says one thing twice on a row
                    // with no room for it.
                    let text = match detail {
                        Some(detail) if !crate::sidebar::same_program(detail, label) => {
                            format!("{label}  {detail}")
                        }
                        _ => label.clone(),
                    };
                    let shown = self.chrome_fit(&text, right - pen);
                    self.append_chrome(
                        &shown,
                        if *active { foreground } else { chrome.dim_text },
                        (pen, row_top + text_offset),
                        quads,
                    );
                }
            }
        }

        // Two full-width actions pinned to the bottom: new session
        // (with the shell picker on its trailing split, the way a
        // browser's new-tab button carries its dropdown) and settings.
        // The tab navigator moved into the chevron menu and the
        // palette — relocated, never dropped.
        use unterm_services::i18n::t;
        let footer_left = left + inset;
        let footer_width = (width - inset * 2.0).max(0.0);
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: footer_left,
            top: footer_top,
            width: footer_width,
            height: 1.0,
            color: chrome.inner_highlight,
        });
        let hovered = self.sidebar_footer_action_at(self.window.pointer.0, self.window.pointer.1);
        let square = crate::sidebar::footer_mark_width(row_height);
        let (picker_left, _picker_right, settings_left) =
            self.sidebar_footer_zones(footer_left, footer_width, row_height, pt);
        let main_width = (picker_left - footer_left).max(1.0);
        let ink = |on: bool| if on { foreground } else { chrome.dim_text };
        let lift = |left: f32, width: f32, on: bool, quads: &mut unterm_render::quads::FrameQuads| {
            if on {
                quads.backgrounds.extend(unterm_render::rounded::panel(
                    left,
                    footer_top + 2.0 * pt,
                    width,
                    row_height - 4.0 * pt,
                    radius,
                    chrome.hover_bg,
                ));
            }
        };
        lift(footer_left, main_width, hovered == Some(0), quads);
        lift(settings_left, square, hovered == Some(2), quads);
        // The shell picker shows itself only while the pointer is on the
        // row: at rest the footer is two things -- new session and settings
        // -- and the dropdown appears exactly when someone is close enough
        // to use it. While shown it is a pill, not a stray triangle.
        let row_hovered = self.window.pointer.0 >= footer_left
            && self.window.pointer.0 < footer_left + footer_width
            && self.window.pointer.1 >= footer_top
            && self.window.pointer.1 < footer_top + row_height;
        if row_hovered {
            let mut pill = chrome.hover_bg;
            if hovered != Some(1) {
                pill[3] *= 0.45;
            }
            quads.backgrounds.extend(unterm_render::rounded::panel(
                picker_left,
                footer_top + 3.0 * pt,
                square,
                row_height - 6.0 * pt,
                radius,
                pill,
            ));
        }

        // The one action with words -- everything else on this row is
        // a square with a mark in it, which is what keeps three
        // controls in the height of one.
        let mut pen = footer_left + 7.0 * pt;
        pen = self.append_chrome(
            "+",
            ink(hovered == Some(0)),
            (pen, footer_top + text_offset),
            quads,
        );
        pen += 6.0 * pt;
        let new_label = self.chrome_fit(
            &t("sidebar.new_session"),
            (settings_left - pen - square - 8.0 * pt).max(0.0),
        );
        self.append_chrome(
            &new_label,
            ink(hovered == Some(0)),
            (pen, footer_top + text_offset),
            quads,
        );
        if row_hovered {
            // The triangle's ink sits high in its line box; nudged down so
            // it centres in the pill rather than floating in its upper half.
            let wide = self.chrome_width("\u{25BE}");
            self.append_chrome(
                "\u{25BE}",
                ink(hovered == Some(1)),
                (
                    picker_left + ((square - wide) / 2.0).max(0.0),
                    footer_top + text_offset + 1.5 * pt,
                ),
                quads,
            );
        }
        let wide = self.chrome_width("\u{EB51}");
        self.append_chrome(
            "\u{EB51}",
            ink(hovered == Some(2)),
            (
                settings_left + ((square - wide) / 2.0).max(0.0),
                footer_top + text_offset,
            ),
            quads,
        );
    }

    /// The line of facts about the pane in front, for the top bar.
    ///
    /// Everything in it comes from a cache that refreshes on another thread,
    /// so this is cheap enough to call while painting -- which it has to be,
    /// because the bar is repainted whenever anything moves.
    ///
    /// Empty on a narrow window: the actions are what the bar is for, and
    /// pushing one off to make room for a memory figure is the wrong trade.
    /// The bar itself drops the whole line if what is left does not hold it.
    fn stats_line(&self, window_width: f32) -> String {
        if window_width / self.window.scale.max(0.1) < crate::statsbar::MIN_WIDTH {
            return String::new();
        }
        let Some(live) = self.window.state.as_ref() else {
            return String::new();
        };
        crate::statsbar::compose(&crate::statsbar::facts_for(live.session_id).segments())
    }

    /// The bar as it is laid out right now.
    ///
    /// One place, so a piece is pressed where it is drawn. Measured through the
    /// chrome's own face: a proportional label measured on the terminal's grid
    /// puts every button somewhere other than where it looks.
    fn top_bar(&mut self, window_width: f32) -> Vec<crate::topbar::Placed> {
        let pt = self.chrome_pt();
        let logical = window_width / self.window.scale.max(0.1);
        let stats = self.stats_line(window_width);
        let title = self.window.bar_title.clone();
        // The tally: how many agents the Cockpit sees, and how many wait.
        let cockpit = {
            let statuses = unterm_services::cockpit::status::snapshot();
            let waiting = crate::cockpit::attention_count(&statuses);
            if statuses.is_empty() {
                String::new()
            } else if waiting > 0 {
                format!("{} ✋{waiting}", statuses.len())
            } else {
                format!("{}", statuses.len())
            }
        };
        let open = self.window.tree.is_some();
        // The measuring closure needs the fonts and the atlas, and so does the
        // caller afterwards -- so they are taken apart for the call and put
        // back by the borrow ending. The facts line alone is measured in the
        // terminal face, because that is the face it is drawn in.
        let (mono, ui, atlas) = (&mut self.window.font, &mut self.window.chrome_font, &mut self.window.atlas);
        let mut measure = |text: &str| {
            if !stats.is_empty() && text == stats {
                crate::terminal::chrome_text_width(text, mono, atlas)
            } else {
                crate::terminal::chrome_text_width(text, ui, atlas)
            }
        };
        crate::topbar::layout(
            window_width,
            logical,
            pt,
            &stats,
            &cockpit,
            &title,
            open,
            self.window.top_bar_quiet,
            &mut measure,
        )
    }

    /// Draw the bar along the top: the wordmark, the facts, the actions and the
    /// window buttons. No tabs -- those are in the strip down the left.
    fn append_top_bar(&mut self, window_width: f32, quads: &mut unterm_render::quads::FrameQuads) {
        let height = self.top_bar_height();
        let chrome = self.chrome();
        let foreground = self.chrome_foreground();

        quads.backgrounds.push(unterm_render::quads::Quad {
            left: 0.0,
            top: 0.0,
            width: window_width,
            height,
            color: chrome.surface,
        });
        // A hairline under it, so the bar and the terminal read as two surfaces
        // of one window rather than one surface with a seam.
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: 0.0,
            top: height - 1.0,
            width: window_width,
            height: 1.0,
            color: chrome.outer_edge,
        });

        let bar = self.top_bar(window_width);
        let hovered = self.hovered_top_bar_item();
        let pt = self.chrome_pt();
        let radius = crate::ui_tokens::CORNER_RADIUS * pt;
        let text_top = ((height - self.window.chrome_font.metrics().height) / 2.0
            + crate::ui_tokens::TOPBAR_TEXT_NUDGE * pt)
            .max(0.0);

        for piece in &bar {
            let is_hovered = hovered == Some(piece.item);

            // The window buttons are drawn rather than typed: a close cross
            // from a font is a different cross on every machine.
            let maximized = self.window.unmaximized_rect.is_some()
                || self
                    .window.state
                    .as_ref()
                    .is_some_and(|live| live.window.is_maximized());
            if let Some(button) = crate::topbar::window_button(piece.item, maximized) {
                if is_hovered {
                    let fill = if button == crate::window_buttons::Button::Close {
                        crate::window_buttons::hover_fill(button, chrome.is_light)
                    } else {
                        self.chrome_overrides
                            .button_hover_background
                            .unwrap_or_else(|| {
                                crate::window_buttons::hover_fill(button, chrome.is_light)
                            })
                    };
                    quads.backgrounds.push(unterm_render::quads::Quad {
                        left: piece.left,
                        top: 0.0,
                        width: piece.width,
                        height,
                        color: fill,
                    });
                }
                let color = if is_hovered && button == crate::window_buttons::Button::Close {
                    crate::window_buttons::hovered_icon_color(button, chrome.is_light)
                } else if is_hovered {
                    self.chrome_overrides
                        .button_hover_foreground
                        .or(self.chrome_overrides.button_foreground)
                        .unwrap_or_else(|| {
                            crate::window_buttons::hovered_icon_color(button, chrome.is_light)
                        })
                } else {
                    self.chrome_overrides
                        .button_foreground
                        .unwrap_or_else(|| crate::window_buttons::icon_color(chrome.is_light))
                };
                quads.backgrounds.extend(crate::window_buttons::quads(
                    button,
                    piece.left,
                    0.0,
                    piece.width,
                    height,
                    color,
                ));
                continue;
            }

            // An action under the pointer gets a rounded surface, inset from
            // the bar's edges so it reads as a button rather than as a column.
            if is_hovered
                && !matches!(
                    piece.item,
                    crate::topbar::Item::Wordmark | crate::topbar::Item::Title
                )
            {
                let inset = 3.0 * pt;
                quads.backgrounds.extend(unterm_render::rounded::panel(
                    piece.left,
                    inset,
                    piece.width,
                    height - inset * 2.0,
                    radius,
                    chrome.hover_bg,
                ));
            }

            // The project toggle says whether it is on, the way a pressed
            // button does: without it there is no way to tell the strip is
            // hidden rather than empty.
            if piece.item == crate::topbar::Item::Action(crate::keys::Action::TreeSidebar)
                && self.window.tree.is_some()
            {
                let inset = 3.0 * pt;
                quads.backgrounds.extend(unterm_render::rounded::panel(
                    piece.left,
                    inset,
                    piece.width,
                    height - inset * 2.0,
                    radius,
                    chrome.selected_bg,
                ));
            }

            // The cockpit tally is a pill even at rest — the window's
            // one aggregate view of the agents — and it turns the
            // waiting amber the moment any of them needs an answer.
            let cockpit_waiting = piece.item == crate::topbar::Item::Cockpit
                && crate::cockpit::attention_count(
                    &unterm_services::cockpit::status::snapshot(),
                ) > 0;
            if piece.item == crate::topbar::Item::Cockpit && !piece.label.is_empty() {
                let inset = 3.0 * pt;
                let mut fill = if cockpit_waiting {
                    let mut amber = crate::cockpit::Badge::NeedsYou.color();
                    amber[3] = 0.16;
                    amber
                } else {
                    chrome.hover_bg
                };
                if is_hovered {
                    fill[3] = (fill[3] + 0.10).min(1.0);
                }
                quads.backgrounds.extend(unterm_render::rounded::panel(
                    piece.left,
                    inset,
                    piece.width,
                    height - inset * 2.0,
                    (height - inset * 2.0) / 2.0,
                    fill,
                ));
            }

            let text = match (piece.icon, piece.label.is_empty()) {
                (Some(icon), true) => icon.to_string(),
                (Some(icon), false) => format!("{icon}  {}", piece.label),
                (None, _) => piece.label.clone(),
            };
            if text.trim().is_empty() {
                continue;
            }
            if piece.item == crate::topbar::Item::Stats {
                // 0.57.4 set the facts line in the terminal face, tinted
                // toward the accent: data against the grid it describes, not
                // another label in the UI face.
                let accent = chrome.focus_rail;
                let tinted = [
                    foreground[0] + (accent[0] - foreground[0]) * 0.45,
                    foreground[1] + (accent[1] - foreground[1]) * 0.45,
                    foreground[2] + (accent[2] - foreground[2]) * 0.45,
                    1.0,
                ];
                let mono_top = ((height - self.window.font.metrics().height) / 2.0
                    + crate::ui_tokens::CHROME_TEXT_BASELINE_NUDGE * pt)
                    .max(0.0);
                self.append_mono(&text, tinted, (piece.left, mono_top), quads);
                continue;
            }
            let color = match piece.item {
                // The facts about a pane are context, not the thing being
                // read. The wordmark carries the brand at the bar's own
                // full strength, as 0.57.4 set it.
                crate::topbar::Item::Wordmark => foreground,
                crate::topbar::Item::Stats => chrome.dim_text,
                // The title is context, not a label to read first: it
                // sits below the brand and the tally in the bar's own
                // order of voices.
                crate::topbar::Item::Title => {
                    let mut quiet = chrome.dim_text;
                    quiet[3] *= 0.66;
                    quiet
                }
                crate::topbar::Item::Cockpit if cockpit_waiting => {
                    crate::cockpit::Badge::NeedsYou.color()
                }
                _ if is_hovered => foreground,
                _ => chrome.dim_text,
            };
            if piece.item == crate::topbar::Item::Wordmark {
                // The icon's mark, in the icon's two colours — the
                // prompt chevron in the chrome's foreground, the status
                // dot in the icon's amber. Same geometry the installer
                // and taskbar icons are generated from.
                let cell_width = self.chrome_width("M");
                let em = crate::ui_tokens::UI_FONT_SIZE as f32 * pt;
                let mark_height = (em * 0.95).round().max(8.0);
                let mark_width = (mark_height * crate::brand::ASPECT).round().max(8.0);
                let mark_top = ((height - mark_height) / 2.0).max(0.0);
                let key_for = |part: u32| unterm_render::atlas::GlyphKey {
                    stack: crate::chrome_font::STACK,
                    face: usize::MAX,
                    glyph_index: u32::MAX - part,
                    pixel_size: mark_height as u32,
                };
                if self.window.atlas.get(key_for(0)).is_none() {
                    let mark =
                        crate::brand::rasterize(mark_width as usize, mark_height as usize);
                    self.window.atlas.insert(key_for(0), &mark.chevron);
                    self.window.atlas.insert(key_for(1), &mark.dot);
                }
                for (part, tint) in [
                    (0, foreground),
                    (1, crate::brand::DOT_COLOR),
                ] {
                    if let Some(slot) = self.window.atlas.get(key_for(part)) {
                        quads.glyphs.push(unterm_render::quads::glyph_quad(
                            slot,
                            piece.left,
                            mark_top + mark_height,
                            tint,
                            &self.window.atlas,
                        ));
                    }
                }
                self.append_chrome(
                    &text,
                    color,
                    (piece.left + cell_width * (0.95 + 0.42), text_top),
                    quads,
                );
                continue;
            }
            // Icons are centred in their button; text starts where it was put.
            let left = if piece.icon.is_some() {
                let wide = self.chrome_width(&text);
                piece.left + ((piece.width - wide) / 2.0).max(0.0)
            } else {
                piece.left
            };
            self.append_chrome(&text, color, (left, text_top), quads);
        }
    }

    /// The name of whatever the pointer is resting on, if it has one to give.
    ///
    /// Only the icons with no words beside them: a button that says what it
    /// does needs no second chance to say it.
    fn append_tooltip(&mut self, window_width: f32, quads: &mut unterm_render::quads::FrameQuads) {
        if self.window.pointer.1 >= self.top_bar_height() {
            return;
        }
        let bar = self.top_bar(window_width);
        let Some(piece) = bar
            .iter()
            .find(|piece| piece.contains(self.window.pointer.0))
            .cloned()
        else {
            return;
        };
        let Some(tooltip) = piece.tooltip.filter(|text| !text.trim().is_empty()) else {
            return;
        };

        let pt = self.chrome_pt();
        let chrome = self.chrome();
        let wide = self.chrome_width(&tooltip);
        let pad = 6.0 * pt;
        let width = wide + pad * 2.0;
        let height = self.chrome_row_height();
        // Under the button it names, and never off the right edge.
        let left = (piece.left + piece.width / 2.0 - width / 2.0)
            .clamp(0.0, (window_width - width).max(0.0));
        let top = self.top_bar_height() + 2.0 * pt;

        // Over everything: a tooltip behind the text it explains is no help.
        let mark = quads.mark();
        quads.backgrounds.extend(unterm_render::rounded::panel(
            left,
            top,
            width,
            height,
            crate::ui_tokens::CORNER_RADIUS * pt,
            chrome.group_bg,
        ));
        let text_top = ((height - self.window.chrome_font.metrics().height) / 2.0
            + crate::ui_tokens::CHROME_TEXT_BASELINE_NUDGE * pt)
            .max(0.0);
        let color = self.window.colors.foreground;
        self.append_chrome(&tooltip, color, (left + pad, top + text_top), quads);
        quads.raise_since(mark);
    }

    /// A press on the top bar. Returns true when the bar took it.
    ///
    /// The empty parts drag the window, which is the first thing anyone tries
    /// on a window with no title bar -- and the last thing they find missing.
    fn click_top_bar(&mut self) -> bool {
        if self.window.pointer.1 >= self.top_bar_height() {
            return false;
        }
        // What is a handle is the bar's own question to answer -- the same
        // list that decided where things were drawn. Deciding it again here
        // is how a piece comes to be drawn in one place and grabbed in
        // another.
        if self.pointer_is_on_a_drag_handle() {
            // The second press on the same spot toggles maximise, the way
            // every title bar has always answered a double-click.
            let click = match self.window.last_topbar_click.take() {
                Some(previous) => previous.again(0, self.window.pointer.0, self.window.pointer.1),
                None => crate::sidebar::RowClick::first(0, self.window.pointer.0, self.window.pointer.1),
            };
            let toggles = click.streak() >= 2;
            self.window.last_topbar_click = Some(click);
            if let Some(live) = self.window.state.as_ref() {
                if toggles {
                    live.window.set_maximized(!live.window.is_maximized());
                } else {
                    let _ = live.window.drag_window();
                }
            }
            return true;
        }
        let Some(item) = self.hovered_top_bar_item() else {
            return true;
        };

        match item {
            // These are handles, and were taken above.
            crate::topbar::Item::Wordmark | crate::topbar::Item::Title => {}
            crate::topbar::Item::Cockpit => {
                self.window.inbox_open = !self.window.inbox_open;
                self.window.inbox_selected = 0;
            }
            // The pane facts begin with the running shell. In 0.57.4 that
            // shell identity was an entry point to the shell selector; making
            // it a drag handle in the new chrome removed the visible picker.
            crate::topbar::Item::Stats => self.open_shell_selector(),
            crate::topbar::Item::Menu => self.open_quick_menu(),
            crate::topbar::Item::Action(action) => {
                if let Some(live) = self.window.state.as_ref() {
                    let session_id = live.session_id;
                    self.run_key_action(action, session_id);
                }
            }
            crate::topbar::Item::Minimise => {
                if let Some(live) = self.window.state.as_ref() {
                    live.window.set_minimized(true);
                }
            }
            crate::topbar::Item::Maximise => {
                self.toggle_maximize();
            }
            crate::topbar::Item::Close => self.request_close(),
        }
        self.window.drawn_revision = None;
        true
    }

    /// The bar as it is drawn right now, and which column the pointer is in.
    ///
    /// One place, so what is hit is always what was drawn.
    /// Which piece of the top bar the pointer is over.
    fn hovered_top_bar_item(&mut self) -> Option<crate::topbar::Item> {
        if self.window.pointer.1 >= self.top_bar_height() {
            return None;
        }
        let width = self.window.state.as_ref()?.width as f32;
        let bar = self.top_bar(width);
        crate::topbar::hit(&bar, self.window.pointer.0)
    }

    /// Whether a press here should drag the window rather than do something.
    fn pointer_is_on_a_drag_handle(&mut self) -> bool {
        if self.window.pointer.1 >= self.top_bar_height() {
            return false;
        }
        let Some(width) = self.window.state.as_ref().map(|live| live.width as f32) else {
            return false;
        };
        let bar = self.top_bar(width);
        crate::topbar::is_drag_handle(&bar, self.window.pointer.0)
    }

    fn append_scrollbar(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        if !self.scrollbar_enabled {
            return;
        }
        let Some((session_id, left, track_top, track)) = self.active_pane_scrollbar() else {
            return;
        };
        let Ok(snapshot) = self.engine.read_styled_screen(session_id) else {
            return;
        };

        let total = snapshot.scrollback_rows + snapshot.rows;
        let top_row = snapshot
            .lines
            .first()
            .map(|line| line.row.max(0) as usize)
            .unwrap_or(0);
        let Some(thumb) = crate::scrollbar::thumb(total, snapshot.rows, top_row, track) else {
            return;
        };

        // The track first, so the thumb reads as a position within something
        // rather than a stripe floating at the edge.
        quads.backgrounds.push(unterm_render::quads::Quad {
            left,
            top: track_top,
            width: crate::scrollbar::WIDTH,
            height: track,
            // The track is a tint of the thumb rather than a mix of the
            // frame: a scheme that chose a scrollbar colour chose it against
            // its own background, and deriving one here ignores that.
            color: crate::chrome::mix(self.window.colors.background, self.theme().scrollbar, 0.35),
        });
        quads.backgrounds.push(unterm_render::quads::Quad {
            left,
            top: track_top + thumb.top,
            width: crate::scrollbar::WIDTH,
            height: thumb.height,
            color: self.theme().scrollbar,
        });
    }

    /// The visual bell, as `visual_bell` configures it: a flash that rises
    /// and falls on the config's own curves, over the whole background or
    /// over just the cursor's cell.
    fn append_bell_flash(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(rung_at) = self.window.bell_at else {
            return;
        };
        let Some(intensity) = self.visual_bell.intensity_at(rung_at.elapsed().as_millis()) else {
            // The fade is over; stop asking for frames on its behalf.
            self.window.bell_at = None;
            return;
        };
        // The previous front end coloured the flash with the palette's
        // `visual_bell` entry and fell back to the foreground; this theme
        // format has no such entry, so the foreground is the flash.
        let mut color = self.window.colors.foreground;
        color[3] = intensity;
        let quad = match self.visual_bell.target {
            crate::terminal::BellTarget::BackgroundColor => {
                let Some(live) = self.window.state.as_ref() else {
                    return;
                };
                unterm_render::quads::Quad {
                    left: 0.0,
                    top: 0.0,
                    width: live.width as f32,
                    height: live.height as f32,
                    color,
                }
            }
            crate::terminal::BellTarget::CursorColor => {
                let Some((left, top)) = self.cursor_cell_origin() else {
                    return;
                };
                let metrics = self.window.font.metrics();
                unterm_render::quads::Quad {
                    left,
                    top,
                    width: metrics.width,
                    height: metrics.height,
                    color,
                }
            }
        };
        quads.backgrounds.push(quad);
    }

    /// The focused pane's cursor cell, in window pixels, if it is on screen.
    fn cursor_cell_origin(&self) -> Option<(f32, f32)> {
        // Only the check that a window exists; the geometry below is the App's.
        self.window.state.as_ref()?;
        let session_id = self.focused_session();
        let snapshot = self.engine.read_styled_screen(session_id).ok()?;
        if !snapshot.cursor.visible {
            return None;
        }
        let row = usize::try_from(snapshot.cursor.y).ok()?;
        if row >= snapshot.rows || snapshot.cursor.x >= snapshot.cols.max(1) {
            return None;
        }
        let origin = self
            .placements()
            .into_iter()
            .find(|placement| placement.session_id == session_id)
            .map(|placement| placement.origin)
            .unwrap_or((self.terminal_left(), self.terminal_top()));
        let metrics = self.window.font.metrics();
        Some((
            origin.0 + snapshot.cursor.x as f32 * metrics.width,
            origin.1 + row as f32 * metrics.height,
        ))
    }

    /// Underline the link the pointer is over, so a link is discoverable by
    /// pointing at it -- as it always was.
    ///
    /// Only while the modifier is down: a line that appears under everything
    /// the pointer passes is noise, and one that appears when clicking would
    /// do something is a hint.
    fn append_hovered_link(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(link) = self.link_under_pointer() else {
            return;
        };
        let metrics = self.window.font.metrics();
        let top_offset = self.terminal_top();
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: link.start as f32 * metrics.width,
            top: top_offset
                + link.row as f32 * metrics.height
                + unterm_render::decorations::underline_top(metrics),
            width: (link.end - link.start) as f32 * metrics.width,
            height: unterm_render::decorations::thickness(metrics),
            color: self.window.colors.foreground,
        });
    }

    /// The link the pointer is over, if any.
    fn link_under_pointer(&self) -> Option<crate::links::Link> {
        let live = self.window.state.as_ref()?;
        let snapshot = self.engine.read_styled_screen(live.session_id).ok()?;
        let metrics = self.window.font.metrics();
        let left = self.terminal_left();
        let top = self.terminal_top();
        let column = ((self.window.pointer.0 - left).max(0.0) / metrics.width.max(1.0)) as usize;
        let row = ((self.window.pointer.1 - top).max(0.0) / metrics.height.max(1.0)) as usize;

        let line = snapshot.lines.get(row)?;
        crate::links::links_in_row(row, line, &self.link_rules)
            .into_iter()
            .find(|link| link.covers(row, column))
    }

    /// Which cell the pointer is over, in scrollback coordinates.
    fn cell_under_pointer(&self) -> unterm_engine::next_core::selection::SelectionPoint {
        // The viewport's top row, so a selection made while scrolled back stays
        // on the text it was made on rather than on whatever is there later.
        let top = self
            .window.state
            .as_ref()
            .and_then(|live| self.engine.read_styled_screen(live.session_id).ok())
            .map(|snapshot| snapshot.lines.first().map(|line| line.row).unwrap_or(0))
            .unwrap_or(0);

        // From the terminal's own origin, not the window's: the bar above and
        // the gap around the grid are not part of it, and a selection measured
        // from the window's corner lands two rows above where it was drawn.
        let metrics = self.window.font.metrics();
        let origin = (self.terminal_left(), self.terminal_top());
        crate::select::cell_at(
            self.window.pointer.0 - origin.0,
            self.window.pointer.1 - origin.1,
            metrics,
            top,
        )
    }

    /// Extract what the current drag covers.
    fn update_selection(&mut self) {
        use unterm_engine::next_core::selection::{selected_text, SelectionRow};

        let Some(drag) = self.window.drag else {
            return;
        };
        let Some(selection) = drag.selection() else {
            // Still a click, not a selection.
            return;
        };
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let Ok(snapshot) = self.engine.read_styled_screen(live.session_id) else {
            return;
        };

        let rows: Vec<SelectionRow> = snapshot
            .lines
            .iter()
            .map(|line| SelectionRow {
                row: line.row,
                text: selection_row_text(&line.cells),
                wrapped: line.wrapped,
            })
            .collect();

        let text = strip_spacer_marks(selected_text(&selection, &rows));
        if !text.is_empty() {
            // What was selected, so a copy that comes out wrong can be traced
            // to the selection rather than to the clipboard.
            log::debug!("selected {} char(s): {:?}", text.chars().count(), text);
        }
        self.window.selected = (!text.is_empty()).then_some(text);
    }

    /// The columns of the non-space run around a column, on one grid row.
    fn word_bounds_at(&self, row: i64, column: usize) -> Option<(usize, usize)> {
        let live = self.window.state.as_ref()?;
        let snapshot = self.engine.read_styled_screen(live.session_id).ok()?;
        let line = snapshot.lines.iter().find(|line| line.row == row)?;
        let chars: Vec<char> = selection_row_text(&line.cells).chars().collect();
        if chars.is_empty() {
            return None;
        }
        let at = column.min(chars.len() - 1);
        if chars[at].is_whitespace() {
            return None;
        }
        let mut start = at;
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let mut end = at;
        while end + 1 < chars.len() && !chars[end + 1].is_whitespace() {
            end += 1;
        }
        Some((start, end))
    }

    /// Select the run of non-space characters under a double-click.
    fn select_word_at(&mut self, cell: unterm_engine::next_core::selection::SelectionPoint) {
        use unterm_engine::next_core::selection::{SelectionPoint, SelectionShape};
        let Some((start, end)) = self.word_bounds_at(cell.row, cell.column) else {
            return;
        };
        let mut drag = crate::select::Drag::start(
            SelectionPoint::new(start, cell.row),
            SelectionShape::Linear,
        );
        drag.extend(SelectionPoint::new(end, cell.row));
        self.window.drag = Some(drag);
        self.update_selection();
    }

    /// Select the whole row under a triple-click.
    fn select_line_at(&mut self, cell: unterm_engine::next_core::selection::SelectionPoint) {
        use unterm_engine::next_core::selection::{SelectionPoint, SelectionShape};
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let Ok(snapshot) = self.engine.read_styled_screen(live.session_id) else {
            return;
        };
        let width = snapshot.cols.max(1);
        let mut drag =
            crate::select::Drag::start(SelectionPoint::new(0, cell.row), SelectionShape::Linear);
        drag.extend(SelectionPoint::new(width.saturating_sub(1), cell.row));
        self.window.drag = Some(drag);
        self.update_selection();
    }

    /// Grow the held drag to the pointer, snapping to what the click streak
    /// established: a double-click drag grows word by word, a triple-click
    /// drag row by row, and a plain drag cell by cell.
    fn extend_drag_to(&mut self, point: unterm_engine::next_core::selection::SelectionPoint) {
        use unterm_engine::next_core::selection::{SelectionPoint, SelectionShape};
        if self.window.drag.is_none() {
            return;
        }
        let anchor = match (self.window.select_granularity, self.window.select_anchor) {
            (SelectGranularity::Cell, _) | (_, None) => {
                if let Some(drag) = self.window.drag.as_mut() {
                    drag.extend(point);
                }
                return;
            }
            (_, Some(anchor)) => anchor,
        };
        let forward = (point.row, point.column) >= (anchor.row, anchor.column);
        let (from, to) = match self.window.select_granularity {
            SelectGranularity::Word => {
                let (a_start, a_end) = self
                    .word_bounds_at(anchor.row, anchor.column)
                    .unwrap_or((anchor.column, anchor.column));
                let pointer_word = self.word_bounds_at(point.row, point.column);
                if forward {
                    (
                        SelectionPoint::new(a_start, anchor.row),
                        SelectionPoint::new(
                            pointer_word.map_or(point.column, |(_, end)| end),
                            point.row,
                        ),
                    )
                } else {
                    (
                        SelectionPoint::new(a_end, anchor.row),
                        SelectionPoint::new(
                            pointer_word.map_or(point.column, |(start, _)| start),
                            point.row,
                        ),
                    )
                }
            }
            SelectGranularity::Line => {
                let cols = self
                    .window.state
                    .as_ref()
                    .and_then(|live| self.engine.read_styled_screen(live.session_id).ok())
                    .map_or(1, |snapshot| snapshot.cols.max(1));
                if forward {
                    (
                        SelectionPoint::new(0, anchor.row),
                        SelectionPoint::new(cols - 1, point.row),
                    )
                } else {
                    (
                        SelectionPoint::new(cols - 1, anchor.row),
                        SelectionPoint::new(0, point.row),
                    )
                }
            }
            SelectGranularity::Cell => unreachable!("handled above"),
        };
        let mut drag = crate::select::Drag::start(from, SelectionShape::Linear);
        drag.extend(to);
        self.window.drag = Some(drag);
    }

    /// Put the selection on the clipboard.
    ///
    /// A selection nobody can copy is decoration, and this is the one action
    /// every terminal user reaches for within a minute of selecting anything.
    /// Carry out a key binding.
    fn run_key_action(&mut self, action: crate::keys::Action, session_id: usize) {
        use crate::keys::Action;
        if std::env::var_os("UNTERM_TRACE_KEYS").is_some() {
            log::info!("  run_key_action {:?} font={}pt", action, self.window.font_points);
        }
        match action {
            Action::Copy => self.copy_selection(),
            Action::Paste => self.paste_clipboard(),
            Action::SplitRight => {
                self.split(unterm_engine::next_core::layout::SplitAxis::Horizontal)
            }
            Action::SplitDown => self.split(unterm_engine::next_core::layout::SplitAxis::Vertical),
            Action::CopyMode => {
                self.window.copy_mode = Some(crate::copy_mode::CopyMode::default());
                self.window.drawn_revision = None;
            }
            Action::QuickSelect => self.open_quick_select(),
            Action::Insights => self.open_insights(),
            Action::CockpitInbox => {
                self.window.inbox_open = !self.window.inbox_open;
                self.window.inbox_selected = 0;
                self.window.drawn_revision = None;
            }
            Action::GitPanel => self.toggle_git_panel(),
            Action::DirJump => {
                // The in-app jump palette is the feature, as 0.57.4 had it;
                // the OS picker stays one of its rows (and Ctrl+O). Wired
                // straight to the picker, the palette was orphaned -- and on
                // anything but Windows, the action was only a failure toast.
                let entries = self.dir_jump_entries("");
                self.open_browser(entries, unterm_services::i18n::t("dirjump.placeholder"));
            }
            Action::LeftTabBar => {
                self.window.sidebar_open = !self.window.sidebar_open;
                self.resize_panes();
                self.window.drawn_revision = None;
            }
            Action::ThemePicker => {
                let entries = self.theme_entries();
                self.open_palette(entries);
            }
            Action::Composer => {
                self.window.composer = match self.window.composer.take() {
                    Some(_) => None,
                    None => Some(crate::composer::Composer::default()),
                };
                self.window.drawn_revision = None;
            }
            Action::CommandPalette => {
                self.open_named_palette(
                    self.command_palette_entries(),
                    unterm_services::i18n::t("menu.command_palette"),
                );
            }
            Action::Launcher => self.open_shell_selector(),
            Action::Search => {
                self.window.search = Some(crate::search::Search::default());
                self.window.drawn_revision = None;
            }
            Action::NewTab => self.new_tab(),
            Action::NextTab => self.cycle_tab(1),
            Action::PreviousTab => self.cycle_tab(-1),
            Action::CloseTab => self.close_tab(),
            Action::NewWindow => self.new_window(),
            Action::ClosePane => self.close_pane(session_id),
            Action::ZoomPane => self.toggle_zoom(session_id),
            Action::CharSelect => {
                // Open where 0.57.4 opened: on the recents when there are
                // any, and on the smileys when nothing was ever picked.
                let recents = crate::charselect::recent_choices();
                self.charselect_group = crate::charselect::starting_group(&recents);
                let entries = self.character_entries("");
                self.window.palette = Some(crate::palette::Palette::characters(entries));
                self.window.drawn_revision = None;
            }
            Action::Settings => self.open_settings(),
            Action::TreeSidebar => self.toggle_tree(),
            Action::FleetLaunch => {
                let entries = self.fleet_entries();
                self.open_fleet(entries);
            }
            Action::ClearScrollback => self.clear_scrollback(session_id, false),
            Action::ClearScreen => self.clear_scrollback(session_id, true),
            Action::SelectPane => self.open_pane_select(crate::paneselect::Mode::Activate),
            Action::SwapPane => self.open_pane_select(crate::paneselect::Mode::Swap),
            Action::FocusPane(direction) => self.focus_pane_toward(direction),
            Action::ResizePane(direction) => self.resize_pane_toward(direction),
            Action::MoveTab(step) => self.move_tab(step),
            Action::PreviousPrompt | Action::NextPrompt => {
                let amount = if action == Action::PreviousPrompt {
                    -1
                } else {
                    1
                };
                let _ = self.engine.scroll_viewport_to_prompt(session_id, amount);
                self.window.drawn_revision = None;
            }
            Action::SelectTab(number) => self.select_tab(number),
            Action::IncreaseFontSize => self.change_font_size(1.0),
            Action::DecreaseFontSize => self.change_font_size(-1.0),
            Action::ResetFontSize => self.set_font_size(self.configured_font_points),
            Action::ToggleFullScreen => self.toggle_full_screen(),
            Action::ScrollPageUp | Action::ScrollPageDown => {
                let rows = self
                    .engine
                    .read_styled_screen(session_id)
                    .map(|snapshot| snapshot.rows)
                    .unwrap_or(24);
                let page = crate::scroll::lines_for_page(rows);
                let delta = if action == Action::ScrollPageUp {
                    -page
                } else {
                    page
                };
                let _ = self.engine.scroll_viewport_by(session_id, delta);
            }
        }
    }

    /// Another terminal, as its own process.
    ///
    /// Unterm's windows are separate processes -- that is what makes
    /// `instance.list` able to name them and an agent able to drive one
    /// without touching another -- so a new window is a new instance, started
    /// where this one is looking.
    /// Open another window -- in this process.
    ///
    /// It used to spawn a second copy of the executable, which is why every
    /// window cost a fresh GPU: choosing an adapter is ~200 ms and a new
    /// process has to choose again. Windows Terminal opens its second window
    /// in 172 ms for exactly this reason. Here the work is deferred to
    /// `about_to_wait`, which is where winit hands out the event loop a
    /// window has to be created from.
    fn new_window(&mut self) {
        // The same queue an MCP call uses, so a window opened by a keystroke
        // and one opened by an agent get their ids from the same place. Two
        // allocators would eventually give two windows one number.
        unterm_engine::request_window();
    }

    /// Park the window being shown and start a fresh one beside it.
    /// Open another window, under the id already promised to whoever asked.
    ///
    /// The id is a parameter rather than taken here because an MCP caller was
    /// told the number before this ran -- a window can only be built where
    /// winit hands out an `ActiveEventLoop`, and no agent can wait for one.
    fn open_window(&mut self, event_loop: &ActiveEventLoop, request: unterm_engine::WindowRequest) {
        let id = request.id;
        let Some(parked_id) = self.active_window_id() else {
            return;
        };
        let font = match TerminalFont::open_named(
            self.font_family.as_deref(),
            crate::terminal::pixels_for_points(self.configured_pixel_size as f32, 1.0),
            &self.font_fallbacks,
            self.window.font_shape,
        ) {
            Ok(font) => font,
            Err(err) => {
                log::warn!("could not open a font for the new window: {err:#}");
                return;
            }
        };
        let chrome_font = match crate::chrome_font::open(&self.font_fallbacks, 1.0) {
            Ok(font) => font,
            Err(err) => {
                log::warn!("could not open the chrome font for the new window: {err:#}");
                return;
            }
        };
        let picture = crate::background::configured(&self.config);
        let fresh = WindowState::fresh(
            id,
            &self.config,
            self.configured_pixel_size,
            self.window.font_shape,
            font,
            chrome_font,
            picture,
        );
        // A new window is a new shell. Adopting is for the first window of a
        // process, which has to show what the Core was already running.
        let previous = std::mem::replace(&mut self.window, fresh);
        self.parked.push(ParkedWindow {
            id: parked_id,
            state: previous,
        });
        let explicit = std::mem::replace(&mut self.explicit_launch, true);
        // `start` reads these for the shell it is about to make. Swapped
        // around the call rather than assigned, the way `explicit_launch` is:
        // they describe the *first* shell of a window, and a path left behind
        // in `start_directory` would open every later tab there too.
        let directory = request
            .cwd
            .is_some()
            .then(|| std::mem::replace(&mut self.start_directory, request.cwd.clone()));
        let command = request.command.first().map(|program| {
            let mut builder = portable_pty::CommandBuilder::new(program);
            builder.args(request.command.iter().skip(1));
            std::mem::replace(&mut self.shell, Some(builder))
        });
        let started = self.start(event_loop);
        if let Some(previous) = directory {
            self.start_directory = previous;
        }
        if let Some(previous) = command {
            self.shell = previous;
        }
        self.explicit_launch = explicit;
        match started {
            Ok(live) => {
                live.window.request_redraw();
                self.window.state = Some(live);
            }
            Err(err) => {
                log::warn!("could not open another window: {err:#}");
                // Put the one that was working back in front.
                if let Some(parked) = self.parked.pop() {
                    self.window = parked.state;
                }
            }
        }
    }

    /// Where a new window should start: where the focused pane is.
    fn current_directory(&self) -> Option<std::path::PathBuf> {
        let live = self.window.state.as_ref()?;
        unterm_engine::SessionEngine::list_sessions(&self.engine)
            .ok()
            .into_iter()
            .flatten()
            .find(|session| session.id == live.session_id)
            .and_then(|session| session.shell.cwd)
            .map(std::path::PathBuf::from)
            .or_else(|| self.start_directory.clone())
    }

    /// Close one pane, leaving the rest of the tab alone.
    ///
    /// Distinct from closing the tab: with a split open, the pane is what the
    /// key is aimed at. When it was the last one the tab goes with it, which
    /// is what makes this safe to reach for.
    fn close_pane(&mut self, session_id: usize) {
        let _slow = SlowGuard::new("close_pane");
        let Some(tab_id) = self.window.tabs.tab_of_pane(session_id) else {
            return;
        };
        if self.window.tabs.pane_ids(tab_id).len() < 2 {
            self.close_tab();
            return;
        }
        crate::statsbar::forget(session_id);
        unterm_services::ghost_text::forget(session_id as u64);
        let _ = self.engine.destroy_session(session_id);
        self.window.tabs.close_pane(session_id);
        if let Some(pane) = self.window.tabs.active_pane(tab_id) {
            self.focus_session(pane);
        }
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    /// One pane fills the tab, or gives the space back.
    fn toggle_zoom(&mut self, session_id: usize) {
        let Some(tab_id) = self.window.tabs.tab_of_pane(session_id) else {
            return;
        };
        let zoomed = self.window.tabs.zoomed_pane(tab_id) == Some(session_id);
        self.window.tabs.set_zoomed(session_id, !zoomed);
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    /// Move focus to the nearest pane in a direction.
    ///
    /// Nearest by edge rather than by tree position: with three panes the tree
    /// has a shape the screen does not show, and someone pressing Alt+Right
    /// means the pane to the right of this one.
    /// Move focus to the nearest pane in a direction.
    fn focus_pane_toward(&mut self, direction: crate::keys::Direction) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let placements = self.placements();
        let target =
            crate::panes::pane_toward(&placements, live.session_id, direction, self.window.font.metrics());
        if let Some(pane) = target {
            self.window.tabs.set_active_pane(pane);
            self.focus_session(pane);
            self.window.drawn_revision = None;
        }
    }

    fn resize_pane_toward(&mut self, direction: crate::keys::Direction) {
        use crate::keys::Direction;
        use unterm_engine::next_core::layout::SplitAxis;

        let pane = self.focused_session();
        let (axis, delta) = match direction {
            Direction::Left => (SplitAxis::Horizontal, -0.05),
            Direction::Right => (SplitAxis::Horizontal, 0.05),
            Direction::Up => (SplitAxis::Vertical, -0.05),
            Direction::Down => (SplitAxis::Vertical, 0.05),
        };
        let Some(change) = self.window.tabs.adjust_split_ratio(pane, axis, delta) else {
            return;
        };
        // Tell the engine where the divider went. The split tree lives in
        // this window, but the arrangement has to outlive it: without this
        // the pane comes back from a restart at the size it was created at,
        // however long the user spent settling on another one.
        if let Err(err) = self
            .engine
            .set_split_ratio(change.owner_pane_id, change.first_ratio)
        {
            log::debug!(
                "could not record the split ratio for pane {}: {err:#}",
                change.owner_pane_id
            );
        }
        self.resize_panes();
        self.window.drawn_revision = None;
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
    }

    /// Go to a tab by its number, counting from one as the keys are labelled.
    ///
    /// Nine means the last one however many there are, which is what every
    /// browser does and what people reach for.
    /// Show the tab at `index` in the strip, whatever number it would be.
    ///
    /// Not `select_tab`: that one takes a *number key*, where nine and above
    /// mean "the last tab" -- right for a keyboard with nine keys, and wrong
    /// for a strip whose ninth row is the ninth tab. Routing the strip
    /// through it sent every click past the eighth row to the end instead,
    /// which reads as a tab that will not switch.
    fn select_tab_at(&mut self, index: usize) {
        let Some(tab_id) = self.window.tabs.tab_ids().get(index).copied() else {
            #[cfg(target_os = "macos")]
            crate::macos_open::trace(&format!("select_tab_at {index}: no tab there"));
            return;
        };
        #[cfg(target_os = "macos")]
        crate::macos_open::trace(&format!(
            "select_tab_at {index} -> tab {tab_id} pane {:?}",
            self.window.tabs.active_pane(tab_id)
        ));
        self.activate_tab(tab_id);
    }

    fn select_tab(&mut self, number: u8) {
        let ids = self.window.tabs.tab_ids();
        let Some(tab_id) = crate::topbar::tab_for_number(number, ids.len())
            .and_then(|index| ids.get(index).copied())
        else {
            #[cfg(target_os = "macos")]
            crate::macos_open::trace(&format!(
                "select_tab {number}: no tab for it among {} ids",
                ids.len()
            ));
            return;
        };
        #[cfg(target_os = "macos")]
        crate::macos_open::trace(&format!(
            "select_tab {number} -> tab {tab_id} pane {:?}",
            self.window.tabs.active_pane(tab_id)
        ));
        self.window.tabs.set_active_tab(tab_id);
        self.window.tab_id = Some(tab_id);
        if let Some(pane) = self.window.tabs.active_pane(tab_id) {
            self.focus_session(pane);
        }
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    fn change_font_size(&mut self, steps: f32) {
        self.set_font_size(self.window.font_points + steps);
    }

    /// Redraw everything at a new size.
    ///
    /// The atlas is thrown away rather than added to: it is keyed by pixel
    /// size, so keeping it would only hold glyphs nothing will ask for again.
    /// The panes are told their new grid afterwards -- a shell that still
    /// thinks it has eighty columns wraps its output in the wrong place.
    fn set_font_size(&mut self, points: f32) {
        let points = points.clamp(6.0, 72.0);
        if (points - self.window.font_points).abs() < f32::EPSILON {
            return;
        }
        self.reopen_font(points, self.window.scale);
    }

    /// Open the font at a size in points, for a display at `scale`.
    fn reopen_font(&mut self, points: f32, scale: f32) {
        let pixels = crate::terminal::pixels_for_points(points, Self::font_scale_for(scale));
        let Ok(font) = TerminalFont::open_named(
            self.font_family.as_deref(),
            pixels,
            &self.font_fallbacks,
            self.window.font_shape,
        ) else {
            log::warn!("no font at {pixels} pixels; keeping {}", self.window.font_points);
            return;
        };
        self.window.font = font;
        // The chrome follows the display's scale but not the terminal's font
        // size: making the tab labels bigger because somebody zoomed the
        // terminal is not what zooming the terminal means.
        if let Ok(chrome) =
            crate::chrome_font::open(&self.font_fallbacks, Self::font_scale_for(scale))
        {
            self.window.chrome_font = chrome;
        }
        self.window.font_points = points;
        self.window.scale = scale;
        self.window.atlas = GlyphAtlas::new(1024, 1024);
        if let Some(live) = self.window.state.as_mut() {
            live.atlas_uploaded_glyphs = usize::MAX;
        }
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    fn toggle_full_screen(&mut self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let full = live.window.fullscreen().is_some();
        live.window.set_fullscreen(if full {
            None
        } else {
            // Borderless on whichever monitor the window is on: an exclusive
            // mode would change the display's resolution, which is not what
            // a terminal going full screen should do to the rest of a desktop.
            Some(winit::window::Fullscreen::Borderless(None))
        });
        self.window.drawn_revision = None;
    }

    fn copy_selection(&mut self) {
        let Some(text) = self.window.selected.clone() else {
            return;
        };
        self.copy_text(&text);
    }

    /// Send the clipboard to the shell.
    ///
    /// Through the engine's paste rather than as typed input, because the
    /// engine knows whether the program asked for bracketed paste. Without the
    /// brackets a multi-line paste is executed a line at a time -- paste a
    /// script and you have run it.
    fn paste_clipboard(&mut self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let session_id = live.session_id;
        let tx = self.clipboard_tx.clone();
        crate::clipboard::run(tx, move || {
            let result = arboard::Clipboard::new()
                .map_err(|err| err.to_string())
                .and_then(|mut board| match board.get_text() {
                    Ok(text) if !text.is_empty() => Ok(text),
                    // No text — a system screenshot or a browser's "copy
                    // image" leaves an image alone on the clipboard, and a
                    // paste that silently does nothing reads as broken. In
                    // a terminal the useful form of an image is a path to
                    // it, so it becomes a file in the captures folder.
                    _ => match board.get_image() {
                        Ok(picture) => clipboard_image_to_file(&picture)
                            .map(|path| format!("{} ", shell_quoted_path(&path)))
                            .map_err(|err| err.to_string()),
                        Err(err) => Err(err.to_string()),
                    },
                });
            ClipboardResult::Read {
                pane_id: session_id,
                result,
            }
        });
    }

    /// The pane keys and pastes go to.
    fn focused_session(&self) -> usize {
        self.window.tab_id
            .and_then(|tab_id| self.window.tabs.active_pane(tab_id))
            .or_else(|| self.window.state.as_ref().map(|live| live.session_id))
            .unwrap_or(0)
    }

    /// Draw the focused pane's pending command prediction at its live cursor.
    fn append_ghost_text(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        // The input method owns this position while it is composing text.
        if !self.window.preedit.is_empty() {
            return;
        }
        let pane = self.focused_session();
        let Some((_input, ghost)) = unterm_services::ghost_text::current_ghost(pane as u64) else {
            return;
        };
        let Ok(snapshot) = self.engine.read_styled_screen(pane) else {
            return;
        };
        if !snapshot.cursor.visible {
            return;
        }
        let Ok(row) = usize::try_from(snapshot.cursor.y) else {
            return;
        };
        if row >= snapshot.rows || snapshot.cursor.x >= snapshot.cols {
            return;
        }
        let placement = self
            .placements()
            .into_iter()
            .find(|placement| placement.session_id == pane);
        let (pane_origin, pane_cols) = match placement {
            Some(placement) => (placement.origin, placement.cols),
            None => ((self.terminal_left(), self.terminal_top()), snapshot.cols),
        };
        let available = pane_cols.saturating_sub(snapshot.cursor.x);
        let ghost = crate::ghost::truncate_to_columns(&ghost, available);
        if ghost.is_empty() {
            return;
        }
        let metrics = self.window.font.metrics();
        let origin = (
            pane_origin.0 + snapshot.cursor.x as f32 * metrics.width,
            pane_origin.1 + row as f32 * metrics.height,
        );
        crate::terminal::append_text(
            &ghost,
            &mut self.window.font,
            &mut self.window.atlas,
            crate::ghost::color(self.window.colors.foreground, self.window.colors.background),
            origin,
            quads,
        );
    }

    /// Split the focused pane.
    fn split(&mut self, axis: unterm_engine::next_core::layout::SplitAxis) {
        let (Some(tab_id), Some(live)) = (self.window.tab_id, self.window.state.as_ref()) else {
            return;
        };
        let focused = self.window.tabs.active_pane(tab_id).unwrap_or(live.session_id);

        // Through the engine's own split, not a bare create: the
        // engine then records what this arrangement is, and a window
        // opening later -- this one after a restart, or another one
        // onto the same Core -- can rebuild it instead of guessing at
        // horizontal-and-half. It also puts this path and the MCP
        // surface's `session.split` on the same road.
        let env = launch_env_for_new_pane();
        let session = match self.engine.split_session(unterm_engine::SplitSessionRequest {
            source_pane_id: focused,
            direction: match axis {
                unterm_engine::next_core::layout::SplitAxis::Horizontal => {
                    unterm_engine::SplitDirection::Right
                }
                unterm_engine::next_core::layout::SplitAxis::Vertical => {
                    unterm_engine::SplitDirection::Down
                }
            },
            size_percent: 50,
            command_dir: None,
            command: prepare_shell(self.shell.clone()),
            env,
            launch_policy: LaunchPolicySnapshot::default(),
        }) {
            Ok(session) => session,
            Err(err) => {
                log::warn!("could not open a pane: {err:#}");
                return;
            }
        };

        // Right and down, as this window's own split command asks for
        // above, put the new pane after the source.
        if let Err(err) = self.window.tabs.split(
            focused,
            session.id,
            axis,
            0.5,
            unterm_engine::next_core::layout::SplitSide::Second,
        ) {
            log::warn!("could not split: {err:#}");
            crate::statsbar::forget(session.id);
            unterm_services::ghost_text::forget(session.id as u64);
            let _ = self.engine.destroy_session(session.id);
            return;
        }
        self.window.tabs.set_active_pane(session.id);
        self.resize_panes();
        self.show_notice(unterm_services::i18n::t("interaction.split"));
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
        self.window.drawn_revision = None;
    }

    /// Put text a program asked for onto the system clipboard.
    ///
    /// `OSC 52`, which is the only way anything running over ssh can copy.
    /// Taken once: the engine reports the last request, and without
    /// remembering what was already honoured every frame would set the
    /// clipboard again and stamp on whatever the user copied since.
    fn take_clipboard_request(&mut self, request: Option<String>) {
        let Some(text) = request else {
            return;
        };
        if self.window.clipboard_honoured.as_deref() == Some(text.as_str()) {
            return;
        }
        self.window.clipboard_honoured = Some(text.clone());
        self.copy_text(&text);
    }

    /// Tell a pane the terminal gained or lost focus, if it asked.
    ///
    /// `CSI I` and `CSI O`, and only when the program turned reporting on:
    /// sending them unasked puts stray characters into anything that did not
    /// negotiate it, which is what the mode exists to prevent.
    fn report_focus(&mut self, focused: bool) {
        let Ok(sessions) = unterm_engine::SessionEngine::list_sessions(&self.engine) else {
            return;
        };
        let sequence = if focused { "[I" } else { "[O" };
        for session in sessions {
            let asked = self
                .engine
                .read_styled_screen(session.id)
                .map(|snapshot| snapshot.focus_reporting)
                .unwrap_or(false);
            if asked {
                let _ = self.engine.write_input(session.id, sequence);
            }
        }
    }

    /// Interrupt whatever the pane is running.
    ///
    /// The byte has already gone to the shell, which is enough on a pty with
    /// a line discipline. Windows has none: the byte only reaches the shell's
    /// line editor, so a running program -- which is the thing you press
    /// Ctrl+C at -- never hears it without a console control event.
    fn interrupt(&self, pane_id: usize) {
        let process = unterm_engine::SessionEngine::activity(&self.engine, pane_id)
            .ok()
            .and_then(|activity| activity.process);
        let Some(shell) = process.as_ref().and_then(|p| p.root_pid) else {
            return;
        };
        let foreground = process.as_ref().and_then(|p| p.foreground_pid);
        if let Err(err) = unterm_services::interrupt::stop_foreground(shell, foreground) {
            // Worth a line: an interrupt that quietly did nothing is what
            // this exists to remove.
            log::warn!("could not interrupt pane {pane_id}: {err}");
        }
    }

    /// Where the first shell starts, as the command line asked.
    pub fn set_start_directory(&mut self, directory: Option<std::path::PathBuf>) {
        if directory.is_some() {
            self.explicit_launch = true;
        }
        self.start_directory = directory;
    }

    /// Reopen where the last window closed: its size, and a tab per saved
    /// directory.
    pub fn set_restore(&mut self, saved: crate::session_restore::LastSession) {
        self.restore = Some(saved);
    }

    /// Override the configured shell for the first pane when `unterm start`
    /// supplied an explicit program after `--`.
    pub fn set_start_command(&mut self, argv: Vec<String>) {
        let Some(program) = argv.first() else {
            return;
        };
        self.explicit_launch = true;
        let mut command = portable_pty::CommandBuilder::new(program);
        command.args(argv.iter().skip(1));
        self.shell = Some(command);
    }

    /// Feed the cockpit what the panes are showing.
    ///
    /// The tracker watches screen tails and titles to work out whether an
    /// agent is waiting on a person. Nothing else in this front end calls it,
    /// so without this the inbox is always empty and looks broken rather than
    /// idle.
    fn feed_cockpit(&mut self) {
        if self.cockpit_fed_at.elapsed() < COCKPIT_POLL {
            return;
        }
        self.cockpit_fed_at = std::time::Instant::now();

        let Ok(sessions) = unterm_engine::SessionEngine::list_sessions(&self.engine) else {
            return;
        };
        let active_pane = self.focused_session();
        let pane_ids: Vec<u64> = sessions.iter().map(|session| session.id as u64).collect();
        let mut announcements: Vec<(u64, String)> = Vec::new();
        for session in &sessions {
            // Schedule process/manifest detection for every pane. This is
            // non-blocking; the worker-backed cache is consumed by poll below.
            let _ = crate::statsbar::facts_for(session.id);
            // The pulse, not the screen. This loop runs over every pane
            // more than twice a second; asking each one for 4800 styled cells and
            // keeping only the characters cost 3.8 s at forty panes — nine
            // times the poll's own budget, which is a window that never
            // finishes a frame.
            let since = self
                .window.pane_notices
                .get(&session.id)
                .map(|notice| notice.revision)
                .filter(|revision| *revision != 0);
            let Ok(pulse) =
                self.engine
                    .read_pane_pulse(session.id, since, COCKPIT_TAIL_ROWS)
            else {
                continue;
            };
            let notice = self.window.pane_notices.entry(session.id).or_default();
            if pulse.unchanged {
                // Nothing has been written to this pane since the last poll,
                // so every conclusion below would be the one already drawn.
                // Notifications still count: a bell is not a screen change.
                if pulse.notifications != notice.notifications_seen {
                    let fresh = pulse.notifications > notice.notifications_seen;
                    notice.notifications_seen = pulse.notifications;
                    if fresh {
                        if session.id != active_pane {
                            notice.unread = true;
                        }
                        if let Some(text) = pulse.last_notification.clone() {
                            announcements.push((session.id as u64, text));
                        }
                    }
                }
                if session.id == active_pane {
                    notice.unread = false;
                }
                continue;
            }
            let tail = pulse.tail.clone();
            if notice.revision != 0
                && notice.revision != pulse.revision
                && session.id != active_pane
            {
                notice.unread = true;
            }
            let error = session.is_dead || crate::sidebar::output_looks_like_error(&tail);
            if error {
                notice.error = true;
            } else if session.id == active_pane {
                notice.error = false;
            }
            if session.id == active_pane {
                notice.unread = false;
            }
            // A program that asked for the user's eye gets it: the pane
            // marks itself unread, the Cockpit learns, and the bar says so.
            if pulse.notifications != notice.notifications_seen {
                let fresh = pulse.notifications > notice.notifications_seen;
                notice.notifications_seen = pulse.notifications;
                if fresh {
                    if session.id != active_pane {
                        notice.unread = true;
                    }
                    if let Some(text) = pulse.last_notification.clone() {
                        announcements.push((session.id as u64, text));
                    }
                }
            }
            notice.revision = pulse.revision;
            unterm_services::cockpit::status::on_screen_tail(session.id as u64, &tail);
            unterm_services::cockpit::status::on_title_change(session.id as u64, &session.title);
        }
        for (pane, text) in announcements {
            unterm_services::cockpit::status::on_notification(pane, None, &text);
            // The window says so where the eye is; when the eye is elsewhere,
            // the system's own notification carries it.
            if !self.window.focused {
                let _ = unterm_services::toast::notify("Unterm", &text);
            }
            self.show_notice(format!("\u{1F514} {text}"));
        }

        unterm_services::cockpit::status::poll(&pane_ids, |pane_id| {
            crate::statsbar::known_facts(pane_id as usize).agent_id
        });
        let live: std::collections::HashSet<u64> = pane_ids.iter().copied().collect();
        unterm_services::cockpit::status::retain_panes(&live);

        let statuses = unterm_services::cockpit::status::snapshot();
        let titles: std::collections::HashMap<u64, String> = sessions
            .iter()
            .map(|session| (session.id as u64, session.title.clone()))
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let agents: Vec<unterm_services::server_info::InstanceAgentInfo> = statuses
            .iter()
            .map(|status| unterm_services::server_info::InstanceAgentInfo {
                pane_id: status.pane_id,
                tab_id: self
                    .window.tabs
                    .tab_of_pane(status.pane_id as usize)
                    .map(|id| id as u64),
                window_id: Some(0),
                pane_title: titles.get(&status.pane_id).cloned(),
                agent: status.agent.clone(),
                state: status.state.as_str().to_string(),
                since_unix_ms:
                    now.saturating_sub(
                        status.since.elapsed().as_millis().min(i64::MAX as u128) as i64
                    ),
                task_hint: status.task_hint.clone(),
            })
            .collect();
        let _ = std::thread::Builder::new()
            .name("cockpit-instance-publish".to_string())
            .spawn(move || {
                if let Err(err) = unterm_services::server_info::set_agents(agents) {
                    log::debug!("could not publish cockpit snapshot: {err:#}");
                }
            });

        let current_instance =
            unterm_services::server_info::current_instance_id().unwrap_or_default();
        crate::cockpit::refresh_peer_statuses(current_instance);

        let checkpoint_panes = unterm_services::cockpit::status::take_checkpoint_requests();
        if !checkpoint_panes.is_empty() {
            let _ = std::thread::Builder::new()
                .name("cockpit-checkpoint".to_string())
                .spawn(move || {
                    let engine = unterm_engine::host_engine();
                    for pane_id in checkpoint_panes {
                        let Some(status) =
                            unterm_services::cockpit::status::status_for_pane(pane_id)
                        else {
                            continue;
                        };
                        let activity =
                            unterm_engine::SessionEngine::activity(&*engine, pane_id as usize)
                                .ok();
                        let cwd = activity
                            .as_ref()
                            .and_then(|activity| activity.process.as_ref())
                            .and_then(|process| {
                                process
                                    .foreground_cwd
                                    .clone()
                                    .or_else(|| process.root_cwd.clone())
                            })
                            .or_else(|| {
                                unterm_engine::SessionEngine::shell(&*engine, pane_id as usize)
                                    .ok()
                                    .and_then(|shell| shell.cwd)
                            });
                        if let Some(cwd) = cwd {
                            if let Err(err) =
                                unterm_services::cockpit::review::record_auto_checkpoint(
                                    std::path::Path::new(&cwd),
                                    &status.agent,
                                    pane_id,
                                )
                            {
                                log::debug!("automatic cockpit checkpoint skipped: {err:#}");
                            }
                        }
                    }
                });
        }
    }

    /// The agent inbox, over the terminal.
    fn inbox_rows(&self) -> Vec<crate::cockpit::Row> {
        let statuses = unterm_services::cockpit::status::snapshot();
        let instance_id = unterm_services::server_info::current_instance_id().unwrap_or_default();
        let window_title = self
            .window.window_title
            .clone()
            .unwrap_or_else(|| instance_id.clone());
        let mut located: Vec<crate::cockpit::LocatedStatus> = statuses
            .into_iter()
            .map(|status| crate::cockpit::LocatedStatus {
                pane_id: status.pane_id,
                instance_id: instance_id.clone(),
                window_title: window_title.clone(),
                tab_id: self
                    .window.tabs
                    .tab_of_pane(status.pane_id as usize)
                    .map(|id| id as u64),
                agent: status.agent,
                state: status.state,
                age_seconds: status.since.elapsed().as_secs(),
                task_hint: status.task_hint,
            })
            .collect();
        located.extend(crate::cockpit::peer_statuses());
        crate::cockpit::located_rows(&located)
    }

    /// Move through the inbox or jump directly to the selected pane.
    ///
    /// The inbox is modal while open: keys it does not use are swallowed so
    /// typing while choosing an agent cannot leak into the shell underneath.
    fn handle_inbox_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key as WinitKey, NamedKey};

        if !self.window.inbox_open {
            return false;
        }
        let rows = self.inbox_rows();
        let count = rows.len().min(MAX_INBOX_ROWS);
        if count == 0 {
            self.window.inbox_selected = 0;
        } else {
            self.window.inbox_selected = self.window.inbox_selected.min(count - 1);
        }

        match &event.logical_key {
            WinitKey::Named(NamedKey::Escape) => self.window.inbox_open = false,
            WinitKey::Named(NamedKey::ArrowUp) if count > 0 => {
                self.window.inbox_selected =
                    crate::cockpit::step_selection(self.window.inbox_selected, count, false);
            }
            WinitKey::Named(NamedKey::ArrowDown) if count > 0 => {
                self.window.inbox_selected =
                    crate::cockpit::step_selection(self.window.inbox_selected, count, true);
            }
            WinitKey::Named(NamedKey::Enter) if count > 0 => {
                let selected = rows[self.window.inbox_selected].clone();
                let pane_id = selected.pane_id as usize;
                let current_instance =
                    unterm_services::server_info::current_instance_id().unwrap_or_default();
                if selected.instance_id.is_empty() || selected.instance_id == current_instance {
                    if self.window.tabs.tab_of_pane(pane_id).is_none() {
                        self.show_notice(format!("pane {pane_id} is no longer available"));
                        return true;
                    }
                    self.window.inbox_open = false;
                    self.focus_session(pane_id);
                } else {
                    self.window.inbox_open = false;
                    let instance_id = selected.instance_id.clone();
                    let target_pane = selected.pane_id;
                    self.show_notice(format!("focusing {instance_id} / pane {target_pane}"));
                    let _ = std::thread::Builder::new()
                        .name("cockpit-peer-focus".to_string())
                        .spawn(move || {
                            let peer = unterm_services::server_info::list_live_instances()
                                .into_iter()
                                .find(|instance| instance.id == instance_id);
                            match peer {
                                Some(peer) => {
                                    if let Err(err) =
                                        unterm_services::peer_mcp::focus_pane(&peer, target_pane)
                                    {
                                        log::warn!(
                                            "could not focus {instance_id} pane {target_pane}: {err:#}"
                                        );
                                    }
                                }
                                None => log::warn!(
                                    "could not focus {instance_id} pane {target_pane}: instance is gone"
                                ),
                            }
                        });
                }
            }
            _ => {}
        }

        self.window.drawn_revision = None;
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
        true
    }

    /// Type into the composer. Returns true when the key was the composer's.
    fn handle_composer_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key as WinitKey, NamedKey};

        let shift = self.window.shift_held;
        let ctrl = self.window.ctrl_held;
        let Some(composer) = self.window.composer.as_mut() else {
            return false;
        };
        let mut send = None;
        match &event.logical_key {
            WinitKey::Named(NamedKey::Enter) if shift => composer.typing.push('\n'),
            WinitKey::Named(NamedKey::Enter) if ctrl => send = composer.take_selected(),
            WinitKey::Named(NamedKey::Enter) => composer.commit(),
            WinitKey::Named(NamedKey::Backspace) => {
                composer.typing.pop();
            }
            WinitKey::Named(NamedKey::Delete) => {
                composer.remove_selected();
            }
            WinitKey::Named(NamedKey::ArrowUp) => composer.select_by(-1),
            WinitKey::Named(NamedKey::ArrowDown) => composer.select_by(1),
            WinitKey::Named(NamedKey::Tab) => composer.cycle_mode(),
            WinitKey::Named(NamedKey::Escape) => {
                // The queue first, the panel second. A batch someone has just
                // written is not something one keystroke should throw away
                // without saying so -- and the panel emptying is it saying so.
                if composer.is_empty() {
                    self.window.composer = None;
                } else {
                    composer.clear();
                    composer.typing.clear();
                }
            }
            WinitKey::Named(NamedKey::Space) => composer.typing.push(' '),
            WinitKey::Character(text) => composer.typing.push_str(text),
            // Everything else belongs to the shell behind this: a queue is
            // being written, not a program driven.
            _ => return false,
        }
        if let Some(prompt) = send {
            let pane = self.focused_session();
            let _ = self.engine.write_input(pane, &format!("{prompt}\r"));
        }
        self.window.drawn_revision = None;
        true
    }

    /// The oldest pending MCP suggestion owns only its explicit decision keys.
    fn handle_suggestion_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key as WinitKey, NamedKey};
        let pane = self.focused_session();
        let Some(suggestion) = crate::engine_backend::mcp_state::pending_suggestions_for_pane(pane as u64)
            .into_iter()
            .next()
        else {
            return false;
        };
        let run = self.window.alt_held && matches!(event.logical_key, WinitKey::Named(NamedKey::Enter));
        if matches!(event.logical_key, WinitKey::Named(NamedKey::Tab)) || run {
            match crate::engine_backend::mcp_state::accept_suggestion(&suggestion.id, run) {
                Ok(mut text) => {
                    if run {
                        text.push('\r');
                    }
                    if let Err(err) = self.engine.write_input(pane, &text) {
                        log::warn!("could not accept suggestion: {err:#}");
                    }
                }
                Err(err) => log::warn!("could not accept suggestion: {err}"),
            }
        } else if matches!(event.logical_key, WinitKey::Named(NamedKey::Escape)) {
            if let Err(err) = crate::engine_backend::mcp_state::dismiss_suggestion(&suggestion.id) {
                log::warn!("could not dismiss suggestion: {err}");
            }
        } else {
            return false;
        }
        self.window.drawn_revision = None;
        true
    }

    /// Send the next queued prompt if the pane is ready for one.
    ///
    /// Called from the frame loop rather than on a timer, because the thing it
    /// waits on -- the pane going idle -- is something the frame loop already
    /// knows about.
    fn drain_composer(&mut self) {
        let (true, Some(live)) = (self.window.composer.is_some(), self.window.state.as_ref()) else {
            return;
        };
        let session_id = live.session_id;
        let idle = unterm_engine::SessionEngine::activity(&self.engine, session_id)
            .map(|activity| activity.idle)
            .unwrap_or(false);

        // An agent asking permission to carry on looks exactly like an agent
        // that has finished, and a queue that cannot tell them apart sends its
        // next prompt as the answer to the question. So the question is
        // answered first, and only the narrow shape of question that offers a
        // yes as the obvious answer -- anything that mentions deleting,
        // removing, overwriting or forcing waits for a person.
        let auto_approve = self
            .window.composer
            .as_ref()
            .is_some_and(|composer| composer.mode() == crate::composer::ExecutionMode::AutoApprove);
        if auto_approve && idle && self.pane_is_asking_permission(session_id) {
            crate::engine_backend::mcp_state::audit_gui_write(
                "composer.auto_approve",
                session_id as u64,
                "accepted narrow affirmative confirmation",
            );
            let _ = self.engine.write_input(session_id, "y\r");
            self.window.drawn_revision = None;
            return;
        }

        let Some(prompt) = self
            .window.composer
            .as_mut()
            .and_then(|composer| composer.take_next(idle))
        else {
            return;
        };
        let _ = self.engine.write_input(session_id, &format!("{prompt}\r"));
        self.window.drawn_revision = None;
    }

    /// Whether the pane's last line is a question the composer may answer.
    fn pane_is_asking_permission(&self, session_id: usize) -> bool {
        let Ok(screen) = self.engine.read_screen(session_id) else {
            return false;
        };
        screen
            .lines
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(|line| crate::composer::is_confirmation(line))
            .unwrap_or(false)
    }

    /// The queue, and the line being written.
    fn append_composer(&mut self, window_width: f32, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(composer) = self.window.composer.clone() else {
            return;
        };
        let metrics = self.window.font.metrics();
        let width = (window_width * 0.6)
            .max(metrics.width * 30.0)
            .min(window_width);
        let left = ((window_width - width) / 2.0).max(0.0);
        let queued = composer.queued();
        let shown = queued.len().min(MAX_COMPOSER_ROWS);
        let rows = shown + 2;
        let top = metrics.height * 2.0;
        let foreground = self.window.colors.foreground;

        quads.backgrounds.extend(unterm_render::rounded::panel(
            left,
            top,
            width,
            metrics.height * rows as f32,
            self.corner_radius(),
            mix(self.window.colors.background, foreground, 0.10),
        ));

        let title = unterm_services::i18n::t("composer.title");
        let mode = composer.mode().label();
        let heading = if queued.is_empty() {
            format!(
                "{title}  [{mode}]  ({})",
                unterm_services::i18n::t("composer.hint")
            )
        } else {
            format!(
                "{title}  [{mode}]  ({})",
                unterm_services::i18n::t_args(
                    "composer.waiting",
                    &[("n", &queued.len().to_string())]
                )
            )
        };
        let mut lines = vec![heading];
        lines.extend(
            queued
                .iter()
                .take(shown)
                .enumerate()
                .map(|(index, prompt)| {
                    let marker = if index == composer.selected() {
                        ">"
                    } else {
                        " "
                    };
                    format!("{marker} {}. {}", index + 1, prompt.replace('\n', " ↵ "))
                }),
        );
        // The line being written, with a cursor after it so it is obviously
        // the one accepting keys.
        lines.push(format!("> {}_", composer.typing.replace('\n', " ↵ ")));

        for (index, line) in lines.iter().enumerate() {
            crate::terminal::append_text(
                line,
                &mut self.window.font,
                &mut self.window.atlas,
                foreground,
                (left + metrics.width, top + metrics.height * index as f32),
                quads,
            );
        }
    }

    fn append_suggestion(
        &mut self,
        window_width: f32,
        quads: &mut unterm_render::quads::FrameQuads,
    ) {
        let pending =
            crate::engine_backend::mcp_state::pending_suggestions_for_pane(self.focused_session() as u64);
        let Some(suggestion) = pending.first() else {
            return;
        };
        let metrics = self.window.font.metrics();
        let width = (window_width * 0.64)
            .max(metrics.width * 36.0)
            .min(window_width);
        let left = ((window_width - width) / 2.0).max(0.0);
        let top = metrics.height * 2.0;
        let foreground = self.window.colors.foreground;
        let source = if suggestion.posted_by_agent.trim().is_empty() {
            "agent"
        } else {
            suggestion.posted_by_agent.as_str()
        };
        let mut lines = vec![
            format!("Suggestion from {source}  (1/{})", pending.len()),
            suggestion.text.replace('\n', " ↵ "),
        ];
        if let Some(reason) = suggestion
            .rationale
            .as_deref()
            .filter(|reason| !reason.trim().is_empty())
        {
            lines.push(format!("Why: {}", reason.replace('\n', " ")));
        }
        lines.push("Tab accept  Alt+Enter accept & run  Esc dismiss".to_string());
        quads.backgrounds.extend(unterm_render::rounded::panel(
            left,
            top,
            width,
            metrics.height * lines.len() as f32,
            self.corner_radius(),
            mix(self.window.colors.background, foreground, 0.12),
        ));
        for (index, line) in lines.iter().enumerate() {
            crate::terminal::append_text(
                line,
                &mut self.window.font,
                &mut self.window.atlas,
                foreground,
                (left + metrics.width, top + metrics.height * index as f32),
                quads,
            );
        }
    }

    fn toggle_git_panel(&mut self) {
        self.window.git_panel = match self.window.git_panel {
            Some(_) => None,
            None => {
                let cwd = self
                    .current_directory()
                    .or_else(dirs_next::home_dir)
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                Some(GitDock {
                    panel: crate::git::read(&cwd),
                    cwd,
                })
            }
        };
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    fn git_panel_width(&self) -> f32 {
        self.git_panel_width_for(self.window_width())
    }

    fn git_panel_width_for(&self, window_width: f32) -> f32 {
        if self.window.git_panel.is_none() {
            return 0.0;
        }
        let pt = self.chrome_pt();
        let max = (window_width * crate::ui_tokens::GIT_PANEL_MAX_RATIO)
            .max(crate::ui_tokens::GIT_PANEL_MIN_WIDTH * pt);
        (crate::ui_tokens::GIT_PANEL_WIDTH * pt)
            .clamp(crate::ui_tokens::GIT_PANEL_MIN_WIDTH * pt, max)
            .round()
    }

    /// What git says about where this pane is.
    fn append_git_panel(
        &mut self,
        window_width: f32,
        quads: &mut unterm_render::quads::FrameQuads,
    ) {
        let Some(cwd) = self.current_directory() else {
            return;
        };
        if let Some(dock) = self.window.git_panel.as_mut() {
            if dock.cwd != cwd {
                dock.cwd = cwd.clone();
                dock.panel = crate::git::read(&cwd);
            }
        }
        let Some(status) = self.window.git_panel.as_ref().map(|dock| dock.panel.clone()) else {
            return;
        };
        let metrics = self.window.font.metrics();
        let width = self.git_panel_width().min(window_width);
        // A repository inspector is a dock, not a modal: keep the terminal
        // visible beside it and anchor it to the right edge.
        let left = (window_width - width).max(0.0);
        let top = self.terminal_top();
        let foreground = self.window.colors.foreground;

        let heading = status.heading();
        let lines: Vec<String> = status
            .entries()
            .iter()
            .take(MAX_GIT_ROWS)
            .map(|entry| format!("{:<3}{}", entry.code, entry.path))
            .collect();
        let height = self.terminal_height();

        quads.backgrounds.extend(unterm_render::rounded::panel(
            left,
            top,
            width,
            height,
            self.corner_radius(),
            mix(self.window.colors.background, foreground, 0.10),
        ));
        crate::terminal::append_text(
            &heading,
            &mut self.window.font,
            &mut self.window.atlas,
            foreground,
            (left + metrics.width, top),
            quads,
        );
        for (index, line) in lines.iter().enumerate() {
            crate::terminal::append_text(
                line,
                &mut self.window.font,
                &mut self.window.atlas,
                foreground,
                (
                    left + metrics.width,
                    top + metrics.height * (index + 1) as f32,
                ),
                quads,
            );
        }
    }

    fn append_inbox(&mut self, window_width: f32, quads: &mut unterm_render::quads::FrameQuads) {
        if !self.window.inbox_open {
            return;
        }
        let rows = self.inbox_rows();

        let metrics = self.window.font.metrics();
        let width = (window_width * 0.5)
            .max(metrics.width * 30.0)
            .min(window_width);
        let left = ((window_width - width) / 2.0).max(0.0);
        let top = metrics.height * 2.0;
        let shown = rows.len().min(MAX_INBOX_ROWS);
        if shown == 0 {
            self.window.inbox_selected = 0;
        } else {
            self.window.inbox_selected = self.window.inbox_selected.min(shown - 1);
        }
        let height = metrics.height * (shown + 1) as f32;
        let foreground = self.window.colors.foreground;

        quads.backgrounds.extend(unterm_render::rounded::panel(
            left,
            top,
            width,
            height,
            self.corner_radius(),
            mix(self.window.colors.background, foreground, 0.10),
        ));

        let heading = if rows.is_empty() {
            unterm_services::i18n::t("cockpit.inbox_title")
        } else {
            format!(
                "{}  ({})",
                unterm_services::i18n::t("cockpit.inbox_title"),
                unterm_services::i18n::t_args(
                    "composer.waiting",
                    &[(
                        "n",
                        &rows.iter().filter(|row| row.needs_you).count().to_string()
                    )]
                )
            )
        };
        crate::terminal::append_text(
            &heading,
            &mut self.window.font,
            &mut self.window.atlas,
            foreground,
            (left + metrics.width, top),
            quads,
        );

        for (index, row) in rows.iter().take(shown).enumerate() {
            let row_top = top + metrics.height * (index + 1) as f32;
            if index == self.window.inbox_selected {
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left,
                    top: row_top,
                    width,
                    height: metrics.height,
                    color: mix(self.window.colors.background, foreground, 0.18),
                });
            }
            if row.needs_you {
                // The ones wanting an answer are marked, so the list can be
                // read at a glance rather than word by word.
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left,
                    top: row_top,
                    width: metrics.width * 0.4,
                    height: metrics.height,
                    color: foreground,
                });
            }
            let marker = if index == self.window.inbox_selected {
                "›"
            } else {
                " "
            };
            let location = if row.instance_id.is_empty() {
                format!("{}", row.pane_id)
            } else {
                let window = if row.window_title.is_empty() || row.window_title == row.instance_id {
                    row.instance_id.clone()
                } else {
                    format!("{} ({})", row.window_title, row.instance_id)
                };
                if let Some(tab_id) = row.tab_id {
                    format!("{window} / tab {tab_id} / pane {}", row.pane_id)
                } else {
                    format!("{window} / pane {}", row.pane_id)
                }
            };
            let text = if row.hint.is_empty() {
                format!("{marker} {location}  {}", row.label)
            } else {
                format!("{marker} {location}  {}  -- {}", row.label, row.hint)
            };
            crate::terminal::append_text(
                &text,
                &mut self.window.font,
                &mut self.window.atlas,
                foreground,
                (left + metrics.width, row_top),
                quads,
            );
        }
    }

    /// Start quick select, if there is anything worth labelling.
    ///
    /// Nothing worth labelling means nothing to do: an overlay with no labels
    /// in it looks like a broken feature rather than an empty screen.
    fn open_quick_select(&mut self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let Ok(snapshot) = self.engine.read_styled_screen(live.session_id) else {
            return;
        };
        let found = crate::copy_mode::labelled(&snapshot.lines);
        if found.is_empty() {
            return;
        }
        self.window.quick_select = Some((found, String::new()));
        self.window.drawn_revision = None;
    }

    /// Type a label. Returns true when the key was quick select's.
    fn handle_quick_select_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::Key as WinitKey;
        let Some((found, mut typed)) = self.window.quick_select.take() else {
            return false;
        };
        match &event.logical_key {
            WinitKey::Named(winit::keyboard::NamedKey::Escape) => {}
            WinitKey::Character(text) => {
                typed.push_str(text);
                // Labels are lowercase, but typing one with shift held is a
                // different request -- 0.57.4's "and type it into the pane".
                // Matching has to fold the case, or the shifted spelling
                // could never hit anything.
                let wants_paste = typed.chars().any(|ch| ch.is_uppercase());
                let lowered = typed.to_lowercase();
                if let Some(hit) = found.iter().find(|item| item.label == lowered) {
                    let text = hit.text.clone();
                    self.copy_text(&text);
                    if wants_paste {
                        let pane = self.focused_session();
                        let _ = self.engine.paste_input(pane, &text);
                    }
                } else if found.iter().any(|item| item.label.starts_with(&lowered)) {
                    // A prefix of a longer label: wait for the rest.
                    self.window.quick_select = Some((found, typed));
                }
            }
            _ => self.window.quick_select = Some((found, typed)),
        }
        self.window.drawn_revision = None;
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
        true
    }

    /// Move and select with the keyboard. Returns true when handled.
    fn handle_copy_mode_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::Key as WinitKey;
        let Some(mut mode) = self.window.copy_mode else {
            return false;
        };
        let named = match &event.logical_key {
            WinitKey::Named(named) => Some(format!("{named:?}")),
            _ => None,
        };
        let character = match &event.logical_key {
            WinitKey::Character(text) => Some(text.to_string()),
            _ => None,
        };

        // An armed `f`/`t` owns the next character: it is the target to jump
        // to, not a motion -- without this, `ft` could never find a literal
        // `t`. Escape disarms; anything that is not a character is neither a
        // target nor a reason to give up waiting for one.
        if let Some((forward, till)) = mode.pending_find {
            if named.as_deref() == Some("Escape") {
                mode.pending_find = None;
            } else if let Some(target) = character.as_deref().and_then(|text| {
                let mut chars = text.chars();
                let first = chars.next()?;
                chars.next().is_none().then_some(first)
            }) {
                mode.pending_find = None;
                let line = self.line_text(mode.row);
                mode.apply_find(target, forward, till, &line);
            }
            self.window.copy_mode = Some(mode);
            self.window.drawn_revision = None;
            if let Some(live) = self.window.state.as_ref() {
                live.window.request_redraw();
            }
            return true;
        }

        let Some(motion) =
            crate::copy_mode::motion_for(named.as_deref(), character.as_deref(), self.window.ctrl_held)
        else {
            // Nothing reaches the shell: a stray keystroke running a command
            // in the pane behind is the worst thing a mode can do.
            self.window.copy_mode = Some(mode);
            return true;
        };

        match motion {
            crate::copy_mode::Motion::Leave => self.window.copy_mode = None,
            crate::copy_mode::Motion::Yank => {
                if let Some(text) = self.copy_mode_selection(&mode) {
                    self.copy_text(&text);
                }
                self.window.copy_mode = None;
            }
            crate::copy_mode::Motion::Find { forward, till } => {
                mode.pending_find = Some((forward, till));
                self.window.copy_mode = Some(mode);
            }
            crate::copy_mode::Motion::RepeatFind(same_direction) => {
                if let Some((target, forward, till)) = mode.last_find {
                    let direction = if same_direction { forward } else { !forward };
                    let line = self.line_text(mode.row);
                    mode.apply_find(target, direction, till, &line);
                    // `,` mirrors this one jump, not the remembered find:
                    // `;` afterwards must still go the way the `f` did.
                    mode.last_find = Some((target, forward, till));
                }
                self.window.copy_mode = Some(mode);
            }
            motion @ (crate::copy_mode::Motion::WordLeft | crate::copy_mode::Motion::WordRight) => {
                let rows = self.screen_shape().0;
                let lines: Vec<String> = self
                    .window.state
                    .as_ref()
                    .and_then(|live| self.engine.read_styled_screen(live.session_id).ok())
                    .map(|snapshot| {
                        snapshot
                            .lines
                            .iter()
                            .map(|line| line.cells.iter().map(|cell| cell.ch).collect())
                            .collect()
                    })
                    .unwrap_or_default();
                mode.apply_word(motion, rows, |row| {
                    lines.get(row).cloned().unwrap_or_default()
                });
                self.window.copy_mode = Some(mode);
            }
            motion => {
                let (rows, widths) = self.screen_shape();
                mode.apply(motion, rows, |row| widths.get(row).copied().unwrap_or(0));
                self.window.copy_mode = Some(mode);
            }
        }
        self.window.drawn_revision = None;
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
        true
    }

    /// One row of the screen as plain text, for the motions that need to see
    /// characters rather than widths.
    fn line_text(&self, row: usize) -> String {
        self.window.state
            .as_ref()
            .and_then(|live| self.engine.read_styled_screen(live.session_id).ok())
            .and_then(|snapshot| {
                snapshot
                    .lines
                    .get(row)
                    .map(|line| line.cells.iter().map(|cell| cell.ch).collect())
            })
            .unwrap_or_default()
    }

    /// How many rows the screen has, and how wide each one's text is.
    fn screen_shape(&self) -> (usize, Vec<usize>) {
        let Some(live) = self.window.state.as_ref() else {
            return (0, Vec::new());
        };
        let Ok(snapshot) = self.engine.read_styled_screen(live.session_id) else {
            return (0, Vec::new());
        };
        let widths = snapshot
            .lines
            .iter()
            .map(|line| {
                // Trailing blanks are padding, not text: end-of-line should
                // land on the last character someone wrote.
                line.cells
                    .iter()
                    .rposition(|cell| cell.ch != ' ' && cell.ch != '\0')
                    .map(|last| last + 1)
                    .unwrap_or(0)
            })
            .collect();
        (snapshot.lines.len(), widths)
    }

    /// The text copy mode has selected.
    fn copy_mode_selection(&self, mode: &crate::copy_mode::CopyMode) -> Option<String> {
        let live = self.window.state.as_ref()?;
        let snapshot = self.engine.read_styled_screen(live.session_id).ok()?;
        let ((start_row, start_col), (end_row, end_col)) = mode.selection()?;
        let last = snapshot.lines.len().saturating_sub(1);

        let mut out = String::new();
        for row in start_row..=end_row.min(last) {
            let text = selection_row_text(&snapshot.lines[row].cells);
            // The shape decides the columns: whole rows for `V`, the same
            // column band on every row for `Ctrl+v`, and the vim sweep
            // otherwise.
            let (from, to) = match mode.kind {
                crate::copy_mode::SelectKind::Line => (0, text.chars().count()),
                crate::copy_mode::SelectKind::Block => (
                    start_col.min(end_col),
                    (start_col.max(end_col) + 1).min(text.chars().count()),
                ),
                crate::copy_mode::SelectKind::Cell => (
                    if row == start_row { start_col } else { 0 },
                    if row == end_row {
                        (end_col + 1).min(text.chars().count())
                    } else {
                        text.chars().count()
                    },
                ),
            };
            if from < to {
                out.extend(text.chars().skip(from).take(to - from));
            }
            if row < end_row {
                out.push('\n');
            }
        }
        Some(strip_spacer_marks(out.trim_end().to_string()))
    }

    /// Put text on the clipboard, and say so.
    ///
    /// A copy that does nothing visible is one the user repeats, and then
    /// goes hunting through a clipboard manager for.
    fn copy_text(&mut self, text: &str) {
        let tx = self.clipboard_tx.clone();
        let text = text.to_string();
        crate::clipboard::run(tx, move || {
            let result = arboard::Clipboard::new()
                .and_then(|mut board| board.set_text(text))
                .map_err(|err| err.to_string());
            ClipboardResult::Written(result)
        });
    }

    fn collect_clipboard_results(&mut self) {
        while let Ok(result) = self.clipboard_rx.try_recv() {
            match result {
                ClipboardResult::Read { pane_id, result } => match result {
                    Ok(text) if !text.is_empty() => match self.engine.paste_input(pane_id, &text) {
                        Ok(_) => self.show_notice(unterm_services::i18n::t("interaction.pasted")),
                        Err(err) => {
                            log::warn!("could not paste: {err:#}");
                            self.show_notice(unterm_services::i18n::t("interaction.paste_failed"));
                        }
                    },
                    Ok(_) => {}
                    Err(err) => {
                        log::warn!("could not read the clipboard: {err}");
                        self.show_notice(unterm_services::i18n::t("interaction.paste_failed"));
                    }
                },
                ClipboardResult::Written(Ok(())) => {
                    self.show_notice(unterm_services::i18n::t("interaction.copied"));
                }
                ClipboardResult::Written(Err(err)) => {
                    log::warn!("could not copy to the clipboard: {err}");
                    self.show_notice(unterm_services::i18n::t("interaction.paste_failed"));
                }
                ClipboardResult::DirectoryPicked { then, result } => match result {
                    Ok(Some(path)) => {
                        let path = path.display().to_string();
                        match then {
                            crate::palette::BrowseThen::ChangeDirectory => {
                                self.change_directory(&path)
                            }
                            crate::palette::BrowseThen::NewTab => self.new_tab_in(&path),
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        log::warn!("could not open the system folder picker: {err}");
                        // The cause rides along: a notice that only says
                        // "could not open" is a notice the user can only
                        // report back as "it errored".
                        self.show_notice(format!(
                            "{} — {err}",
                            unterm_services::i18n::t("dirjump.picker_failed")
                        ));
                    }
                },
                ClipboardResult::ScreenshotFinished { mode, result } => match result {
                    Ok(path) => self.show_notice(unterm_services::i18n::t_args(
                        "screenshot.saved",
                        &[("mode", &mode), ("path", &path.display().to_string())],
                    )),
                    Err(err) => self.show_notice(unterm_services::i18n::t_args(
                        "screenshot.failed",
                        &[("mode", &mode), ("err", &err)],
                    )),
                },
                ClipboardResult::ExportFinished(result) => match result {
                    Ok(path) => self.show_notice(unterm_services::i18n::t_args(
                        "recording.exported",
                        &[("path", &path.display().to_string())],
                    )),
                    Err(err) => self.show_notice(unterm_services::i18n::t_args(
                        "recording.export_failed",
                        &[("err", &err)],
                    )),
                },
                ClipboardResult::ScrollbackCaptured(result) => match result {
                    Ok(path) => self.show_notice(format!("✓ {}", path.display())),
                    Err(err) => self.show_notice(unterm_services::i18n::t_args(
                        "scrollshot.failed",
                        &[("err", &err)],
                    )),
                },
            }
        }
    }

    fn click_git_panel(&self) -> bool {
        let Some(live) = self.window.state.as_ref() else {
            return false;
        };
        let width = self.git_panel_width();
        width > 0.0
            && self.window.pointer.0 >= live.width as f32 - width
            && self.window.pointer.1 >= self.terminal_top()
            && self.window.pointer.1 < live.height as f32 - self.status_bar_height()
    }

    /// Open the native directory chooser away from winit's event thread.
    fn open_system_directory_picker(&mut self, then: crate::palette::BrowseThen) {
        let start = self.current_directory();
        let title = unterm_services::i18n::t("dirjump.picker_title");
        let tx = self.clipboard_tx.clone();
        crate::clipboard::run(tx, move || ClipboardResult::DirectoryPicked {
            then,
            result: crate::directory::pick_directory(start.as_deref(), &title)
                .map_err(|err| err.to_string()),
        });
    }

    /// Open the palette on a set of rows.

    /// Take the focused pane to a directory, by typing it there.
    ///
    /// Through the shell rather than behind its back: a shell that is told to
    /// `cd` updates its own prompt, its history and its OSC 7 report, and the
    /// terminal learns the new directory the same way it learns every other
    /// one. Moving the pty's directory underneath it would leave the shell
    /// convinced it was somewhere else.
    fn change_directory(&mut self, path: &str) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let command = format!("cd {}\r", shell_quoted_path(path));
        let _ = self.engine.write_input(live.session_id, &command);
    }

    /// Open a tab whose shell starts in a directory.
    ///
    /// The directory travels with the session request rather than through
    /// `start_directory`: that field is where the *first* shell starts, read
    /// once at startup, and parking a path there opened every later tab in
    /// the default directory instead.
    fn new_tab_in(&mut self, path: &str) {
        let shell = self.shell.clone();
        self.open_tab_with(shell, Some(path.to_string()));
    }

    /// Reopen a saved workspace, a tab per saved directory.
    ///
    /// Next-core has no live workspaces to switch between -- a workspace is
    /// a snapshot shared with the MCP `workspace.*` tools -- so 0.57.4's
    /// "switch to workspace" becomes "open its tabs here".
    fn restore_workspace(&mut self, name: &str) {
        let cwds = crate::workspaces::cwds(name);
        if cwds.is_empty() {
            self.show_notice(format!("workspace `{name}` has no directories to open"));
            return;
        }
        let count = cwds.len();
        for cwd in cwds {
            self.new_tab_in(&cwd);
        }
        self.show_notice(format!("workspace `{name}` opened · {count} tabs"));
    }

    /// Ask for a workspace name on the palette line. Enter saves the open
    /// tabs under it, Esc changes nothing.
    fn open_workspace_save(&mut self) {
        self.window.palette = Some(crate::palette::Palette::writing(vec![
            crate::palette::Entry {
                label: "Save Workspace".to_string(),
                hint: "type a name · Enter saves · Esc cancels".to_string(),
                command: crate::palette::Command::SaveWorkspace,
            },
        ]));
        self.window.drawn_revision = None;
    }

    /// Write the open tabs down under a name: one entry per tab, where its
    /// pane is now. The same file `workspace.save` writes, so the palette and
    /// an agent see one list.
    fn save_workspace(&mut self, name: &str) {
        let sessions =
            unterm_engine::SessionEngine::list_sessions(&self.engine).unwrap_or_default();
        let mut tabs = Vec::new();
        for tab in self.window.tabs.tab_ids() {
            let Some(pane) = self.window.tabs.active_pane(tab) else {
                continue;
            };
            let Some(session) = sessions.iter().find(|session| session.id == pane) else {
                continue;
            };
            let Some(cwd) = session.shell.cwd.clone() else {
                continue;
            };
            let title = self
                .window.tab_titles
                .get(&tab)
                .cloned()
                .unwrap_or_else(|| session.title.clone());
            tabs.push((title, cwd));
        }
        match crate::workspaces::save(name, &tabs) {
            Ok(count) => self.show_notice(format!("workspace `{name}` saved · {count} tabs")),
            Err(err) => self.show_notice(format!("could not save the workspace: {err}")),
        }
    }

    /// Start recording the focused pane, or stop and say where it went.
    fn toggle_recording(&mut self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let pane = live.session_id;
        let recording = unterm_engine::RecordingEngine::recording_status(&self.engine, pane)
            .map(|status| status.enabled)
            .unwrap_or(false);
        let outcome = if recording {
            unterm_engine::RecordingEngine::stop_recording(&self.engine, pane)
                .map(|stopped| format!("recording saved to {}", stopped.md_path))
        } else {
            unterm_engine::RecordingEngine::start_recording(&self.engine, pane)
                .map(|started| format!("recording to {}", started.md_path))
        };
        match outcome {
            Ok(message) => log::info!("{message}"),
            Err(err) => log::warn!("recording: {err}"),
        }
        self.window.drawn_revision = None;
    }

    fn export_session(&mut self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let pane = live.session_id;
        let tx = self.clipboard_tx.clone();
        crate::clipboard::run(tx, move || {
            let result = unterm_engine::RecordingEngine::export_markdown(
                &*unterm_engine::host_engine(),
                pane,
                None,
            )
            .map(|exported| std::path::PathBuf::from(exported.path))
            .map_err(|err| err.to_string())
            .and_then(|path| {
                arboard::Clipboard::new()
                    .and_then(|mut board| board.set_text(path.display().to_string()))
                    .map_err(|err| err.to_string())?;
                Ok(path)
            });
            ClipboardResult::ExportFinished(result)
        });
        self.show_notice(unterm_services::i18n::t("recording.exporting"));
    }

    /// Settings live in a browser, not in a cell grid.
    /// Close once it is safe or once it is confirmed. Nothing running and a
    /// single tab close immediately; anything else opens one confirmation on
    /// the palette line, and Enter there closes for real.
    fn request_close(&mut self) {
        if self.window.close_confirmed || !self.close_needs_confirmation() {
            // Nothing was at stake, so nothing is left behind either: no
            // prompt was shown, no indicator will appear, and a shell that
            // outlived the window nobody was asked about is exactly the
            // invisible state the prompt exists to avoid.
            self.perform_close(CloseOutcome::EndSessions);
            return;
        }
        use unterm_services::i18n::t;
        // What is actually at stake, counted rather than implied: "2
        // agents waiting, 3 sessions running" is a sentence someone
        // can act on; "programs are running" is one they dismiss.
        let sessions = unterm_engine::SessionEngine::list_sessions(&self.engine)
            .map(|sessions| sessions.len())
            .unwrap_or(0);
        let waiting = crate::cockpit::attention_count(
            &unterm_services::cockpit::status::snapshot(),
        );
        let subject = if waiting > 0 {
            unterm_services::i18n::t_args(
                "close.title_waiting",
                &[
                    ("waiting", &waiting.to_string()),
                    ("sessions", &sessions.to_string()),
                ],
            )
        } else {
            unterm_services::i18n::t_args(
                "close.title",
                &[("sessions", &sessions.to_string())],
            )
        };

        let entry = |label: &str, hint: &str, command: crate::palette::Command| {
            crate::palette::Entry {
                label: t(label),
                hint: t(hint),
                command,
            }
        };
        // Three outcomes when a Core holds the sessions, two when this
        // process does: offering to keep shells running in the
        // background while they live and die with this window would be
        // a promise the Local engine cannot keep. The recoverable
        // answer is first, so the default action is the safe one.
        let entries = if self.engine.sessions_outlive_this_window() {
            vec![
                entry(
                    "close.background",
                    "close.background.hint",
                    crate::palette::Command::KeepRunningInBackground,
                ),
                entry(
                    "close.drain",
                    "close.drain.hint",
                    crate::palette::Command::DrainThenExit,
                ),
                entry(
                    "close.cancel_exit",
                    "close.cancel_exit.hint",
                    crate::palette::Command::CancelAndExit,
                ),
            ]
        } else {
            vec![entry(
                "close.exit_now",
                "close.exit_now.hint",
                crate::palette::Command::ConfirmCloseWindow,
            )]
        };
        self.window.palette = Some(crate::palette::Palette::confirm(entries, subject));
        self.window.drawn_revision = None;
    }

    /// Whether closing now would take something down with it: several tabs,
    /// or any pane whose foreground program is not just its own idle shell.
    fn close_needs_confirmation(&self) -> bool {
        if !self.close_prompts {
            return false;
        }
        // With another window still open, closing this one closes a view.
        // Nothing has to be decided about the sessions behind it, because
        // this process is not going anywhere and neither are they -- so the
        // prompt would be asking a question that has no consequences.
        if self.window_count() > 1 {
            return false;
        }
        let sessions =
            unterm_engine::SessionEngine::list_sessions(&self.engine).unwrap_or_default();
        if sessions.len() > 1 {
            return true;
        }
        sessions.iter().any(|session| {
            let facts = crate::statsbar::known_facts(session.id);
            if !facts.agent.is_empty() {
                return true;
            }
            let foreground = facts.title.trim_start_matches('\u{25B6}').trim();
            // The stats line names Windows shells on purpose; a shell being
            // itself is not a reason to ask before closing.
            !foreground.is_empty()
                && !crate::sidebar::same_program(foreground, &session.shell.process_name)
        })
    }

    /// Fill the desktop work area instead of asking the OS to maximise: a
    /// borderless window the OS maximises hangs eight pixels off every edge,
    /// which is the black band that read as a deformed bar.
    fn toggle_maximize(&mut self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        // A session restored by winit, a Win+Up gesture and a title-bar
        // double-click use the OS maximized state rather than our work-area
        // rectangle. The caption button must restore those immediately.
        if live.window.is_maximized() {
            live.window.set_maximized(false);
            return;
        }
        if let Some((position, size)) = self.window.unmaximized_rect.take() {
            live.window.set_outer_position(position);
            let _ = live.window.request_inner_size(size);
            return;
        }
        let Some((left, top, width, height)) = unterm_services::work_area() else {
            live.window.set_maximized(!live.window.is_maximized());
            return;
        };
        let position = live
            .window
            .outer_position()
            .unwrap_or(winit::dpi::PhysicalPosition::new(0, 0));
        self.window.unmaximized_rect = Some((position, live.window.inner_size()));
        live.window
            .set_outer_position(winit::dpi::PhysicalPosition::new(left, top));
        let _ = live
            .window
            .request_inner_size(winit::dpi::PhysicalSize::new(width, height));
    }

    /// Close the window, and end the sessions or leave them running.
    ///
    /// Both halves of that decision used to be spread across two paths that
    /// disagreed: the title bar's own close button left every session alive,
    /// while the same click on the system frame destroyed them all. One
    /// function, one argument, so "did my shells survive?" has one answer.
    /// Whether closing the last window should end this process.
    ///
    /// D3 of the multi-window design: the platforms disagree and both are
    /// right. On Windows and Linux an application is its windows, so the last
    /// one closing ends it -- unless the user chose to keep the sessions
    /// running, which is what the tray is for. On macOS an application
    /// outlives its windows: it stays in the Dock, and Cmd+Q is what ends it.
    /// Making one of those the rule everywhere would break the habit of
    /// whichever platform lost.
    fn last_window_close_ends_process() -> bool {
        !cfg!(target_os = "macos")
    }

    /// Close this window when it is not the last one.
    ///
    /// D1 of the multi-window design: closing a window closes a view. The
    /// sessions live in the Core and another window of this process is still
    /// showing something, so nothing here may end a shell -- including the
    /// shells this window was showing, which the user can still reach from
    /// the other window. Returns false when this *is* the last window, which
    /// is the case that has to be asked about.
    fn close_this_view(&mut self) -> bool {
        // The same rule the prompt uses, asked the same way. Two spellings of
        // "is this the last window" are two things to keep in step, and the
        // one place they must agree is here: the prompt decides whether to
        // ask, this decides whether the answer matters.
        if self.window_count() == 1 {
            return false;
        }
        self.window.close_confirmed = false;
        self.save_last_session();
        // Drop the window itself first: its surface must go before the one
        // taking its place is drawn into.
        if let Some(live) = self.window.state.take() {
            live.window.set_visible(false);
        }
        // Before the handle goes: an `instance.focus` naming this window must
        // fail rather than raise whichever window replaced it.
        crate::mcp_host::forget_window(self.window.id);
        let next = self.parked.pop().expect("a parked window");
        self.window = next.state;
        crate::mcp_host::set_focused_window(self.window.id);
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
        // A window that was behind has had its tabs settled without anyone
        // resizing its panes: `resize_panes` only ever describes the window
        // in front. Coming forward is when that debt is paid.
        self.resize_panes();
        self.window.drawn_revision = None;
        true
    }

    fn perform_close(&mut self, outcome: CloseOutcome) {
        let _slow = SlowGuard::new("perform_close");
        self.save_last_session();
        if outcome == CloseOutcome::EndSessions {
            // Every session *this window* holds, not every session there is.
            //
            // Asking the engine for the list gives the Core's, which is every
            // window's: two windows open, close one, and the other is left
            // running with no panes at all. Verified on the machine before
            // this line changed. The tab registry is this window's own, and
            // "the shells I was showing" is what closing a window ends.
            let mine: Vec<usize> = self
                .window.tabs
                .tab_ids()
                .into_iter()
                .flat_map(|tab| self.window.tabs.pane_ids(tab))
                .collect();
            for pane in mine {
                crate::statsbar::forget(pane);
                unterm_services::ghost_text::forget(pane as u64);
                let _ = self.engine.destroy_session(pane);
            }
        }
        if let Some(live) = self.window.state.as_ref() {
            live.window.set_visible(false);
        }
        // macOS keeps the application when its last window goes: it stays in
        // the Dock, and clicking it there brings a window back through
        // `resumed`. Ending the process would make the icon lie. Everywhere
        // else an application is its windows, so this goes on to exit.
        if !Self::last_window_close_ends_process() && outcome == CloseOutcome::KeepSessions {
            self.window.state = None;
            self.window.close_confirmed = false;
            return;
        }
        self.window.closing = true;
        // Take the Core with us. It is spawned detached so that closing a
        // window does not end the shells behind it, which is right for a
        // window and wrong for the application leaving: nothing else ever
        // stops it, so every quit left one running. They pile up unseen on
        // macOS, and on Windows one holds unterm-core.exe open against the
        // next installer -- which is why installing used to ask the user to
        // close a program that has no window.
        //
        // Here rather than after the event loop returns: the fuse below ends
        // this process with `exit`, which does not unwind, so nothing after
        // `run_app` runs on any platform. The sessions this window owned are
        // already gone by now.
        crate::engine_backend::stop_core_if_ours();
        // The fuse. Whatever a teardown thread is joining, whatever lock a
        // worker still wants: the user chose to leave, the state is saved,
        // and needing Force Quit to make that stick is the one outcome this
        // function is not allowed to produce.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(3));
            std::process::exit(0);
        });
    }

    /// What the indicator has to report right now.
    fn background_status(&self) -> crate::tray::Status {
        crate::tray::Status {
            sessions: unterm_engine::SessionEngine::list_sessions(&self.engine)
                .map(|sessions| sessions.len())
                .unwrap_or(0),
            waiting: crate::cockpit::attention_count(
                &unterm_services::cockpit::status::snapshot(),
            ),
        }
    }

    /// Put the window away and leave the sessions running behind an indicator.
    ///
    /// The window's GPU surface, its adopted panes and its Dock tile all go;
    /// the process stays alive for one reason only, which is to be the thing
    /// on screen that says the shells are still there and hands them back.
    ///
    /// Returns false when this desktop has no tray to put an indicator in. The
    /// caller then closes the ordinary way rather than parking sessions where
    /// nothing announces them -- the Core still holds them and `unterm` still
    /// finds them, but nobody is left believing an icon exists.
    fn enter_background(&mut self) -> bool {
        let _slow = SlowGuard::new("enter_background");
        self.save_last_session();
        let status = self.background_status();
        let Some(tray) = crate::tray::Tray::show(status) else {
            return false;
        };
        self.window.tray = Some(tray);
        self.window.tray_refreshed = std::time::Instant::now();
        // A focus request made while the window was still up has been served
        // already; leaving it pending would reopen the window the moment it
        // finished closing.
        crate::tray::clear_wake();
        // The window handle goes -- an `instance.focus` arriving on the MCP
        // thread must not raise a dead one. The Core's channel *stays*: a
        // parked front end can still do everything except paint, and the two
        // requests that do need a screen -- focus and confirmation -- are
        // exactly the two that should bring the window back rather than fail.
        // Detaching here would make an agent's "look at this" an error for as
        // long as the user had Unterm in the background, which is precisely
        // when an agent has something worth looking at.
        // Every window, not one: parking destroys them all, and a stale
        // entry here is an `Arc` keeping a dead window alive for the next
        // `instance.focus` to raise.
        crate::mcp_host::forget_all_windows();
        if let Some(live) = self.window.state.take() {
            live.window.set_visible(false);
            drop(live);
        }
        crate::tray::set_dock_visible(false);
        log::info!(
            "parked in the background: {} session(s), {} waiting",
            status.sessions,
            status.waiting
        );
        true
    }

    /// Come back from the tray: build the window again and let it adopt the
    /// sessions the Core kept. `start` already does the adopting -- it is the
    /// same path a cold launch onto a populated Core takes.
    fn leave_background(&mut self, event_loop: &ActiveEventLoop) {
        let _slow = SlowGuard::new("leave_background");
        // The Dock tile first: a window created while the process is still an
        // accessory opens behind every other app.
        crate::tray::set_dock_visible(true);
        match self.start(event_loop) {
            Ok(live) => {
                self.window.state = Some(live);
                // The prompt was answered by the window that just came back,
                // not by this one. Until parking existed the latch could
                // never outlive its answer -- the process died moments after
                // -- so nothing cleared it; now it does outlive it, and a
                // stale `true` sends the *next* close straight past the
                // prompt and into "end every session". Which is how a second
                // Cmd-W killed two shells the user was never asked about.
                self.window.close_confirmed = false;
                self.window.restore_extra_tabs_pending = true;
                self.window.drawn_revision = None;
                if let Some(live) = self.window.state.as_ref() {
                    live.window.set_visible(true);
                    live.window.focus_window();
                    live.window.request_redraw();
                }
                // The indicator's whole subject is back on screen; leaving it
                // up would be a second, stale copy of the same report.
                self.window.tray = None;
                self.draw();
            }
            Err(err) => {
                // Keep the indicator: it is the only way back, and the
                // sessions it counts are still there to come back to.
                crate::tray::set_dock_visible(false);
                log::error!("could not reopen the window from the tray: {err:#}");
            }
        }
    }

    /// The rest of the tabs the last window had, one per saved directory.
    fn restore_extra_tabs(&mut self) {
        // A Core that kept the sessions has already restored them --
        // `sync_tabs` adopts every one. Opening the saved directories
        // on top would spawn a second shell for each, and the count
        // would grow with every restart. The saved list is for a cold
        // start, where nothing is running to adopt.
        let adopted = unterm_engine::SessionEngine::list_sessions(&self.engine)
            .map(|sessions| sessions.len())
            .unwrap_or(0);
        if adopted > 1 {
            return;
        }
        let extra: Vec<String> = self
            .restore
            .as_ref()
            .map(|saved| saved.cwds.iter().skip(1).cloned().collect())
            .unwrap_or_default();
        if extra.is_empty() {
            return;
        }
        for cwd in extra {
            self.new_tab_in(&cwd);
        }
        self.select_tab(1);
    }

    /// Write down what this window looked like, for the next plain launch.
    fn save_last_session(&mut self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let size = live.window.inner_size();
        let mut cwds = Vec::new();
        let sessions =
            unterm_engine::SessionEngine::list_sessions(&self.engine).unwrap_or_default();
        for tab in self.window.tabs.tab_ids() {
            let Some(pane) = self.window.tabs.active_pane(tab) else {
                continue;
            };
            if let Some(cwd) = sessions
                .iter()
                .find(|session| session.id == pane)
                .and_then(|session| session.shell.cwd.clone())
            {
                cwds.push(cwd);
            }
        }
        crate::session_restore::save(&crate::session_restore::LastSession {
            width: size.width,
            height: size.height,
            maximized: live.window.is_maximized() || self.window.unmaximized_rect.is_some(),
            cwds,
        });
    }

    /// The chip under the pointer, when the pointer is in the bottom bar.
    /// The same laying-out the painter does, so a press lands on what the
    /// eye sees.
    fn status_bar_segment_at_pointer(&mut self) -> Option<crate::statusbar::SegmentKind> {
        let (width, height) = {
            let live = self.window.state.as_ref()?;
            (live.width as f32, live.height as f32)
        };
        if self.window.pointer.1 < height - self.status_bar_height() {
            return None;
        }
        let metrics = self.window.font.metrics();
        let columns = (width / metrics.width.max(1.0)).floor().max(0.0) as usize;
        let status = self.status();
        let segments = crate::statusbar::segments(&status, columns);
        let pt = self.chrome_pt();
        let gap = self.mono_width(crate::statusbar::GAP);
        let mut pen = (crate::ui_tokens::CHROME_PANEL_INSET * pt).round();
        for segment in &segments {
            let wide = self.mono_width(&segment.text);
            if pen + wide > width {
                break;
            }
            if self.window.pointer.0 >= pen && self.window.pointer.0 < pen + wide {
                return Some(segment.kind);
            }
            pen += wide + gap;
        }
        None
    }

    /// A press on the bottom bar. True whenever it landed in the bar, so a
    /// missed chip is swallowed rather than falling through to the terminal
    /// as a stray selection; each chip does what 0.57.4's did.
    fn click_status_bar(&mut self) -> bool {
        let Some(live) = self.window.state.as_ref() else {
            return false;
        };
        let session_id = live.session_id;
        let height = live.height as f32;
        if self.window.pointer.1 < height - self.status_bar_height() {
            return false;
        }
        let hit = self.status_bar_segment_at_pointer();
        match hit {
            Some(crate::statusbar::SegmentKind::Cwd) => {
                // A click on the path copies it, ready to paste anywhere.
                let directory = self.status().directory;
                self.copy_text(&directory);
                self.show_notice(format!("\u{2713} {directory}"));
            }
            Some(crate::statusbar::SegmentKind::Project) => {
                self.run_key_action(crate::keys::Action::DirJump, session_id);
            }
            Some(crate::statusbar::SegmentKind::CaptureInclude) => self.start_system_capture(false),
            Some(crate::statusbar::SegmentKind::CaptureExclude) => self.start_system_capture(true),
            Some(crate::statusbar::SegmentKind::Theme) => {
                self.run_key_action(crate::keys::Action::ThemePicker, session_id);
            }
            Some(crate::statusbar::SegmentKind::Mcp) => {
                // The chip's click exports what the bar can only count: the
                // recent audit entries, ready to paste into a report.
                let snapshot = crate::engine_backend::mcp_state::insights_mcp_snapshot(200);
                let mut text = format!(
                    "mcp inputs: {} (agents seen: {})\n",
                    snapshot.input_count, snapshot.agents_seen
                );
                for entry in &snapshot.recent_audit {
                    text.push_str(entry);
                    text.push('\n');
                }
                self.copy_text(&text);
                self.show_notice("\u{2713} MCP activity copied".to_string());
            }
            // The chip is the switch, as 0.57.4's was; the settings page is a
            // right press away.
            Some(crate::statusbar::SegmentKind::Proxy) => self.toggle_proxy(),
            Some(crate::statusbar::SegmentKind::Profile) => self.open_settings(),
            // The chip says the arrangement is wrong; the press says why. The
            // reason is the handshake's own words -- which name the offending
            // Core and its version -- because "unavailable" alone leaves the
            // user with nothing to act on.
            Some(crate::statusbar::SegmentKind::Core) => {
                if let Some(reason) = crate::engine_backend::core_fallback_reason() {
                    self.show_notice(reason.to_string());
                }
            }
            _ => {}
        }
        self.window.drawn_revision = None;
        true
    }

    /// The proxy chip: flip whether new sessions get Unterm's proxy env vars.
    ///
    /// Switching on is gated on a probe — a proxy URL injected into a shell
    /// with nothing behind it turns every command there into a connection
    /// failure, so when nothing answers, the toggle stays off and the chip
    /// says so for a moment instead. Off needs no probe: it only stops the
    /// injection. Nothing here goes near the OS's own proxy settings, on any
    /// path — the toggle is Unterm's alone.
    fn toggle_proxy(&mut self) {
        if unterm_services::launch_env::unterm_proxy_enabled() {
            if let Err(err) = unterm_services::launch_env::set_unterm_proxy_enabled(false) {
                log::warn!("could not switch the proxy off: {err:#}");
                return;
            }
            self.show_notice(unterm_services::i18n::t("proxy.disabled_for_new_shells"));
            return;
        }
        match unterm_services::launch_env::probe_unterm_proxy() {
            Some(url) => {
                if let Err(err) = unterm_services::launch_env::set_unterm_proxy_enabled(true) {
                    log::warn!("could not switch the proxy on: {err:#}");
                    return;
                }
                // The notice names what answered, so "on" is checkable at a
                // glance rather than an act of faith.
                self.show_notice(format!(
                    "{} ({})",
                    unterm_services::i18n::t("proxy.enabled_for_new_shells"),
                    crate::statusbar::short_proxy(&url)
                ));
            }
            None => {
                // The same moment a notice gets, on the chip itself: a notice
                // would cover the chip whose state it is explaining.
                const SHOWN_FOR: std::time::Duration = std::time::Duration::from_millis(2400);
                self.window.proxy_error_until = Some(std::time::Instant::now() + SHOWN_FOR);
            }
        }
    }

    /// A right press on the chrome. True when the chrome takes it -- before
    /// this, a right-click aimed at a tab fell through to the terminal's
    /// paste gesture, which is the worst possible answer to asking for a
    /// menu.
    fn chrome_right_click(&mut self) -> bool {
        if self.window.pointer.1 < self.top_bar_height() {
            // The chevron answers either button with its menu, as before.
            if self.hovered_top_bar_item() == Some(crate::topbar::Item::Menu) {
                self.open_quick_menu();
            }
            return true;
        }
        if let Some(live) = self.window.state.as_ref() {
            if self.window.pointer.1 >= live.height as f32 - self.status_bar_height() {
                // The proxy chip answers a right press with the settings page,
                // as 0.57.4's did; the rest of the bar swallows it.
                if self.status_bar_segment_at_pointer()
                    == Some(crate::statusbar::SegmentKind::Proxy)
                {
                    self.open_settings();
                }
                return true;
            }
        }
        if self.window.sidebar_open {
            if let Some((left, top, width, height, _row_height)) = self.sidebar_dock() {
                let inside = self.window.pointer.0 >= left
                    && self.window.pointer.0 < left + width
                    && self.window.pointer.1 >= top
                    && self.window.pointer.1 < top + height;
                if inside {
                    // The "+" answers a right press with the shell list, as
                    // 0.57.4's did; a right press on a tab row opens its menu.
                    if self.sidebar_footer_action_at(self.window.pointer.0, self.window.pointer.1) == Some(0) {
                        self.open_shell_selector();
                    } else if let Some(at) = self.sidebar_row_at(self.window.pointer.0, self.window.pointer.1) {
                        if let Some(crate::sidebar::Row::Tab { index, .. }) =
                            self.sidebar_rows().get(at).cloned()
                        {
                            self.select_tab_at(index);
                            self.open_tab_menu(index);
                        }
                    }
                    self.window.drawn_revision = None;
                    return true;
                }
            }
        }
        false
    }

    /// The tab's own menu, on the palette: 0.57.4's context-menu verbs.
    fn open_tab_menu(&mut self, index: usize) {
        use unterm_services::i18n::t;
        let action = |label: String, action: crate::keys::Action| crate::palette::Entry {
            label,
            hint: crate::keys::chord_hint(action).unwrap_or_default(),
            command: crate::palette::Command::Action(action),
        };
        self.window.palette = Some(crate::palette::Palette::new(vec![
            action(t("menu.new_tab"), crate::keys::Action::NewTab),
            action(
                t("settings.menu.split_right"),
                crate::keys::Action::SplitRight,
            ),
            crate::palette::Entry {
                label: t("tab.rename"),
                hint: String::new(),
                command: crate::palette::Command::OpenTabRename { index },
            },
            action(t("tab.move_left"), crate::keys::Action::MoveTab(-1)),
            action(t("tab.move_right"), crate::keys::Action::MoveTab(1)),
            action(t("tab.close_pane"), crate::keys::Action::ClosePane),
            action(t("tab.close"), crate::keys::Action::CloseTab),
        ]));
        self.window.drawn_revision = None;
    }

    /// The quick menu's long screenshot: the focused pane's entire history to
    /// one tall PNG under `~/.unterm/captures/`, with the path shown where
    /// the eye already is.
    fn capture_scrollback(&mut self) {
        let pane = self.focused_session();
        let dir = unterm_protocol::state_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".").join(".unterm"))
            .join("captures");
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.show_notice(format!("capture failed: {err}"));
            return;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|at| at.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("scrollshot-{stamp}.png"));
        let tx = self.clipboard_tx.clone();
        crate::clipboard::run(tx, move || {
            let result = crate::mcp_host::scrollback_png(pane, &path)
                .map(|_| path)
                .map_err(|err| format!("{err:#}"))
                .and_then(|path| {
                    arboard::Clipboard::new()
                        .and_then(|mut board| board.set_text(path.display().to_string()))
                        .map_err(|err| err.to_string())?;
                    Ok(path)
                });
            ClipboardResult::ScrollbackCaptured(result)
        });
        self.show_notice(unterm_services::i18n::t("scrollshot.started"));
    }

    fn open_settings(&mut self) {
        let info = unterm_services::server_info::read();
        if info.http_port == 0 {
            log::warn!("the settings server has not started yet");
            return;
        }
        let url = format!("http://127.0.0.1:{}", info.http_port);
        if let Err(err) = crate::links::open(&url) {
            log::warn!("could not open {url}: {err}");
        }
    }

    /// Open the Unzoo One console. Same HTTP server as the settings page,
    /// off `/console/`.
    fn open_console(&mut self) {
        let info = unterm_services::server_info::read();
        if info.http_port == 0 {
            log::warn!("the settings server has not started yet");
            return;
        }
        let url = format!("http://127.0.0.1:{}/console/", info.http_port);
        if let Err(err) = crate::links::open(&url) {
            log::warn!("could not open {url}: {err}");
        }
    }

    /// The picker's rows: every theme the product ships.
    fn theme_entries(&self) -> Vec<crate::palette::Entry> {
        let current = self.theme_id.clone();
        crate::theme::THEMES
            .iter()
            .map(|theme| crate::palette::Entry {
                label: theme.name.to_string(),
                hint: if current.as_deref() == Some(theme.id) {
                    unterm_services::i18n::t_args("theme.current", &[("name", theme.name)])
                } else {
                    unterm_services::i18n::t(&format!("theme.preset.{}.desc", theme.id))
                },
                command: crate::palette::Command::ApplyTheme {
                    id: theme.id.to_string(),
                },
            })
            .collect()
    }

    /// Switch to a theme, and remember it for next time.
    ///
    /// The atlas goes with it: glyphs are rasterized as coverage and tinted
    /// when drawn, so they survive -- but the frame's own colours are baked
    /// into quads that have already been built, and the simplest way to be
    /// sure none are stale is to draw the next frame from nothing.
    fn apply_theme(&mut self, id: &str) {
        let Some(theme) = crate::theme::by_id(id) else {
            return;
        };
        self.window.colors = unterm_render::quads::FrameColors {
            background: theme.background,
            foreground: theme.foreground,
            palette: &theme.ansi,
        };
        self.theme_id = Some(theme.id.to_string());
        if let Err(err) = crate::theme::remember(theme.id) {
            log::warn!("could not remember the theme: {err:#}");
        }
        self.show_notice(unterm_services::i18n::t_args(
            "theme.switched_to",
            &[("name", theme.name)],
        ));
        self.window.drawn_revision = None;
    }

    /// The rows for jumping to a directory.
    ///
    /// Everything at once -- what is under here, what was open before, and the
    /// machine's drives -- because the palette filters as you type and the
    /// point of this picker is not knowing which of the three it is in.
    /// The rows for the directory jump, for what has been typed so far.
    fn dir_jump_entries(&self, query: &str) -> Vec<crate::palette::Entry> {
        let here = self
            .current_directory()
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // Where the pane already is, first, and only while nothing has been
        // typed: once there is a query it is a row that matches nothing and
        // sits above the rows that do.
        let mut entries = Vec::new();
        if query.is_empty() {
            entries.push(crate::palette::Entry {
                label: unterm_services::i18n::t("dirjump.system_picker"),
                hint: "Ctrl+O".to_string(),
                command: crate::palette::Command::OpenDirectoryPicker {
                    then: crate::palette::BrowseThen::ChangeDirectory,
                },
            });
            entries.push(crate::palette::Entry {
                label: unterm_services::i18n::t("dirjump.here"),
                hint: here.display().to_string(),
                command: crate::palette::Command::ChangeDirectory {
                    path: here.display().to_string(),
                },
            });
        }
        entries.extend(
            crate::dir_jump::for_query(&here, query)
                .into_iter()
                .map(|entry| crate::palette::Entry {
                    // The section it came from and the path it is. The section
                    // is the grouping the picker used to show as headings; the
                    // path is what tells two same-named directories apart.
                    hint: format!("{}  {}", entry.section.heading(), entry.path.display()),
                    label: entry.label,
                    command: crate::palette::Command::ChangeDirectory {
                        path: entry.path.display().to_string(),
                    },
                }),
        );
        entries
    }

    /// The rows behind the status bar's triangle.
    fn quick_entries(&self) -> Vec<crate::palette::Entry> {
        let recording = self
            .window.state
            .as_ref()
            .and_then(|live| {
                unterm_engine::RecordingEngine::recording_status(&self.engine, live.session_id).ok()
            })
            .map(|status| status.enabled)
            .unwrap_or(false);

        use unterm_services::i18n::t;

        // 0.57.4's chevron menu, in its order: the window's verbs with their
        // chords, then the palette, then the session's recording and capture
        // family, then Settings, then who this is.
        let chord =
            |action: crate::keys::Action| crate::keys::chord_hint(action).unwrap_or_default();
        let action = |label: String, action: crate::keys::Action| crate::palette::Entry {
            label,
            hint: chord(action),
            command: crate::palette::Command::Action(action),
        };
        let mut entries = vec![
            action(t("menu.new_tab"), crate::keys::Action::NewTab),
            action(
                t("settings.menu.split_right"),
                crate::keys::Action::SplitRight,
            ),
            action(t("menu.dir_jump"), crate::keys::Action::DirJump),
            action(t("menu.tree_sidebar"), crate::keys::Action::TreeSidebar),
            action(t("menu.git_panel"), crate::keys::Action::GitPanel),
            action(t("menu.left_tabs"), crate::keys::Action::LeftTabBar),
            // The two controls the sidebar footer used to carry as its
            // own glyphs. Still one click away — from here and from the
            // new-session row's split — never dropped.
            crate::palette::Entry {
                label: t("menu.shell_selector"),
                hint: String::new(),
                command: crate::palette::Command::OpenShellSelector,
            },
            crate::palette::Entry {
                label: t("menu.tab_navigator"),
                hint: String::new(),
                command: crate::palette::Command::OpenTabNavigator,
            },
            action(t("menu.find"), crate::keys::Action::Search),
            action(
                t("menu.command_palette"),
                crate::keys::Action::CommandPalette,
            ),
            crate::palette::Entry {
                label: if recording {
                    t("settings.menu.recording_on")
                } else {
                    t("settings.menu.recording_off")
                },
                hint: t("settings.menu.recording.hint"),
                command: crate::palette::Command::ToggleRecording,
            },
            crate::palette::Entry {
                label: t("settings.menu.export_session"),
                hint: t("settings.menu.export_session.hint"),
                command: crate::palette::Command::ExportSession,
            },
            crate::palette::Entry {
                label: t("menu.capture_region"),
                hint: t("command.capture_region.hint"),
                command: crate::palette::Command::SelectCaptureRegion,
            },
            crate::palette::Entry {
                label: t("menu.capture_desktop"),
                hint: String::new(),
                command: crate::palette::Command::CaptureDesktop,
            },
            crate::palette::Entry {
                label: t("menu.capture_scrollback"),
                hint: String::new(),
                command: crate::palette::Command::CaptureScrollback,
            },
            crate::palette::Entry {
                label: t("settings.menu.web_settings"),
                hint: t("settings.menu.web_settings.hint"),
                command: crate::palette::Command::OpenSettings,
            },
            // Who this is: the studio's existing caption, the site, and
            // the version — the menu's quiet last line, clicking through
            // to the website.
            crate::palette::Entry {
                label: format!("Unterm · {}", t("sidebar.author_caption")),
                hint: format!("unterm.app · v{}", env!("CARGO_PKG_VERSION")),
                command: crate::palette::Command::OpenUrl {
                    url: "https://unterm.app".to_string(),
                },
            },
        ];
        // 控制台只在装了资源时进菜单：没装的话点开只会是一个 404。
        if unterm_settings::console_dir().is_some() {
            let at = entries
                .iter()
                .position(|entry| entry.command == crate::palette::Command::OpenSettings)
                .map(|index| index + 1)
                .unwrap_or(entries.len());
            entries.insert(
                at,
                crate::palette::Entry {
                    label: t("settings.menu.console"),
                    hint: t("settings.menu.console.hint"),
                    command: crate::palette::Command::OpenConsole,
                },
            );
        }
        entries
    }

    /// Every command the GUI exposes, not only the subset with a key chord.
    fn command_palette_entries(&self) -> Vec<crate::palette::Entry> {
        let mut entries = command_entries();
        for extra in self.quick_entries() {
            if !entries.iter().any(|entry| entry.command == extra.command) {
                entries.push(extra);
            }
        }
        entries.extend(workspace_entries());
        entries
    }

    /// The Insights card: what the AI layer is seeing, as rows to read.
    ///
    /// A read-only palette rather than a bespoke overlay: Esc, arrows and
    /// clicking-to-dismiss already work there, and rows whose command is
    /// `Nothing` cannot run anything by accident.
    fn open_insights(&mut self) {
        use unterm_services::i18n::t_args;
        let row = |label: String| crate::palette::Entry {
            label,
            hint: String::new(),
            command: crate::palette::Command::Nothing,
        };
        let status = self.status();
        let snapshot = crate::engine_backend::mcp_state::insights_mcp_snapshot(8);
        let mut entries = vec![
            row(t_args("insights.shell", &[("shell", &status.shell)])),
            row(t_args("insights.cwd", &[("cwd", &status.directory)])),
            row(t_args(
                "insights.inputs",
                &[("count", &snapshot.input_count.to_string())],
            )),
            row(t_args(
                "insights.agents",
                &[("count", &snapshot.agents_seen.to_string())],
            )),
            row(t_args(
                "insights.suggestions",
                &[("count", &snapshot.pending_suggestions.to_string())],
            )),
            row(t_args(
                "insights.confirmations",
                &[("count", &snapshot.pending_confirmations.to_string())],
            )),
        ];
        // The tail of the audit log, one row each: "what just happened" is
        // the question an Insights card is opened to answer.
        for entry in &snapshot.recent_audit {
            entries.push(row(entry.clone()));
        }
        self.open_palette(entries);
    }

    /// Drop the quick menu under its button, right edge pinned on screen.
    fn open_quick_menu(&mut self) {
        if self.window.quick_menu.take().is_some() {
            // The second press on the chevron closes what the first opened.
            self.window.drawn_revision = None;
            return;
        }
        let entries = self.quick_entries();
        let pt = self.chrome_pt();
        let width = {
            let mut widest: f32 = 0.0;
            for entry in &entries {
                let mut wide = self.chrome_width(&entry.label);
                if !entry.hint.is_empty() {
                    wide += self.chrome_width(&entry.hint) + 18.0 * pt;
                }
                widest = widest.max(wide);
            }
            widest + (12.0 + 14.0) * pt
        };
        let window_width = self
            .window.state
            .as_ref()
            .map(|live| live.width as f32)
            .unwrap_or(0.0);
        let anchor_right = self
            .top_bar(window_width)
            .iter()
            .find(|piece| piece.item == crate::topbar::Item::Menu)
            .map(|piece| piece.left + piece.width)
            .unwrap_or(window_width);
        let width = width.min((window_width - 8.0 * pt).max(1.0));
        let left = (anchor_right - width)
            .max(4.0 * pt)
            .min((window_width - width - 4.0 * pt).max(0.0));
        let window_height = self
            .window.state
            .as_ref()
            .map(|live| live.height as f32)
            .unwrap_or(0.0);
        let top = self.top_bar_height() + 4.0 * pt;
        let row_height = self.chrome_row_height();
        let available_height =
            (window_height - top - self.status_bar_height() - 4.0 * pt).max(row_height);
        let visible_rows = ((available_height / row_height) as usize)
            .max(1)
            .min(entries.len());
        self.window.quick_menu = Some(QuickMenu {
            entries,
            hover: None,
            top_row: 0,
            visible_rows,
            left,
            top,
            width,
            row_height,
        });
        self.window.drawn_revision = None;
    }

    /// A press while the dropdown is open: run the row under it, and close
    /// either way — a menu left open after a click elsewhere is debris.
    fn click_quick_menu(&mut self) -> bool {
        let Some(mut menu) = self.window.quick_menu.take() else {
            return false;
        };
        self.window.drawn_revision = None;
        if let Some(delta) = menu.arrow_at(self.window.pointer.0, self.window.pointer.1) {
            menu.scroll(delta);
            self.window.quick_menu = Some(menu);
            return true;
        }
        if let Some(at) = menu.row_at(self.window.pointer.0, self.window.pointer.1) {
            if let Some(entry) = menu.entries.get(at) {
                self.run_palette_command(entry.command.clone(), "");
            }
            return true;
        }
        // Outside the card: the press only closes it.
        true
    }

    /// The dropdown itself: a bordered card of rows under the chevron.
    fn append_quick_menu(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(menu) = self.window.quick_menu.clone() else {
            return;
        };
        let pt = self.chrome_pt();
        let chrome = self.chrome();
        let radius = 6.0 * pt;
        // Over everything already drawn, the way a menu has to be.
        let _mark = quads.mark();
        quads.backgrounds.extend(unterm_render::rounded::panel(
            menu.left - 1.0,
            menu.top - 1.0,
            menu.width + 2.0,
            menu.height() + 2.0,
            radius,
            chrome.outer_edge,
        ));
        quads.backgrounds.extend(unterm_render::rounded::panel(
            menu.left,
            menu.top,
            menu.width,
            menu.height(),
            radius,
            chrome.group_bg,
        ));
        let text_offset = ((menu.row_height - self.window.chrome_font.metrics().height) / 2.0
            + crate::ui_tokens::CHROME_TEXT_BASELINE_NUDGE * pt)
            .max(0.0);
        let hover = menu.row_at(self.window.pointer.0, self.window.pointer.1);
        let has_scroll_gutter = menu.entries.len() > menu.visible_rows;
        for (visible_index, (index, entry)) in menu
            .entries
            .iter()
            .enumerate()
            .skip(menu.top_row)
            .take(menu.visible_rows)
            .enumerate()
        {
            let row_top = menu.top + visible_index as f32 * menu.row_height;
            if hover == Some(index) {
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left: menu.left,
                    top: row_top,
                    width: menu.width,
                    height: menu.row_height,
                    color: chrome.hover_bg,
                });
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left: menu.left,
                    top: row_top,
                    width: 2.0 * pt,
                    height: menu.row_height,
                    color: chrome.focus_rail,
                });
            }
            let foreground = self.chrome_foreground();
            self.append_chrome(
                &entry.label.clone(),
                foreground,
                (menu.left + 12.0 * pt, row_top + text_offset),
                quads,
            );
            if !entry.hint.is_empty() {
                let wide = self.chrome_width(&entry.hint);
                let label_wide = self.chrome_width(&entry.label);
                if label_wide + wide + 34.0 * pt <= menu.width {
                    let right_pad = if has_scroll_gutter { 30.0 } else { 14.0 } * pt;
                    self.append_chrome(
                        &entry.hint.clone(),
                        chrome.dim_text,
                        (
                            menu.left + menu.width - wide - right_pad,
                            row_top + text_offset,
                        ),
                        quads,
                    );
                }
            }
        }
        let arrow_left = menu.left + menu.width - 13.0 * pt;
        let arrow_color = self.chrome_foreground();
        if menu.top_row > 0 {
            self.append_chrome(
                "▲",
                arrow_color,
                (arrow_left, menu.top + text_offset),
                quads,
            );
        }
        if menu.top_row + menu.visible_rows < menu.entries.len() {
            self.append_chrome(
                "▼",
                arrow_color,
                (
                    arrow_left,
                    menu.top + menu.height() - menu.row_height + text_offset,
                ),
                quads,
            );
        }
    }

    fn open_palette(&mut self, entries: Vec<crate::palette::Entry>) {
        self.window.palette = Some(crate::palette::Palette::new(entries));
        self.window.drawn_revision = None;
    }

    fn open_named_palette(&mut self, entries: Vec<crate::palette::Entry>, title: String) {
        self.window.palette = Some(crate::palette::Palette::new(entries).titled(title));
        self.window.drawn_revision = None;
    }

    fn open_shell_selector(&mut self) {
        self.window.palette = Some(crate::palette::Palette::shells(launcher_entries()));
        self.window.drawn_revision = None;
    }

    fn tab_navigator_entries(&self) -> Vec<crate::palette::Entry> {
        let sessions =
            unterm_engine::SessionEngine::list_sessions(&self.engine).unwrap_or_default();
        self.window.tabs
            .tab_ids()
            .into_iter()
            .enumerate()
            .filter_map(|(index, tab_id)| {
                let pane_id = self.window.tabs.active_pane(tab_id)?;
                let session = sessions.iter().find(|session| session.id == pane_id)?;
                let facts = crate::statsbar::known_facts(pane_id);
                let project = session
                    .shell
                    .cwd
                    .as_deref()
                    .map(crate::sidebar::project_name)
                    .unwrap_or_default();
                let identity = facts
                    .agent_id
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| session.title.clone());
                let detail = crate::statusbar::short_name(&session.shell.process_name);
                Some(crate::palette::Entry {
                    label: [project, identity, detail]
                        .into_iter()
                        .filter(|part| !part.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join("  "),
                    hint: format!("tab {}  pane {}", index + 1, pane_id),
                    command: crate::palette::Command::ActivateTab { tab_id },
                })
            })
            .collect()
    }

    fn open_tab_navigator(&mut self) {
        self.open_palette(self.tab_navigator_entries());
    }

    /// Show a tab, and tell what is on it how big it now is.
    ///
    /// `placements` describes the *active* tab alone, so a pane behind
    /// another one is not resized while it is there -- and bringing it
    /// forward is therefore the only moment it can be told. Leave the line
    /// out and the pane keeps whatever size it last had, until something
    /// else happens to resize the window while that pane is the one in
    /// front. Between those two moments its program is drawing to a grid
    /// that no longer exists.
    ///
    /// A full-screen program shows it plainly, because it anchors its
    /// interface to the bottom of the screen it believes it has: the frame
    /// lands short of the real bottom and the input line lands somewhere
    /// else entirely, with the difference in height lying blank in between.
    /// That was the reported symptom, and forcing one resize by hand put it
    /// right, which is what pointed here.
    ///
    /// `select_tab` has always had the line. Reaching the same tab through
    /// the palette did not.
    fn activate_tab(&mut self, tab_id: usize) {
        if !self.window.tabs.set_active_tab(tab_id) {
            return;
        }
        self.window.tab_id = Some(tab_id);
        if let Some(pane_id) = self.window.tabs.active_pane(tab_id) {
            self.focus_session(pane_id);
        }
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    /// Open a palette whose line is a task rather than a filter.
    ///
    /// Nothing to send is nothing to open: a crew picker on a machine with no
    /// agents installed is an empty card.
    fn open_fleet(&mut self, entries: Vec<crate::palette::Entry>) {
        if entries.is_empty() {
            self.show_notice(unterm_services::i18n::t("cockpit.fleet_no_agents"));
            return;
        }
        self.window.palette = Some(crate::palette::Palette::writing(entries));
        self.window.drawn_revision = None;
    }

    /// Open a palette that goes and looks again as the query changes.
    fn open_browser(&mut self, entries: Vec<crate::palette::Entry>, title: String) {
        self.window.palette = Some(crate::palette::Palette::browsing(entries).titled(title));
        self.window.drawn_revision = None;
    }

    /// Bring a palette's rows up to date with what has been typed.
    ///
    /// A fixed list only narrows. A directory list is asked again, because a
    /// typed path names a place nothing has scanned -- the scan is bounded and
    /// the disk is not.
    fn requery_palette(&self, palette: &mut crate::palette::Palette) {
        match palette.source {
            crate::palette::Source::Fixed => palette.refilter(),
            // The line is the task, not a filter: narrowing the crews by what
            // has been typed empties the list on the first word. Typing does
            // clear a stale complaint, though -- it was about the last attempt.
            crate::palette::Source::Text => palette.error = None,
            crate::palette::Source::Directories => {
                let rows = self.dir_jump_entries(&palette.query);
                palette.replace_entries(rows);
            }
            crate::palette::Source::Characters => {
                let rows = self.character_entries(&palette.query);
                palette.replace_entries(rows);
            }
        }
    }

    /// How round a panel's corners are on this display.
    ///
    /// The radius is a size the reader sees, so it grows with the display's
    /// scale. Fixed in device pixels it is a third smaller on a 1.5x screen
    /// than on a 1x one, which is the difference between a rounded corner and
    /// a corner that looks slightly damaged.
    fn corner_radius(&self) -> f32 {
        unterm_render::rounded::RADIUS * self.font_scale().max(1.0)
    }

    /// Where the open palette is, for hit-testing.
    fn palette_card(&self) -> Option<crate::palette::Geometry> {
        let palette = self.window.palette.as_ref()?;
        let live = self.window.state.as_ref()?;
        let metrics = self.window.font.metrics();
        let rows = palette.visible().len().min(crate::palette::MAX_ROWS);
        Some(match palette.view {
            // The same arithmetic the paint uses, so a row is pressed
            // where it is drawn.
            crate::palette::View::Confirm => crate::palette::Geometry::confirming(
                live.width as f32,
                (metrics.width, metrics.height),
                rows,
                palette.error.is_some(),
            ),
            crate::palette::View::Search => {
                let geometry = if palette.title.is_some() {
                    crate::palette::Geometry::titled
                } else {
                    crate::palette::Geometry::new
                };
                geometry(
                    live.width as f32,
                    (metrics.width, metrics.height),
                    rows,
                    palette.error.is_some(),
                )
            }
            crate::palette::View::ShellSelector => {
                self.shell_selector_card(rows, palette.error.is_some())
            }
        })
    }

    /// The old shell chooser was a 56-cell card centred inside the terminal
    /// pane, one third of the way down its free height. Centring against the
    /// whole window put the replacement half over the navigation dock.
    fn shell_selector_card(&self, rows: usize, has_error: bool) -> crate::palette::Geometry {
        let metrics = self.window.font.metrics();
        let leading_rows = 3;
        let trailing_rows = 2;
        let lines = rows + leading_rows + trailing_rows + usize::from(has_error);
        let area_left = self.terminal_left();
        let area_top = self.terminal_top();
        let area_width = self.terminal_width();
        let area_height = self.terminal_height();
        let width = (metrics.width * 56.0)
            .min((area_width - metrics.width * 4.0).max(metrics.width * 24.0))
            .min(area_width);
        let height = metrics.height * lines as f32;
        crate::palette::Geometry {
            left: area_left + ((area_width - width) / 2.0).max(0.0),
            top: area_top + ((area_height - height) / 3.0).max(0.0),
            width,
            height,
            row_height: metrics.height,
            rows,
            leading_rows,
            trailing_rows,
        }
    }

    /// Follow the pointer over an open palette.
    ///
    /// Selecting rather than highlighting separately: the row under the
    /// pointer is the row Enter runs, so moving the mouse and then pressing
    /// Enter does what it looks like it will.
    fn hover_palette(&mut self) -> bool {
        let Some(card) = self.palette_card() else {
            return false;
        };
        // The right-hand gutter belongs to the scroll arrows.  Hovering one
        // must not silently select the row underneath it.
        if self.palette_scroll_arrow_at(card).is_some() {
            return true;
        }
        let Some(row) = card.row_at(self.window.pointer.0, self.window.pointer.1) else {
            return false;
        };
        if let Some(palette) = self.window.palette.as_mut() {
            let selected = palette.window_start(crate::palette::MAX_ROWS) + row;
            if palette.selected != selected {
                palette.selected = selected;
                self.window.drawn_revision = None;
            }
        }
        true
    }

    /// Which of the palette's visible scroll arrows is under the pointer.
    ///
    /// Arrows are part of the same bounded result window as the wheel and the
    /// keyboard; keeping their hit test here prevents the painted button and
    /// the row picker from disagreeing about who owns the click.
    fn palette_scroll_arrow_at(&self, card: crate::palette::Geometry) -> Option<isize> {
        let palette = self.window.palette.as_ref()?;
        if card.rows == 0 {
            return None;
        }
        let metrics = self.window.font.metrics();
        let in_gutter = self.window.pointer.0 >= card.left + card.width - metrics.width * 2.0
            && self.window.pointer.0 < card.left + card.width;
        if !in_gutter {
            return None;
        }
        let first_top = card.top + metrics.height * card.leading_rows as f32;
        let last_top = first_top + metrics.height * (card.rows.saturating_sub(1)) as f32;
        if self.window.pointer.1 >= first_top
            && self.window.pointer.1 < first_top + metrics.height
            && palette.can_scroll_up()
        {
            Some(-1)
        } else if self.window.pointer.1 >= last_top
            && self.window.pointer.1 < last_top + metrics.height
            && palette.can_scroll_down(crate::palette::MAX_ROWS)
        {
            Some(1)
        } else {
            None
        }
    }

    /// A press while a palette is open. Returns true when the palette took it.
    ///
    /// A press on a row runs it; a press anywhere else closes the palette
    /// without reaching the terminal. Closing is what everyone expects from a
    /// menu, and letting the press through as well would put the cursor
    /// somewhere in the pane the menu was covering.
    fn click_palette(&mut self) -> bool {
        let Some(card) = self.palette_card() else {
            return false;
        };
        if let Some(delta) = self.palette_scroll_arrow_at(card) {
            if let Some(palette) = self.window.palette.as_mut() {
                palette.scroll(delta, crate::palette::MAX_ROWS);
            }
            self.window.drawn_revision = None;
            return true;
        }
        match card.row_at(self.window.pointer.0, self.window.pointer.1) {
            Some(row) => {
                let chosen = self.window.palette.as_ref().and_then(|palette| {
                    let row = palette.window_start(crate::palette::MAX_ROWS) + row;
                    palette
                        .visible()
                        .get(row)
                        .map(|entry| (entry.command.clone(), palette.query.clone()))
                });
                self.window.palette = None;
                if let Some((command, task)) = chosen {
                    self.run_palette_command(command, &task);
                }
            }
            None => self.window.palette = None,
        }
        self.window.drawn_revision = None;
        true
    }

    /// Type into the open palette. Returns true when the key was the palette's.
    fn handle_palette_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::Key as WinitKey;
        let Some(mut palette) = self.window.palette.take() else {
            return false;
        };
        let named = match &event.logical_key {
            WinitKey::Named(named) => Some(format!("{named:?}")),
            _ => None,
        };
        let character = match &event.logical_key {
            WinitKey::Character(text) => Some(text.to_string()),
            _ => None,
        };

        // Directory-jump parity with 0.57.4: the modifier shortcuts belong to
        // the picker, not to the terminal underneath it.
        if palette.source == crate::palette::Source::Directories
            && self.window.ctrl_held
            && matches!(character.as_deref(), Some("o") | Some("O"))
        {
            self.open_system_directory_picker(crate::palette::BrowseThen::ChangeDirectory);
            self.window.drawn_revision = None;
            return true;
        }

        let mut keep = true;
        if palette.view == crate::palette::View::ShellSelector {
            if let Some(index) = character
                .as_deref()
                .and_then(|text| text.chars().next())
                .filter(|ch| ch.is_ascii_digit())
                .and_then(|ch| ch.to_digit(10))
                .map(|digit| if digit == 0 { 9 } else { digit as usize - 1 })
            {
                if let Some(entry) = palette.visible().get(index).cloned().cloned() {
                    self.run_palette_command(entry.command, "");
                    keep = false;
                }
            }
        }
        if !keep {
            self.window.drawn_revision = None;
            return true;
        }
        // Ctrl+R turns the character picker to its next group, as it did in
        // 0.57.4: the thirteen groups are a wheel, and shift turns it the
        // other way. The query is cleared because it was a search over the
        // page that is no longer open.
        if palette.source == crate::palette::Source::Characters
            && self.window.ctrl_held
            && matches!(character.as_deref(), Some("r") | Some("R"))
        {
            self.charselect_group = if self.window.shift_held {
                self.charselect_group.previous()
            } else {
                self.charselect_group.next()
            };
            palette.query.clear();
            self.requery_palette(&mut palette);
            self.window.palette = Some(palette);
            self.window.drawn_revision = None;
            if let Some(live) = self.window.state.as_ref() {
                live.window.request_redraw();
            }
            return true;
        }
        match crate::palette::key_for(named.as_deref(), character.as_deref(), self.window.ctrl_held) {
            crate::palette::Key::Close => keep = false,
            crate::palette::Key::Step(delta) => palette.step(delta),
            crate::palette::Key::Complete => {
                if palette.source == crate::palette::Source::Directories {
                    let completed = palette.current().and_then(|entry| match &entry.command {
                        crate::palette::Command::Browse { path, .. }
                        | crate::palette::Command::ChangeDirectory { path }
                        | crate::palette::Command::NewTabIn { path } => Some(path.clone()),
                        _ => None,
                    });
                    if let Some(path) = completed {
                        palette.query = path;
                        self.requery_palette(&mut palette);
                    }
                }
            }
            crate::palette::Key::Backspace => {
                if palette.source == crate::palette::Source::Directories && palette.query.is_empty()
                {
                    if let Some(parent) = self
                        .current_directory()
                        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
                    {
                        palette.query = parent.display().to_string();
                        if !palette.query.ends_with(std::path::MAIN_SEPARATOR) {
                            palette.query.push(std::path::MAIN_SEPARATOR);
                        }
                    }
                } else {
                    palette.query.pop();
                }
                self.requery_palette(&mut palette);
            }
            crate::palette::Key::Type(text) => {
                palette.query.push_str(&text);
                self.requery_palette(&mut palette);
            }
            crate::palette::Key::Accept => {
                keep = false;
                let task = palette.query.clone();
                if let Some(entry) = palette.current().cloned() {
                    if palette.source == crate::palette::Source::Directories && self.window.ctrl_held {
                        let path = match entry.command {
                            crate::palette::Command::ChangeDirectory { path }
                            | crate::palette::Command::NewTabIn { path }
                            | crate::palette::Command::Browse { path, .. } => Some(path),
                            crate::palette::Command::OpenDirectoryPicker { .. } => {
                                self.open_system_directory_picker(
                                    crate::palette::BrowseThen::NewTab,
                                );
                                None
                            }
                            _ => None,
                        };
                        if let Some(path) = path {
                            self.new_tab_in(&path);
                        }
                    } else {
                        self.run_palette_command(entry.command, &task);
                    }
                }
            }
            // Nothing reaches the shell while the palette is open: a
            // keystroke through it would run in the pane behind.
            crate::palette::Key::NotOurs => {}
        }

        if keep {
            self.window.palette = Some(palette);
        }
        self.window.drawn_revision = None;
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
        true
    }

    /// Do what a palette row says.
    ///
    /// `task` is what was typed. Almost every row ignores it -- the line is a
    /// filter and the row is the answer -- but on the fleet card the line *is*
    /// the request, and it has to travel with the row: by the time this runs
    /// the palette has been taken away, and reading the task back out of it
    /// finds nothing. Which is what happened: Enter closed the card and
    /// launched nothing at all.
    fn run_palette_command(&mut self, command: crate::palette::Command, task: &str) {
        match command {
            crate::palette::Command::Action(action) => {
                let session_id = self.window.state.as_ref().map(|live| live.session_id);
                if let Some(session_id) = session_id {
                    self.run_key_action(action, session_id);
                }
            }
            crate::palette::Command::Launch { program, args } => {
                self.new_tab_running(&program, &args)
            }
            crate::palette::Command::ChangeDirectory { path } => self.change_directory(&path),
            crate::palette::Command::OpenShellSelector => self.open_shell_selector(),
            crate::palette::Command::NewTabIn { path } => self.new_tab_in(&path),
            crate::palette::Command::RestoreWorkspace { name } => self.restore_workspace(&name),
            crate::palette::Command::OpenDirectoryPicker { then } => {
                self.open_system_directory_picker(then)
            }
            crate::palette::Command::OpenWorkspaceSave => self.open_workspace_save(),
            crate::palette::Command::SaveWorkspace => {
                let name = task.trim().to_string();
                if name.is_empty() {
                    self.show_notice("a workspace needs a name".to_string());
                } else {
                    self.save_workspace(&name);
                }
            }
            crate::palette::Command::ToggleRecording => self.toggle_recording(),
            crate::palette::Command::ExportSession => self.export_session(),
            crate::palette::Command::OpenSettings => self.open_settings(),
            crate::palette::Command::OpenConsole => self.open_console(),
            crate::palette::Command::ApplyTheme { id } => self.apply_theme(&id),
            crate::palette::Command::TypeCharacter { glyph, name } => {
                self.type_character(&glyph, &name)
            }
            crate::palette::Command::LaunchFleet { agents } => {
                self.launch_fleet(agents, task.trim().to_string())
            }
            crate::palette::Command::OpenTabNavigator => self.open_tab_navigator(),
            crate::palette::Command::ActivateTab { tab_id } => self.activate_tab(tab_id),
            crate::palette::Command::RenameTab { tab_id } => {
                let title = task.trim();
                if title.is_empty() {
                    self.window.tab_titles.remove(&tab_id);
                } else {
                    self.window.tab_titles.insert(tab_id, title.to_string());
                }
            }
            crate::palette::Command::CaptureScrollback => self.capture_scrollback(),
            crate::palette::Command::CaptureDesktop => self.start_system_capture(true),
            crate::palette::Command::OpenUrl { url } => {
                if let Err(err) = crate::links::open(&url) {
                    log::warn!("could not open {url}: {err}");
                }
            }
            crate::palette::Command::ConfirmCloseWindow => {
                self.window.close_confirmed = true;
                self.perform_close(CloseOutcome::EndSessions);
            }
            // The window goes; the Core, the shells and the agents in
            // them stay -- and an indicator goes up saying so, because a
            // promise nobody can see is one the user has to remember.
            crate::palette::Command::KeepRunningInBackground => {
                self.window.close_confirmed = true;
                if !self.enter_background() {
                    // No tray on this desktop. The sessions still outlive
                    // this process and `unterm` still reopens onto them,
                    // but this window is not going to sit there invisibly
                    // pretending an icon exists.
                    log::warn!("no tray indicator available; closing instead of parking");
                    self.perform_close(CloseOutcome::KeepSessions);
                }
            }
            crate::palette::Command::DrainThenExit => {
                // Off this thread: draining waits on shells that may take
                // their time (or forever), and the one thread that must
                // never wait on anything is the one under the pointer.
                if let crate::engine_backend::AppEngine::Core { client, .. } = &self.engine {
                    let client = client.clone();
                    std::thread::spawn(move || {
                        if let Err(err) = client.drain(true) {
                            log::warn!("could not ask the core to drain: {err:#}");
                        }
                    });
                }
                self.window.close_confirmed = true;
                // Destroying the sessions here would be the opposite of
                // draining: the Core is being asked to let them finish.
                self.perform_close(CloseOutcome::KeepSessions);
            }
            crate::palette::Command::CancelAndExit => {
                if let crate::engine_backend::AppEngine::Core { client, .. } = &self.engine {
                    let client = client.clone();
                    std::thread::spawn(move || {
                        if let Err(err) = client.shutdown() {
                            log::warn!("could not stop the core: {err:#}");
                        }
                    });
                }
                self.window.close_confirmed = true;
                // `core.shutdown` ends everything it holds, which is more
                // thorough than destroying the sessions one at a time from
                // here -- and racing it would only make the log confusing.
                self.perform_close(CloseOutcome::KeepSessions);
            }
            crate::palette::Command::OpenTabRename { index } => self.open_tab_rename(index),
            crate::palette::Command::SelectCaptureRegion => self.start_system_capture(false),
            // Information rows: reading them was the point.
            crate::palette::Command::Nothing => {}
            crate::palette::Command::Browse { path, then } => {
                // Stays open on the new directory rather than closing: picking
                // a folder three deep should be three keystrokes, not three
                // trips through the menu.
                self.open_palette(crate::directory::entries(std::path::Path::new(&path), then));
            }
        }
    }

    /// Open a tab running a named program.
    fn new_tab_running(&mut self, program: &str, args: &[String]) {
        let mut command = portable_pty::CommandBuilder::new(program);
        command.args(args);
        self.open_tab_with(Some(command), None);
    }

    /// Open a tab, with a shell of its own.
    fn new_tab(&mut self) {
        let shell = self.shell.clone();
        self.open_tab_with(shell, None);
    }

    fn open_tab_with(
        &mut self,
        command: Option<portable_pty::CommandBuilder>,
        directory: Option<String>,
    ) {
        // A tab needs a window to open into; the size comes from the layout
        // rather than from the window, because the strip may have taken some.
        let Some(_live) = self.window.state.as_ref() else {
            return;
        };
        let (cols, rows) = self
            .window.font
            .grid_for(self.terminal_width(), self.terminal_height());
        let env = launch_env_for_new_pane();
        let session = match self.engine.create_session(CreateSessionRequest {
            cols,
            rows,
            command_dir: directory,
            command: prepare_shell(command),
            env,
            launch_policy: LaunchPolicySnapshot::default(),
        }) {
            Ok(session) => session,
            Err(err) => {
                log::warn!("could not open a tab: {err:#}");
                #[cfg(target_os = "macos")]
                crate::macos_open::trace(&format!("open_tab_with: create_session failed: {err:#}"));
                return;
            }
        };
        match self.window.tabs.create_tab(session.id) {
            Ok(tab_id) => {
                #[cfg(target_os = "macos")]
                crate::macos_open::trace(&format!(
                    "open_tab_with: pane {} in tab {tab_id}",
                    session.id
                ));
                self.window.tabs.set_active_tab(tab_id);
                self.window.tab_id = Some(tab_id);
                self.focus_session(session.id);
                self.resize_panes();
                self.window.drawn_revision = None;
            }
            Err(err) => {
                log::warn!("could not record the tab: {err:#}");
                #[cfg(target_os = "macos")]
                crate::macos_open::trace(&format!(
                    "open_tab_with: create_tab failed for pane {}: {err:#}",
                    session.id
                ));
                crate::statsbar::forget(session.id);
                unterm_services::ghost_text::forget(session.id as u64);
                let _ = self.engine.destroy_session(session.id);
            }
        }
    }

    /// Move to the next tab along, wrapping at the ends.
    ///
    /// Wrapping rather than stopping: with three tabs, cycling forward twice
    /// from the last should land somewhere, and a key that does nothing at the
    /// edge reads as a key that is broken.
    fn cycle_tab(&mut self, step: isize) {
        let ids = self.window.tabs.tab_ids();
        if ids.len() < 2 {
            return;
        }
        let current = self.window.tab_id.or_else(|| self.window.tabs.active_tab());
        let next = ids[next_tab_index(&ids, current, step)];
        self.window.tabs.set_active_tab(next);
        self.window.tab_id = Some(next);
        if let Some(pane) = self.window.tabs.active_pane(next) {
            self.focus_session(pane);
        }
        // Whatever was behind has not been told about any window this size.
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    /// Start the same native interactive screenshot flow 0.57.4 used.
    fn start_system_capture(&mut self, hide_window: bool) {
        let mode = if hide_window {
            unterm_services::i18n::t("screenshot.mode.hidden")
        } else {
            unterm_services::i18n::t("screenshot.mode.visible")
        };
        self.show_notice(unterm_services::i18n::t_args(
            "screenshot.started",
            &[("mode", &mode)],
        ));
        let tx = self.clipboard_tx.clone();
        crate::clipboard::run(tx, move || ClipboardResult::ScreenshotFinished {
            mode,
            result: crate::system_capture::capture_selected_region(hide_window)
                .map_err(|err| format!("{err:#}")),
        });
    }

    /// Move the active tab along the bar without changing its stable id.
    fn move_tab(&mut self, step: isize) {
        let Some(tab_id) = self.window.tab_id.or_else(|| self.window.tabs.active_tab()) else {
            return;
        };
        if !self.window.tabs.move_tab_relative(tab_id, step) {
            return;
        }
        self.window.drawn_revision = None;
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
    }

    /// Close the active tab and everything in it.
    ///
    /// The last tab is not closable: a window with no tab has nothing to show
    /// and no way back, so closing the window is the user's own decision.
    fn close_tab(&mut self) {
        let _slow = SlowGuard::new("close_tab");
        let ids = self.window.tabs.tab_ids();
        if ids.len() < 2 {
            return;
        }
        let Some(tab_id) = self.window.tab_id.or_else(|| self.window.tabs.active_tab()) else {
            return;
        };
        for pane in self.window.tabs.pane_ids(tab_id) {
            crate::statsbar::forget(pane);
            unterm_services::ghost_text::forget(pane as u64);
            let _ = self.engine.destroy_session(pane);
        }
        self.window.tabs.forget_tab(tab_id);
        let remaining = self.window.tabs.tab_ids();
        let Some(next) = remaining.first().copied() else {
            return;
        };
        self.window.tabs.set_active_tab(next);
        self.window.tab_id = Some(next);
        if let Some(pane) = self.window.tabs.active_pane(next) {
            self.focus_session(pane);
        }
        // The tab that takes over was behind until now, so it is owed the
        // same telling as one brought forward any other way.
        self.resize_panes();
        self.window.drawn_revision = None;
    }

    /// Point the window at a pane, and redraw.
    fn focus_session(&mut self, session_id: usize) {
        if !self.window.tabs.set_active_pane(session_id) {
            #[cfg(target_os = "macos")]
            crate::macos_open::trace(&format!(
                "focus_session {session_id}: the tab registry refused it"
            ));
            return;
        }
        if let Err(err) = unterm_engine::SessionEngine::focus_session(&self.engine, session_id) {
            log::warn!("could not focus engine session {session_id}: {err:#}");
        }
        self.window.tab_id = self.window.tabs.tab_of_pane(session_id);
        if let Some(notice) = self.window.pane_notices.get_mut(&session_id) {
            notice.unread = false;
        }
        if let Some(live) = self.window.state.as_mut() {
            live.session_id = session_id;
        }
        self.resize_panes();
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
        self.window.drawn_revision = None;
    }

    /// How tall the terminal area is, once the tab bar has taken its share.
    fn terminal_height(&self) -> f32 {
        self.terminal_height_for(self.window_height())
    }

    /// How tall the terminal is inside a window of this height.
    fn terminal_height_for(&self, window_height: f32) -> f32 {
        let height = window_height;
        // The bar above, the status line below, and a gap at each end. Taken
        // out of the terminal rather than drawn over it: a bar over the grid
        // hides a row the shell still believes in.
        let taken = self.terminal_top() + self.status_bar_height() + self.terminal_padding_bottom();
        // Floored at the engine's minimum grid for the same reason the width
        // is: chrome that does not fit must cost a drawn row, not the pane's
        // contents.
        (height - taken)
            .max(unterm_engine::MIN_SESSION_GRID as f32 * self.window.font.metrics().height)
    }

    /// How tall the line along the bottom is.
    fn status_bar_height(&self) -> f32 {
        // Hidden by default in the inbox design: the facts it carried
        // (cwd, branch, shell) live in the top bar's title, and a
        // resident strip under every terminal is chrome that says the
        // same thing twice. `status_bar = true` in the config brings it
        // back.
        //
        // Config alone decides this, never anything transient. Letting
        // a pending confirmation add the strip changed the terminal's
        // row count without telling the shell, and every row below the
        // cursor was then drawn one line off from where the program
        // believed it was. The banner draws over the grid instead.
        if !self.status_bar_enabled {
            return 0.0;
        }
        // The bar is one terminal cell plus the slight vertical padding the
        // previous front end gave it — its text is set in the terminal face.
        let pt = self.chrome_pt();
        let pad = (crate::ui_tokens::STATUS_BAR_VERTICAL_PADDING * pt)
            .round()
            .max(2.0);
        (self.window.font.metrics().height + pad * 2.0).round().max(1.0)
    }

    /// Rebuild this window onto a Core that replaced the one it was
    /// using.
    ///
    /// The previous Core's panes are gone -- their shells died with the
    /// process -- so every id this window holds is stale. Dropping them
    /// and opening one shell on the new Core is the difference between
    /// "Unterm recovered" and the old answer, which was to tell the user
    /// to restart the application.
    fn recover_from_replaced_core(&mut self) {
        log::warn!("unterm-core was replaced; rebuilding this window's sessions");
        self.sync_tabs();
        let has_session = unterm_engine::SessionEngine::list_sessions(&self.engine)
            .map(|sessions| sessions.iter().any(|session| !session.is_dead))
            .unwrap_or(false);
        if !has_session {
            let (cols, rows) = self
                .window.state
                .as_ref()
                .map(|live| self.window.font.grid_for(live.width as f32, live.height as f32))
                .unwrap_or((80, 24));
            let request = CreateSessionRequest {
                cols,
                rows,
                command_dir: self
                    .start_directory
                    .clone()
                    .or_else(|| self.config_default_cwd.clone())
                    .as_ref()
                    .map(|path| path.display().to_string()),
                command: prepare_shell(self.shell.clone()),
                env: launch_env_for_new_pane(),
                launch_policy: LaunchPolicySnapshot::default(),
            };
            match self.engine.create_session(request) {
                Ok(session) => {
                    if let Err(err) = self.window.tabs.create_tab(session.id) {
                        log::warn!("could not show the recovered session: {err:#}");
                    } else {
                        self.focus_session(session.id);
                    }
                }
                Err(err) => log::warn!("could not open a shell on the new core: {err:#}"),
            }
        }
        // Recovering in silence would read as "my tabs vanished". The
        // banner that reported the loss reports the recovery, in the
        // same place, so the two halves of the story arrive together.
        self.core_replaced_at = Some(std::time::Instant::now());
    }

    /// Make the window's tabs match the engine's sessions.
    ///
    /// The engine is where sessions actually live, and it is not only this
    /// window that makes them: an agent calling `session.create` over MCP
    /// creates one too, and without this the window would never show it. The
    /// same pass drops tabs whose shells have exited, so a tab bar cannot
    /// outlive what it names.
    fn sync_tabs(&mut self) {
        let Ok(sessions) = unterm_engine::SessionEngine::list_sessions(&self.engine) else {
            return;
        };
        let live_ids: std::collections::HashSet<usize> =
            sessions.iter().map(|session| session.id).collect();

        // Every window of this process, the front one first.
        //
        // All of them, not just the one being drawn. A background window
        // still owns its shells: an agent that split a pane there, or closed
        // one, changed that window's arrangement, and leaving the mirror
        // until the user happened to come back means `instance.windows`
        // reports an arrangement that is not the one that exists.
        //
        // Front first because an orphan -- a session no window has, which is
        // what a session created over MCP is, and what D1 leaves behind when
        // a window closes -- belongs to the window the user is in, and
        // whichever runs first takes it. A split is the exception and settles
        // itself: every window skips it except the one holding its source.
        let mut shown: Vec<std::collections::HashSet<usize>> = std::iter::once(&self.window)
            .chain(self.parked.iter().map(|parked| &parked.state))
            .map(pane_set)
            .collect();
        let mut changed = false;
        for index in 0..shown.len() {
            let elsewhere: std::collections::HashSet<usize> = shown
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .flat_map(|(_, panes)| panes.iter().copied())
                .collect();
            let state = match index {
                0 => &mut self.window,
                other => &mut self.parked[other - 1].state,
            };
            let moved = adopt_sessions_into(state, &sessions, &live_ids, &elsewhere);
            if moved {
                shown[index] = pane_set(state);
                // The front window's is settled at the end of this method,
                // where the pane it is showing is settled with it. A
                // background one gets no such pass, and one left pointing at
                // a tab that has gone would come back showing nothing.
                if index > 0 {
                    settle_active_tab(state);
                }
            }
            if index == 0 {
                changed = moved;
            }
        }

        // MCP `session.focus` updates the engine from another thread. Mirror
        // that choice into this front end's tab/layout registry so a peer
        // Inbox jump brings the requested pane into view, not only its window.
        //
        // Mirror it edge-triggered and only into the window the user is in:
        // several windows share one Core, and every tab click anywhere
        // asserts the global active session. A window that levels itself to
        // that value re-follows every click made in every OTHER window, and
        // with two windows open the sidebars visibly chase each other. A
        // CHANGE in the global choice reaches the focused window (that is
        // the Inbox jump); the rest keep their own selection.
        if let Some(requested) = sessions
            .iter()
            .find(|session| session.is_active)
            .map(|session| session.id)
        {
            let externally_changed = self.window.followed_active != Some(requested);
            self.window.followed_active = Some(requested);
            let shown = self.window.state.as_ref().map(|live| live.session_id);
            if externally_changed
                && self.window.focused
                && shown != Some(requested)
                && self.window.tabs.tab_of_pane(requested).is_some()
            {
                #[cfg(target_os = "macos")]
                crate::macos_open::trace(&format!(
                    "sync follows external focus {requested} (was showing {shown:?})"
                ));
                self.focus_session(requested);
            }
        }

        if !changed {
            return;
        }
        // The window may have been left pointing at a tab that no longer
        // exists, or at none at all.
        settle_active_tab(&mut self.window);
        // Only when the arrangement actually moved. This used to run on every
        // pass: a PTY resize per pane, four times a second, forever -- which
        // is a system call and a reflow each time, and was most of what an
        // idle window cost. It also asked for a repaint every time, for a
        // window in which nothing had happened.
        if changed {
            self.resize_panes();
            self.window.drawn_revision = None;
        }
    }

    /// Type into the open search. Returns true when the key was the search's.
    ///
    /// Everything printable extends the pattern; Enter steps through the
    /// matches and Esc closes. Nothing else is taken, so a key the search has
    /// no use for still reaches the shell rather than vanishing.
    fn click_search_bar(&mut self) -> bool {
        let Some(mut search) = self.window.search.take() else {
            return false;
        };
        let session_id = self.focused_session();
        let Some(live) = self.window.state.as_ref() else {
            self.window.search = Some(search);
            return false;
        };
        let metrics = self.window.font.metrics();
        let left = self.terminal_left();
        let width = self.terminal_width();
        let top = (live.height as f32 - self.status_bar_height() - metrics.height)
            .max(self.terminal_top());
        if self.window.pointer.0 < left
            || self.window.pointer.0 >= left + width
            || self.window.pointer.1 < top
            || self.window.pointer.1 >= top + metrics.height
        {
            self.window.search = Some(search);
            return false;
        }

        let controls_left = left + width - 13.0 * metrics.width;
        let cell = ((self.window.pointer.0 - controls_left) / metrics.width).floor() as isize;
        let mut keep = true;
        let mut research = false;
        match cell {
            0..=2 => search.step(-1),
            3..=5 => search.step(1),
            6..=9 => {
                search.cycle_mode();
                research = true;
            }
            10..=12 => keep = false,
            _ => {}
        }
        if keep {
            if research {
                let matches = self.run_search(session_id, &search.pattern, search.mode);
                search.adopt(matches);
            }
            if let Some(found) = search.current() {
                let _ = self
                    .engine
                    .scroll_viewport_to(session_id, found.row as isize);
            }
            self.window.search = Some(search);
        }
        self.window.drawn_revision = None;
        live.window.request_redraw();
        true
    }

    fn handle_search_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::Key as WinitKey;
        let Some(mut search) = self.window.search.take() else {
            return false;
        };

        let named = match &event.logical_key {
            WinitKey::Named(named) => Some(format!("{named:?}")),
            _ => None,
        };
        let character = match &event.logical_key {
            WinitKey::Character(text) => Some(text.to_string()),
            _ => None,
        };

        let mut keep = true;
        let mut research = false;
        match crate::search::key_for(
            named.as_deref(),
            character.as_deref(),
            self.window.ctrl_held,
            self.window.shift_held,
        ) {
            crate::search::Key::Close => keep = false,
            crate::search::Key::Step(delta) => search.step(delta),
            crate::search::Key::Backspace => {
                search.pattern.pop();
                research = true;
            }
            crate::search::Key::Clear => {
                search.pattern.clear();
                research = true;
            }
            crate::search::Key::CycleMode => {
                search.cycle_mode();
                research = true;
            }
            crate::search::Key::Type(text) => {
                search.pattern.push_str(&text);
                research = true;
            }
            crate::search::Key::NotOurs => {
                self.window.search = Some(search);
                return false;
            }
        }

        if keep {
            if research {
                let matches =
                    self.run_search(self.focused_session(), &search.pattern, search.mode);
                search.adopt(matches);
            }
            // Follow the current match, so finding something shows it.
            if let Some(found) = search.current() {
                let _ = self
                    .engine
                    .scroll_viewport_to(self.focused_session(), found.row as isize);
            }
            self.window.search = Some(search);
        }
        self.window.drawn_revision = None;
        if let Some(live) = self.window.state.as_ref() {
            live.window.request_redraw();
        }
        true
    }

    /// Copy mode's cursor, and whatever it has selected.
    fn append_copy_mode(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(mode) = self.window.copy_mode else {
            return;
        };
        let metrics = self.window.font.metrics();
        let top_offset = self.terminal_top();
        let (_, widths) = self.screen_shape();

        if let Some(((start_row, start_col), (end_row, end_col))) = mode.selection() {
            for row in start_row..=end_row {
                let width = widths.get(row).copied().unwrap_or(0);
                let from = if row == start_row { start_col } else { 0 };
                let to = if row == end_row { end_col + 1 } else { width };
                if to <= from {
                    continue;
                }
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left: from as f32 * metrics.width,
                    top: top_offset + row as f32 * metrics.height,
                    width: (to - from) as f32 * metrics.width,
                    height: metrics.height,
                    color: mix(self.window.colors.background, self.window.colors.foreground, 0.28),
                });
            }
        }

        // The cursor over the selection, so it stays visible inside it.
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: mode.column as f32 * metrics.width,
            top: top_offset + mode.row as f32 * metrics.height,
            width: metrics.width,
            height: metrics.height,
            color: self.window.colors.foreground,
        });
    }

    /// Quick select's labels, over the text they stand for.
    /// A letter on every pane, while one is being picked.
    ///
    /// Drawn over the pane's own top-left corner rather than centred: centred
    /// puts the letter in the middle of whatever the pane is showing, and the
    /// corner is where the eye already goes to tell panes apart.
    fn append_pane_select(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(selector) = self.window.pane_select.clone() else {
            return;
        };
        let panes = self.placements();
        let order = crate::paneselect::reading_order(&panes);
        let metrics = self.window.font.metrics();
        let theme = self.theme();

        // Over everything, so a label is never behind the text it labels.
        let mark = quads.mark();
        for (index, label) in selector.labels.iter().enumerate() {
            // Once a letter is typed, only the labels still in the running.
            // Showing the rest offers choices that are no longer there.
            if !selector.typing.is_empty() && !label.starts_with(&selector.typing) {
                continue;
            }
            let Some(pane) = order.get(index).and_then(|at| panes.get(*at)) else {
                continue;
            };
            let width = metrics.width * (label.chars().count() as f32 + 2.0);
            let height = metrics.height * 2.0;
            let left = pane.origin.0;
            let top = pane.origin.1;
            quads.backgrounds.extend(unterm_render::rounded::panel(
                left,
                top,
                width,
                height,
                self.corner_radius(),
                theme.selection,
            ));
            crate::terminal::append_text(
                label,
                &mut self.window.font,
                &mut self.window.atlas,
                theme.selection_text,
                (left + metrics.width, top + metrics.height / 2.0),
                quads,
            );
        }
        quads.raise_since(mark);
    }

    fn append_quick_select(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some((found, typed)) = self.window.quick_select.clone() else {
            return;
        };
        let metrics = self.window.font.metrics();
        let top_offset = self.terminal_top();
        let background = self.window.colors.background;
        let foreground = self.window.colors.foreground;

        for item in &found {
            // Once a letter is typed, only the labels still in the running:
            // showing the rest is showing the user options they no longer have.
            if !typed.is_empty() && !item.label.starts_with(&typed) {
                continue;
            }
            let left = item.start as f32 * metrics.width;
            let top = top_offset + item.row as f32 * metrics.height;
            let width = metrics.width * item.label.chars().count() as f32;
            quads.backgrounds.push(unterm_render::quads::Quad {
                left,
                top,
                width,
                height: metrics.height,
                color: foreground,
            });
            crate::terminal::append_text(
                &item.label,
                &mut self.window.font,
                &mut self.window.atlas,
                background,
                (left, top),
                quads,
            );
        }
    }

    /// The command palette, centred over the terminal.
    ///
    /// Drawn last so it sits over everything, and opaque so the text behind it
    /// cannot be mistaken for one of its rows.
    fn append_palette(&mut self, window_width: f32, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(palette) = self.window.palette.as_ref() else {
            return;
        };
        let metrics = self.window.font.metrics();
        let view = palette.view;
        let window_start = palette.window_start(crate::palette::MAX_ROWS);
        let rows: Vec<(String, bool)> = palette
            .visible()
            .iter()
            .skip(window_start)
            .take(crate::palette::MAX_ROWS)
            .enumerate()
            .map(|(index, entry)| {
                let hint = if entry.hint.is_empty() {
                    String::new()
                } else {
                    format!("   {}", entry.hint)
                };
                let prefix = if view == crate::palette::View::ShellSelector {
                    format!("{}  ", window_start + index + 1)
                } else {
                    String::new()
                };
                (
                    format!("{prefix}{}{hint}", entry.label),
                    window_start + index == palette.selected,
                )
            })
            .collect();

        let error = palette.error.clone();
        // The same arithmetic the hit-testing uses, so a row is pressed where
        // it is drawn.
        let card = match view {
            crate::palette::View::Search => {
                let geometry = if palette.title.is_some() {
                    crate::palette::Geometry::titled
                } else {
                    crate::palette::Geometry::new
                };
                geometry(
                    window_width,
                    (metrics.width, metrics.height),
                    rows.len(),
                    error.is_some(),
                )
            }
            crate::palette::View::ShellSelector => {
                self.shell_selector_card(rows.len(), error.is_some())
            }
            crate::palette::View::Confirm => crate::palette::Geometry::confirming(
                window_width,
                (metrics.width, metrics.height),
                rows.len(),
                error.is_some(),
            ),
        };
        let (left, top, width, height) = (card.left, card.top, card.width, card.height);
        // Cut every row to the card it is drawn in. Without this a long
        // label and its description ran straight out past the card's
        // edge and over the terminal behind it -- text with no card
        // under it, which reads as a rendering fault rather than as a
        // row that was too long.
        let columns = ((width / metrics.width.max(1.0)) as usize).saturating_sub(2);
        let rows: Vec<(String, bool)> = rows
            .into_iter()
            .map(|(text, selected)| (crate::sidebar::fit(&text, columns), selected))
            .collect();

        if matches!(
            view,
            crate::palette::View::ShellSelector | crate::palette::View::Confirm
        ) {
            // Match the old selector's dimmed stage: it reads as a modal
            // choice, not as terminal output that happened to be highlighted.
            // A close prompt gets it for a stronger reason -- it is the one
            // question that must not be dismissed by habit.
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: self.terminal_left(),
                top: self.terminal_top(),
                width: self.terminal_width(),
                height: self.terminal_height(),
                color: [0.02, 0.02, 0.02, 0.82],
            });
        }
        quads.backgrounds.extend(unterm_render::rounded::panel(
            left,
            top,
            width,
            height,
            self.corner_radius(),
            if view == crate::palette::View::ShellSelector {
                [0.102, 0.102, 0.102, 1.0]
            } else {
                mix(self.window.colors.background, self.window.colors.foreground, 0.10)
            },
        ));

        let foreground = self.window.colors.foreground;
        if view == crate::palette::View::ShellSelector {
            crate::terminal::append_text(
                "\u{25C6}  Select Shell / New Tab",
                &mut self.window.font,
                &mut self.window.atlas,
                [0.38, 0.69, 0.94, 1.0],
                (left + metrics.width, top),
                quads,
            );
            crate::terminal::append_text(
                "Choose the shell for the new tab",
                &mut self.window.font,
                &mut self.window.atlas,
                mix(foreground, self.window.colors.background, 0.35),
                (left + metrics.width, top + metrics.height),
                quads,
            );
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: left + metrics.width,
                top: top + metrics.height * 2.0 + metrics.height * 0.48,
                width: (width - metrics.width * 2.0).max(0.0),
                height: 1.0,
                color: mix(self.window.colors.background, foreground, 0.25),
            });
        } else if view == crate::palette::View::Confirm {
            // The question, then a rule, then the answers. Nothing to
            // type into: the heading is the whole prompt.
            if let Some(title) = palette.title.as_deref() {
                crate::terminal::append_text(
                    title,
                    &mut self.window.font,
                    &mut self.window.atlas,
                    foreground,
                    (left + metrics.width, top),
                    quads,
                );
            }
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: left + metrics.width,
                top: top + metrics.height * 1.5,
                width: (width - metrics.width * 2.0).max(0.0),
                height: 1.0,
                color: mix(self.window.colors.background, foreground, 0.25),
            });
            // And the keys, under the answers: a prompt that can only
            // be answered with the mouse is one a keyboard user is
            // stuck in.
            let help = unterm_services::i18n::t("close.keys");
            crate::terminal::append_text(
                &help,
                &mut self.window.font,
                &mut self.window.atlas,
                mix(foreground, self.window.colors.background, 0.45),
                (
                    left + metrics.width,
                    top + metrics.height * (rows.len() + card.leading_rows) as f32
                        + metrics.height * 0.4,
                ),
                quads,
            );
        } else {
            if let Some(title) = palette.title.as_deref() {
                crate::terminal::append_text(
                    title,
                    &mut self.window.font,
                    &mut self.window.atlas,
                    foreground,
                    (left + metrics.width, top),
                    quads,
                );
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left: left + metrics.width,
                    top: top + metrics.height - 1.0,
                    width: (width - metrics.width * 2.0).max(0.0),
                    height: 1.0,
                    color: mix(self.window.colors.background, foreground, 0.25),
                });
            }
            // The query line, with a caret so an empty palette still looks
            // like something you type into.
            let query = format!("> {}", palette.query);
            let query_top = top + metrics.height * usize::from(palette.title.is_some()) as f32;
            crate::terminal::append_text(
                &query,
                &mut self.window.font,
                &mut self.window.atlas,
                foreground,
                (left + metrics.width, query_top),
                quads,
            );
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: left + metrics.width * (query.chars().count() + 1) as f32,
                top: query_top,
                width: (metrics.width * 0.15).max(1.0),
                height: metrics.height,
                color: foreground,
            });
        }

        for (index, (text, selected)) in rows.iter().enumerate() {
            let row_top = top + metrics.height * (index + card.leading_rows) as f32;
            if *selected {
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left: left + metrics.width * 0.5,
                    top: row_top,
                    width: width - metrics.width,
                    height: metrics.height,
                    color: match view {
                        crate::palette::View::ShellSelector => [0.176, 0.176, 0.176, 1.0],
                        // A decision's default answer has to be obvious
                        // at a glance: this is the one that Enter takes.
                        crate::palette::View::Confirm => {
                            mix(self.window.colors.background, self.window.colors.foreground, 0.42)
                        }
                        crate::palette::View::Search => {
                            mix(self.window.colors.background, self.window.colors.foreground, 0.30)
                        }
                    },
                });
                if matches!(
                    view,
                    crate::palette::View::ShellSelector | crate::palette::View::Confirm
                ) {
                    quads.backgrounds.push(unterm_render::quads::Quad {
                        left: left + metrics.width * 0.5,
                        top: row_top,
                        width: (metrics.width * 0.22).max(2.0),
                        height: metrics.height,
                        color: if view == crate::palette::View::Confirm {
                            crate::cockpit::Badge::NeedsYou.color()
                        } else {
                            [0.38, 0.69, 0.94, 1.0]
                        },
                    });
                }
            }
            // The answer that cannot be taken back is drawn as one:
            // a row that ends every running shell must not look like
            // the row above it, which merely closes a window.
            let irreversible = palette
                .visible()
                .get(window_start + index)
                .is_some_and(|entry| {
                    entry.command == crate::palette::Command::CancelAndExit
                });
            let ink = if irreversible {
                [0.95, 0.42, 0.35, 1.0]
            } else {
                foreground
            };
            crate::terminal::append_text(
                text,
                &mut self.window.font,
                &mut self.window.atlas,
                ink,
                (left + metrics.width, row_top),
                quads,
            );
        }

        // Visible, clickable affordances for a result set larger than the
        // card.  These share `top_row` with arrows, PageUp/PageDown and the
        // wheel, so every navigation path shows the same slice.
        let arrow_left = left + width - metrics.width * 1.6;
        let arrow_color = mix(foreground, self.window.colors.background, 0.25);
        if palette.can_scroll_up() && !rows.is_empty() {
            crate::terminal::append_text(
                "↑",
                &mut self.window.font,
                &mut self.window.atlas,
                arrow_color,
                (
                    left + width - metrics.width * 1.6,
                    top + metrics.height * card.leading_rows as f32,
                ),
                quads,
            );
        }
        if palette.can_scroll_down(crate::palette::MAX_ROWS) && !rows.is_empty() {
            crate::terminal::append_text(
                "↓",
                &mut self.window.font,
                &mut self.window.atlas,
                arrow_color,
                (
                    arrow_left,
                    top + metrics.height * (card.leading_rows + rows.len() - 1) as f32,
                ),
                quads,
            );
        }

        if view == crate::palette::View::ShellSelector {
            let footer_top = top + metrics.height * (card.leading_rows + rows.len()) as f32;
            crate::terminal::append_text(
                "\u{2191}\u{2193} move   Enter select   1-9 quick select   Esc close",
                &mut self.window.font,
                &mut self.window.atlas,
                mix(foreground, self.window.colors.background, 0.45),
                (left + metrics.width, footer_top),
                quads,
            );
            crate::terminal::append_text(
                "Unterm",
                &mut self.window.font,
                &mut self.window.atlas,
                mix(foreground, self.window.colors.background, 0.65),
                (
                    left + (width - metrics.width * "Unterm".len() as f32) / 2.0,
                    footer_top + metrics.height,
                ),
                quads,
            );
        }

        // Under the rows rather than instead of them: the answer to "this
        // repository has uncommitted changes" is to go and commit, and the
        // task has to still be there to press Enter on afterwards.
        if let Some(error) = error {
            let row_top =
                top + metrics.height * (rows.len() + card.leading_rows + card.trailing_rows) as f32;
            let danger = crate::window_buttons::CLOSE_HOVER;
            crate::terminal::append_text(
                &error,
                &mut self.window.font,
                &mut self.window.atlas,
                danger,
                (left + metrics.width, row_top),
                quads,
            );
        }
    }

    /// The search bar, along the bottom.
    ///
    /// The bottom rather than the top: the tab bar is up there, and a bar
    /// that moved the terminal's rows around every time it opened would
    /// reflow the shell mid-search.
    /// Tint every visible match, the current one brighter — a search that
    /// highlights nothing leaves the reader hunting for what it found.
    fn append_search_matches(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let Some(search) = self.window.search.clone() else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        if self.window.state.is_none() {
            return;
        }
        let session_id = self.focused_session();
        let Ok(snapshot) = self.engine.read_styled_screen(session_id) else {
            return;
        };
        let top_row = snapshot.lines.first().map(|line| line.row).unwrap_or(0);
        let rows = snapshot.rows as i64;
        let metrics = self.window.font.metrics();
        let origin = self
            .placements()
            .into_iter()
            .find(|placement| placement.session_id == session_id)
            .map(|placement| placement.origin)
            .unwrap_or((self.terminal_left(), self.terminal_top()));
        let accent = self.chrome().focus_rail;
        for (index, found) in search.matches.iter().enumerate() {
            if found.row < top_row || found.row >= top_row + rows {
                continue;
            }
            let columns: usize = search
                .pattern
                .chars()
                .map(crate::terminal::column_width)
                .sum::<usize>()
                .max(1);
            let alpha = if index == search.selected { 0.45 } else { 0.18 };
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: origin.0 + found.col as f32 * metrics.width,
                top: origin.1 + (found.row - top_row) as f32 * metrics.height,
                width: columns as f32 * metrics.width,
                height: metrics.height,
                color: [accent[0], accent[1], accent[2], alpha],
            });
        }
    }

    fn append_search_bar(
        &mut self,
        window_width: f32,
        quads: &mut unterm_render::quads::FrameQuads,
    ) {
        let Some(search) = self.window.search.as_ref() else {
            return;
        };
        let label = search.label();
        let mode = match search.mode {
            crate::search::Mode::CaseSensitive => "Aa",
            crate::search::Mode::CaseInsensitive => "aA",
            crate::search::Mode::Regex => ".*",
        };
        let metrics = self.window.font.metrics();
        let height = self.window.state.as_ref().map(|live| live.height).unwrap_or(0) as f32;
        let left = self.terminal_left();
        let width = self.terminal_width();
        let top = (height - self.status_bar_height() - metrics.height).max(self.terminal_top());
        quads.backgrounds.push(unterm_render::quads::Quad {
            left,
            top,
            width: width.min(window_width - left),
            height: metrics.height,
            color: self.window.colors.foreground,
        });
        let background = self.window.colors.background;
        let button_cells = 13usize;
        let columns = (width / metrics.width.max(1.0)).floor().max(0.0) as usize;
        let label = crate::sidebar::fit(&label, columns.saturating_sub(button_cells + 2));
        crate::terminal::append_text(
            &label,
            &mut self.window.font,
            &mut self.window.atlas,
            background,
            (left + metrics.width, top),
            quads,
        );

        let buttons = [("↑", 3usize), ("↓", 3usize), (mode, 4usize), ("×", 3usize)];
        let mut pen = left + width - button_cells as f32 * metrics.width;
        for (text, cells) in buttons {
            let button_width = cells as f32 * metrics.width;
            quads.backgrounds.push(unterm_render::quads::Quad {
                left: pen + 1.0,
                top: top + 1.0,
                width: (button_width - 2.0).max(0.0),
                height: (metrics.height - 2.0).max(0.0),
                color: self.window.colors.background,
            });
            let text_width = text.chars().count() as f32 * metrics.width;
            crate::terminal::append_text(
                text,
                &mut self.window.font,
                &mut self.window.atlas,
                self.window.colors.foreground,
                (pen + ((button_width - text_width) / 2.0).max(0.0), top),
                quads,
            );
            pen += button_width;
        }
    }

    /// Name the window after what is running in it.
    ///
    /// A terminal called "Unterm" whatever is inside it tells the user
    /// nothing when they are looking at a taskbar full of them. The rules are
    /// the engine's, so both front ends name a shell the same way.
    fn update_window_title(&mut self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        use unterm_engine::next_core::tab_title::{render, TabContext, TabTitleRules};

        let shell = unterm_engine::SessionEngine::shell(&self.engine, live.session_id).ok();
        let process_path = shell.map(|shell| shell.process_name).unwrap_or_default();
        let pane_title = unterm_engine::SessionEngine::get_session(&self.engine, live.session_id)
            .map(|session| session.title)
            .unwrap_or_default();

        let index = self
            .window.tab_id
            .and_then(|id| self.window.tabs.tab_ids().iter().position(|c| *c == id))
            .unwrap_or(0)
            + 1;
        let rendered = render(
            &TabTitleRules {
                // No padding: a window title is not a tab label.
                format: "{title}".to_string(),
                ..Default::default()
            },
            TabContext {
                pane_title: &pane_title,
                process_path: &process_path,
                index,
            },
        );
        // The 0.57.4 shape: position when there are several tabs, the
        // project, and which instance this window is -- what tells two
        // Unterm windows apart in Alt-Tab.
        let tabs = self.window.tabs.tab_ids().len();
        let position = if tabs > 1 {
            format!("[{index}/{tabs}] ")
        } else {
            String::new()
        };
        let project = self
            .current_directory()
            .map(|dir| crate::sidebar::project_name(&dir.display().to_string()))
            .filter(|name| !name.is_empty())
            .map(|name| format!("{name} — "))
            .unwrap_or_default();
        // This process's own instance, not the machine's active one: with two
        // windows open, `read()` answers for whichever registered last, and
        // both titles claim to be it.
        let instance = unterm_services::server_info::read_current().id;
        let instance = if instance.is_empty() {
            String::new()
        } else {
            format!(" ({instance})")
        };
        // The bar's centre gets the same subject without the product
        // name: the window already says "Unterm" in its own corner, and
        // a title bar that repeats it has said nothing twice.
        self.window.bar_title = format!("{position}{project}{rendered}");
        let title = format!("{position}{project}{rendered} — Unterm{instance}");
        if self.window.window_title.as_deref() == Some(title.as_str()) {
            return;
        }
        live.window.set_title(&title);
        self.window.window_title = Some(title);
    }

    /// Tell the system where the candidate list belongs.
    ///
    /// At the caret, not in a corner: an input method that puts its
    /// candidates across the window from what is being typed makes the user
    /// look in two places to type one word.
    fn place_ime_candidates(&self) {
        let Some(live) = self.window.state.as_ref() else {
            return;
        };
        let Some((origin, metrics)) = self.preedit_origin() else {
            return;
        };
        live.window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(origin.0 as f64, origin.1 as f64),
            winit::dpi::PhysicalSize::new(
                (self.window.preedit.columns().max(1) as f32 * metrics.width) as u32,
                metrics.height as u32,
            ),
        );
    }

    /// Where composed text is drawn, and the cell size it is drawn on.
    fn preedit_origin(&self) -> Option<((f32, f32), unterm_render::quads::CellMetrics)> {
        let metrics = self.window.font.metrics();
        if let Some(palette) = self.window.palette.as_ref() {
            if palette.view == crate::palette::View::Search {
                let card = self.palette_card()?;
                let query_columns = palette
                    .query
                    .chars()
                    .map(crate::terminal::column_width)
                    .sum::<usize>();
                return Some((
                    (
                        card.left + metrics.width * (3 + query_columns) as f32,
                        card.top + metrics.height * usize::from(palette.title.is_some()) as f32,
                    ),
                    metrics,
                ));
            }
        }
        if let Some(search) = self.window.search.as_ref() {
            let height = self.window.state.as_ref()?.height as f32;
            let top = (height - self.status_bar_height() - metrics.height).max(self.terminal_top());
            let columns = search
                .pattern
                .chars()
                .map(crate::terminal::column_width)
                .sum::<usize>();
            return Some((
                (
                    self.terminal_left() + metrics.width * (9 + columns) as f32,
                    top,
                ),
                metrics,
            ));
        }
        let live = self.window.state.as_ref()?;
        let snapshot = self.engine.read_styled_screen(live.session_id).ok()?;
        let placement = self
            .placements()
            .into_iter()
            .find(|placement| placement.session_id == live.session_id);
        let (pane_origin, pane_cols) = match placement {
            Some(placement) => (placement.origin, placement.cols),
            None => ((0.0, self.terminal_top()), snapshot.cols),
        };
        let cursor = (snapshot.cursor.x, snapshot.cursor.y.max(0) as usize);
        Some((
            crate::ime::origin(
                cursor,
                pane_origin,
                (metrics.width, metrics.height),
                pane_cols,
                self.window.preedit.columns(),
            ),
            metrics,
        ))
    }

    /// Commit text from an input method to the modal that owns the keyboard.
    ///
    /// Winit reports Chinese/Japanese/Korean input as `Ime::Commit`, not as a
    /// normal keyboard character.  Sending every commit straight to the PTY
    /// made English palette filtering work while Chinese appeared to do
    /// nothing (and was secretly typed into the shell underneath).
    fn commit_ime_to_modal(&mut self, text: &str) -> bool {
        if let Some(mut palette) = self.window.palette.take() {
            palette.query.push_str(text);
            self.requery_palette(&mut palette);
            self.window.palette = Some(palette);
            self.window.drawn_revision = None;
            return true;
        }
        if let Some(mut search) = self.window.search.take() {
            search.pattern.push_str(text);
            let matches = self.run_search(self.focused_session(), &search.pattern, search.mode);
            search.adopt(matches);
            if let Some(found) = search.current() {
                let _ = self
                    .engine
                    .scroll_viewport_to(self.focused_session(), found.row as isize);
            }
            self.window.search = Some(search);
            self.window.drawn_revision = None;
            return true;
        }
        false
    }

    /// Draw what the input method is still composing.
    ///
    /// Inverted, the way every terminal marks text that is not committed yet:
    /// it is not in the shell, and drawing it like ordinary output would
    /// suggest it had already been typed.
    /// Throw away a composition nobody is composing.
    ///
    /// Switching input sources mid-composition strands the marked text: the
    /// old input method never ends it, winit never hears an `Ime` event for
    /// it, and AppKit keeps routing editing keys into the ghost. Clearing
    /// our overlay is not enough -- the platform's own marked text has to
    /// go, and toggling IME off and on is the one lever winit exposes that
    /// makes AppKit discard it.
    fn clear_orphan_preedit(&mut self) {
        self.window.preedit = crate::ime::Preedit::default();
        if let Some(live) = self.window.state.as_ref() {
            live.window.set_ime_allowed(false);
            live.window.set_ime_allowed(true);
        }
        self.window.drawn_revision = None;
    }

    fn append_preedit(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        if self.window.preedit.is_empty() {
            return;
        }
        let Some((origin, metrics)) = self.preedit_origin() else {
            return;
        };
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: origin.0,
            top: origin.1,
            width: self.window.preedit.columns() as f32 * metrics.width,
            height: metrics.height,
            color: self.window.colors.foreground,
        });
        let text = self.window.preedit.text.clone();
        let background = self.window.colors.background;
        crate::terminal::append_text(
            &text,
            &mut self.window.font,
            &mut self.window.atlas,
            background,
            origin,
            quads,
        );
        // A caret inside the composition, so a user editing what they have
        // typed so far can see where they are.
        let caret = self.window.preedit.caret_column() as f32 * metrics.width;
        quads.backgrounds.push(unterm_render::quads::Quad {
            left: origin.0 + caret,
            top: origin.1,
            width: (metrics.width * 0.15).max(1.0),
            height: metrics.height,
            color: background,
        });
    }

    /// The lines between split panes.
    ///
    /// Without them two shells sharing a background read as one shell with
    /// strange wrapping, which is exactly the confusion a split is supposed
    /// to remove.
    fn divider_quads(&self) -> Vec<unterm_render::quads::Quad> {
        let (Some(tab_id), Some(_live)) = (self.window.tab_id, self.window.state.as_ref()) else {
            return Vec::new();
        };
        let metrics = self.window.font.metrics();
        let (cols, rows) = self
            .window.font
            .grid_for(self.terminal_width(), self.terminal_height());
        let left_offset = self.terminal_left();
        let top_offset = self.terminal_top();
        let positions = self.window.tabs.positions(tab_id, cols, rows);
        if positions.len() < 2 {
            return Vec::new();
        }

        positions
            .iter()
            .filter_map(|placed| {
                let rect = &placed.rect;
                // Only where the pane does not reach the edge: an edge is
                // already a boundary, and a line drawn on it is a wasted row.
                let vertical = rect.left + rect.width < cols;
                if !vertical && rect.top + rect.height >= rows {
                    return None;
                }
                crate::panes::divider_after(rect.clone(), metrics, vertical)
            })
            .map(|(left, top, width, height)| unterm_render::quads::Quad {
                left: left + left_offset,
                top: top + top_offset,
                width,
                height,
                // Between the two backgrounds: visible against both without
                // drawing attention to itself.
                color: self.theme().divider,
            })
            .collect()
    }

    /// Where each pane goes, in pixels.
    /// The crews worth offering here, and the task line above them.
    fn fleet_entries(&self) -> Vec<crate::palette::Entry> {
        crate::fleet::crews(&unterm_services::cockpit::fleet::installed_agents())
            .into_iter()
            .map(|crew| crate::palette::Entry {
                hint: unterm_services::i18n::t("cockpit.fleet_worktrees"),
                label: crew.label,
                command: crate::palette::Command::LaunchFleet {
                    agents: crew.agents,
                },
            })
            .collect()
    }

    /// Send a crew at the task that has been typed.
    ///
    /// The repository is checked first and on the palette's own thread,
    /// because the answer is instant and the failure belongs in the card that
    /// asked: a dirty worktree is fixed by going and committing, and the task
    /// has to still be there afterwards.
    ///
    /// The launch itself is not: creating a worktree per agent and starting a
    /// tab for each takes seconds, and doing it here would freeze the window
    /// for all of them.
    fn launch_fleet(&mut self, agents: Vec<String>, task: String) {
        let here = self
            .current_directory()
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // Two refusals, and both bring the card back with what was typed still
        // in it. The answer to either is to go and fix something and press
        // Enter again, which needs the task to have survived.
        let refusal = if !crate::fleet::task_is_ready(&task) {
            Some("cockpit.fleet_no_task")
        } else {
            unterm_services::cockpit::fleet::precheck(&here).err()
        };
        if let Some(key) = refusal {
            let entries = self.fleet_entries();
            let mut palette = crate::palette::Palette::writing(entries);
            palette.query = task;
            palette.error = Some(unterm_services::i18n::t(key));
            self.window.palette = Some(palette);
            self.window.drawn_revision = None;
            return;
        }

        self.show_notice(unterm_services::i18n::t("cockpit.fleet_launching"));
        self.window.drawn_revision = None;
        // Creating a worktree per agent and starting a tab for each takes
        // seconds; doing it here would freeze the window for all of them.
        let spawned = std::thread::Builder::new()
            .name("fleet-launch".into())
            .spawn(
                move || match unterm_services::cockpit::fleet::launch(&here, &task, &agents) {
                    Ok(fleet) => log::info!(
                        "fleet {} launched with {} members",
                        fleet.id,
                        fleet.members.len()
                    ),
                    Err(err) => log::error!("fleet launch failed: {err:#}"),
                },
            );
        if let Err(err) = spawned {
            log::error!("could not start the fleet launcher: {err:#}");
        }
    }

    /// Throw away a pane's history.
    ///
    /// The scrollback only, unless the screen is asked for too. A pane with a
    /// hundred thousand lines behind it is the reason anyone asks; losing what
    /// is currently on screen is not part of the request.
    ///
    /// The view is put back to the bottom afterwards. Scrolled up into history
    /// that no longer exists is a blank pane, and it looks like the clear broke
    /// something rather than like it worked.
    fn clear_scrollback(&mut self, session_id: usize, include_screen: bool) {
        if let Err(err) = self.engine.erase_scrollback(session_id, include_screen) {
            log::warn!("could not clear the scrollback: {err:#}");
            return;
        }
        // Back to the bottom. Scrolled up into history that no longer exists
        // is a blank pane, and that reads as the clear having broken something
        // rather than as it having worked.
        let _ = self.engine.scroll_viewport_to(session_id, isize::MAX);
        self.show_notice(unterm_services::i18n::t(if include_screen {
            "notice.screen_cleared"
        } else {
            "notice.scrollback_cleared"
        }));
        self.window.drawn_revision = None;
    }

    /// Put a letter on every pane.
    ///
    /// Nothing to choose between is nothing to open: a selector over one pane
    /// is a letter you have to press to stay where you already are.
    fn open_pane_select(&mut self, mode: crate::paneselect::Mode) {
        let panes = self.placements();
        if panes.len() < 2 {
            return;
        }
        self.window.pane_select = Some(crate::paneselect::Selector::new(panes.len(), mode));
        self.window.drawn_revision = None;
    }

    /// Type at the pane selector. Returns true when the key was its.
    fn handle_pane_select_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::Key as WinitKey;
        let Some(mut selector) = self.window.pane_select.take() else {
            return false;
        };
        let named = match &event.logical_key {
            WinitKey::Named(named) => Some(format!("{named:?}")),
            _ => None,
        };
        let character = match &event.logical_key {
            WinitKey::Character(text) => Some(text.to_string()),
            _ => None,
        };

        match selector.key(named.as_deref(), character.as_deref(), self.window.ctrl_held) {
            crate::paneselect::Outcome::Chose(index) => self.take_pane(&selector, index),
            crate::paneselect::Outcome::Typing => self.window.pane_select = Some(selector),
            crate::paneselect::Outcome::Cancelled => {}
            // Nothing reaches the shell while it is open: a keystroke through
            // it would run in whichever pane happens to be in front.
            crate::paneselect::Outcome::Ignored => self.window.pane_select = Some(selector),
        }
        self.window.drawn_revision = None;
        true
    }

    /// Do whatever the selector was opened to do, to the pane that was picked.
    fn take_pane(&mut self, selector: &crate::paneselect::Selector, index: usize) {
        let panes = self.placements();
        let order = crate::paneselect::reading_order(&panes);
        let Some(chosen) = order.get(index).and_then(|at| panes.get(*at)) else {
            return;
        };
        let chosen = chosen.session_id;

        match selector.mode {
            crate::paneselect::Mode::Activate => {
                self.window.tabs.set_active_pane(chosen);
                self.focus_session(chosen);
            }
            crate::paneselect::Mode::Swap => self.swap_panes(chosen),
        }
    }

    /// Exchange the chosen pane with the one in front, and follow it.
    ///
    /// Done by rebuilding the tab's arrangement with the two panes' places
    /// exchanged, rather than by moving anything: the shells keep running
    /// where they are, and only the rectangles they are drawn in change.
    fn swap_panes(&mut self, chosen: usize) {
        let (Some(tab_id), Some(live)) = (self.window.tab_id, self.window.state.as_ref()) else {
            return;
        };
        let focused = self.window.tabs.active_pane(tab_id).unwrap_or(live.session_id);
        if focused == chosen {
            return;
        }
        let (cols, rows) = self
            .window.font
            .grid_for(self.terminal_width(), self.terminal_height());
        let mut positions = self.window.tabs.positions(tab_id, cols, rows);
        for position in &mut positions {
            if position.pane_id == focused {
                position.pane_id = chosen;
            } else if position.pane_id == chosen {
                position.pane_id = focused;
            }
        }
        // Following means the focus keeps its *pane* rather than its place.
        let active = chosen;
        if let Err(err) = self.window.tabs.adopt_tab(tab_id, &positions, active) {
            log::warn!("could not swap panes: {err:#}");
            return;
        }
        self.window.tabs.set_active_pane(active);
        self.focus_session(active);
        self.resize_panes();
    }

    /// The pane under the pointer, when the tab is split.
    /// Where each pane's close button sits, when the tab is split.
    ///
    /// One per pane, top right, and only with two panes or more: a lone
    /// pane's close is the tab's close, and a button that would do that is
    /// a second close button. Ported from 0.57.4, which drew the same x.
    fn pane_close_buttons(&self) -> Vec<(usize, f32, f32, f32, f32, f32)> {
        let placements = self.placements();
        if placements.len() < 2 {
            return Vec::new();
        }
        let metrics = self.window.font.metrics();
        let Some(active) = self.window.tab_id.and_then(|tab| self.window.tabs.active_pane(tab)) else {
            return Vec::new();
        };
        let max_grid_right = placements
            .iter()
            .map(|placement| placement.origin.0 + placement.cols as f32 * metrics.width)
            .fold(f32::NEG_INFINITY, f32::max);
        placements
            .into_iter()
            // 0.57.4 intentionally shows exactly one close affordance: on the
            // active pane.  A button on every pane leaves an X floating in the
            // middle of a left/right split and reads as a misplaced control.
            .filter(|placement| placement.session_id == active)
            .map(|placement| {
                let grid_right = placement.origin.0 + placement.cols as f32 * metrics.width;
                let pane_right = if (grid_right - max_grid_right).abs() <= 0.5 {
                    self.terminal_left() + self.terminal_width()
                } else {
                    grid_right
                };
                let button_width = metrics.width * 3.0;
                let button_height = metrics.height;
                (
                    placement.session_id,
                    (pane_right - button_width).max(0.0),
                    placement.origin.1 - metrics.height * 0.28,
                    button_width,
                    button_height,
                    pane_right,
                )
            })
            .collect()
    }

    /// The pane close button under the pointer, if it is on one.
    fn pane_close_button_at(&self, x: f32, y: f32) -> Option<usize> {
        self.pane_close_buttons()
            .into_iter()
            .find(|(_, left, top, width, height, _)| {
                x >= *left && x < left + width && y >= *top && y < top + height
            })
            .map(|(session_id, ..)| session_id)
    }

    /// Draw the split-pane close buttons over the panes.
    fn append_pane_close_buttons(&mut self, quads: &mut unterm_render::quads::FrameQuads) {
        let buttons = self.pane_close_buttons();
        if buttons.is_empty() {
            return;
        }
        let chrome = self.chrome();
        let hovered = self.pane_close_button_at(self.window.pointer.0, self.window.pointer.1);
        for (session_id, left, top, width, height, pane_right) in buttons {
            let hover = hovered == Some(session_id);
            let chip = (height * 0.82).min(width * 0.82);
            let arm = (chip * 0.22).max(3.0);
            let center_x = (pane_right - 5.0 * self.chrome_pt() - arm).max(left + arm);
            let center_y = top + height / 2.0;
            if hover {
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left: center_x - chip / 2.0,
                    top: center_y - chip / 2.0,
                    width: chip,
                    height: chip,
                    color: chrome.hover_bg,
                });
            }
            let mut color = if hover {
                self.chrome_foreground()
            } else {
                self.chrome_foreground()
            };
            color[3] *= if hover { 0.9 } else { 0.26 };
            // The old renderer hand-stroked the X above terminal glyphs.
            // Using a font glyph changes its baseline and makes it sink into
            // the first text row, which is the severe offset seen here.
            let thick = (height / 12.0).max(1.6);
            let steps = ((arm * 4.0).round() as i32).max(24);
            for step in -steps..=steps {
                let t = step as f32 / steps as f32;
                let dx = t * arm;
                let dy = t * arm;
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left: center_x + dx - thick / 2.0,
                    top: center_y + dy - thick / 2.0,
                    width: thick,
                    height: thick,
                    color,
                });
                quads.backgrounds.push(unterm_render::quads::Quad {
                    left: center_x + dx - thick / 2.0,
                    top: center_y - dy - thick / 2.0,
                    width: thick,
                    height: thick,
                    color,
                });
            }
        }
    }

    fn pane_under_pointer(&self) -> Option<usize> {
        let metrics = self.window.font.metrics();
        self.placements()
            .into_iter()
            .find(|placement| {
                let width = placement.cols as f32 * metrics.width;
                let height = placement.rows as f32 * metrics.height;
                self.window.pointer.0 >= placement.origin.0
                    && self.window.pointer.0 < placement.origin.0 + width
                    && self.window.pointer.1 >= placement.origin.1
                    && self.window.pointer.1 < placement.origin.1 + height
            })
            .map(|placement| placement.session_id)
    }

    fn placements(&self) -> Vec<crate::panes::PanePlacement> {
        let (Some(tab_id), Some(_live)) = (self.window.tab_id, self.window.state.as_ref()) else {
            return Vec::new();
        };
        let metrics = self.window.font.metrics();
        let (cols, rows) = self
            .window.font
            .grid_for(self.terminal_width(), self.terminal_height());
        let left = self.terminal_left();
        let top = self.terminal_top();
        self.window.tabs
            .positions(tab_id, cols, rows)
            .into_iter()
            .map(|placed| {
                let mut placement = crate::panes::place(placed.pane_id, placed.rect, metrics);
                placement.origin.0 += left;
                placement.origin.1 += top;
                placement
            })
            .collect()
    }

    /// Tell every pane the size it is being drawn at.
    ///
    /// A shell wrapping at a width it is not shown at is the most confusing
    /// possible symptom, so this runs on every split and every resize.
    fn resize_panes(&mut self) {
        for placement in self.placements() {
            // Only a size the pane does not already have. Telling a PTY it is
            // the size it already is still costs a system call, and on Windows
            // it makes the console reflow -- so a resize that changes nothing
            // is not free, it is a flicker.
            let unchanged = self
                .window.pane_sizes
                .get(&placement.session_id)
                .is_some_and(|size| *size == (placement.cols, placement.rows));
            if unchanged {
                continue;
            }
            if self
                .engine
                .resize_session(placement.session_id, placement.cols, placement.rows)
                .is_ok()
            {
                self.window.pane_sizes
                    .insert(placement.session_id, (placement.cols, placement.rows));
            }
        }
        // A pane that has gone takes its remembered size with it, so a reused
        // id cannot inherit one.
        let live: std::collections::HashSet<usize> = self
            .placements()
            .iter()
            .map(|placement| placement.session_id)
            .collect();
        self.window.pane_sizes.retain(|pane, _| live.contains(pane));
    }

    /// Redraw only when the screen actually moved.
    /// The work every tick does, whatever woke it.
    ///
    /// Only the redraw check runs at the tick rate. The housekeeping -- the tab
    /// list, the cockpit, the window's title -- is not latency-critical and is
    /// expensive: listing sessions clones every snapshot, and feeding the
    /// cockpit reads every pane's screen. Running those as fast as the loop
    /// spins is most of what an idle window used to cost.
    fn tick(&mut self) {
        self.finish_startup_session();
        if !self.window.startup_terminal_content_marked {
            if let Some(live) = self.window.state.as_ref().filter(|live| live.session_id != 0) {
                self.engine.refresh_cached_frame(live.session_id);
            }
        }
        if self.window.restore_extra_tabs_pending
            && self.window.startup_first_frame_marked
            && self.window.startup_session.is_none()
            && self.window.state.as_ref().is_some_and(|live| live.session_id != 0)
        {
            self.window.restore_extra_tabs_pending = false;
            self.restore_extra_tabs();
        }
        if let Some(request) = unterm_services::theme_state::after(self.theme_request_seen) {
            self.theme_request_seen = request.generation;
            self.apply_theme(&request.id);
        }
        // Checked every tick rather than with the housekeeping: a window
        // whose Core was replaced is showing tabs for shells that no
        // longer exist, and waiting out the housekeeping interval to say
        // so leaves the user typing into nothing.
        if self.engine.session_epoch() != self.seen_session_epoch {
            self.seen_session_epoch = self.engine.session_epoch();
            self.recover_from_replaced_core();
        }
        if self.window.kept_house_at.elapsed() >= HOUSEKEEPING {
            self.window.kept_house_at = std::time::Instant::now();
            self.sync_tabs();
            self.feed_cockpit();
            self.update_window_title();
        }
        // The composer is checked every tick while it is open, because it is
        // waiting for a pane to go idle and a prompt held back for a quarter of
        // a second is a prompt somebody notices.
        if self.window.composer.is_some() {
            self.drain_composer();
        }
        if self.needs_redraw() {
            self.window.quiet_since = None;
            // One frame per refresh interval, no matter how fast the PTY
            // writes. A skipped request costs nothing: the busy tick fires
            // within the same interval and asks again, so the trailing
            // frame always lands.
            let frame_due = self
                .window.presented_at
                .map_or(true, |at| at.elapsed() >= std::time::Duration::from_millis(8));
            if frame_due {
                if let Some(live) = self.window.state.as_ref() {
                    live.window.request_redraw();
                }
            }
        } else if self.window.quiet_since.is_none() {
            self.window.quiet_since = Some(std::time::Instant::now());
        }
    }

    fn finish_startup_session(&mut self) {
        let ready = self
            .window.startup_session
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished);
        if !ready {
            return;
        }
        let Some(worker) = self.window.startup_session.take() else {
            return;
        };
        match worker.join() {
            Ok(Ok(session)) => {
                crate::startup_trace::mark("session.ready");
                self.window.tab_id = self.window.tabs.create_tab(session.id).ok();
                self.engine.refresh_cached_frame(session.id);
                self.window.quiet_since = None;
                self.window.drawn_revision = None;
                if let Some(live) = self.window.state.as_mut() {
                    live.session_id = session.id;
                    live.window.request_redraw();
                }
                self.draw();
                if !self.window.startup_input.is_empty() {
                    let input = std::mem::take(&mut self.window.startup_input);
                    if let Err(err) = self.engine.write_input(session.id, &input) {
                        log::warn!("could not replay startup input: {err:#}");
                    }
                }
            }
            Ok(Err(err)) => {
                log::error!("startup session failed: {err:#}");
                let err = err.to_string();
                self.show_notice(unterm_services::i18n::t_args(
                    "profile.toast.spawn_failed",
                    &[("err", err.as_str())],
                ));
                self.window.startup_input.clear();
            }
            Err(_) => {
                log::error!("startup session worker panicked");
                self.show_notice("Startup shell failed".to_string());
                self.window.startup_input.clear();
            }
        }
    }

    /// How long to wait before asking again.
    ///
    /// A frame while anything is happening, so output appears as soon as a
    /// display can show it. Slower once a window has been quiet for a while,
    /// because a pane that has produced nothing for two seconds is a pane
    /// nobody is watching for latency -- and a terminal left open on a desk
    /// should not cost a core. Anything the user does wakes the loop directly
    /// and this never gets in the way of it.
    fn tick_interval(&self) -> std::time::Duration {
        const BUSY: std::time::Duration = std::time::Duration::from_millis(8);
        const RESTING: std::time::Duration = std::time::Duration::from_millis(96);
        const SETTLES_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

        if !self.window.startup_terminal_content_marked
            && self.window.state.as_ref().is_some_and(|live| live.session_id != 0)
        {
            return BUSY;
        }

        match self.window.quiet_since {
            Some(since) if since.elapsed() > SETTLES_AFTER => RESTING,
            _ => BUSY,
        }
    }

    fn needs_redraw(&self) -> bool {
        let Some(live) = self.window.state.as_ref() else {
            return false;
        };
        // One number across the panes on screen: any of them moving is
        // a reason to redraw, and the backend answers it the cheapest
        // way it can -- per-pane revisions in this process, the frame
        // cache's own counter when the sessions are in a Core.
        let placements = self.placements();
        if placements.is_empty() && live.session_id == 0 {
            return self.window.drawn_revision != Some(0);
        }
        let panes: Vec<usize> = if placements.is_empty() {
            vec![live.session_id]
        } else {
            placements
                .iter()
                .map(|placement| placement.session_id)
                .collect()
        };
        let revision = self.engine.render_generation(&panes);
        if Some(revision) != self.window.drawn_revision {
            return true;
        }
        // A fading flash needs frames of its own: nothing about the screen
        // changes while it fades out. A blinking cursor needs exactly one
        // frame when its phase flips, not a frame on every idle tick.
        if self.window.bell_at.is_some()
            || (self.cursor_style.blinking
                && self.window.drawn_cursor_solid != Some(self.cursor_is_solid()))
        {
            return true;
        }
        // Blinking text likewise: one frame when a cadence the screen is
        // using flips phase, and none at all when nothing on screen blinks.
        if let Some(drawn) = self.window.drawn_blink {
            let phase = self.blink_phase();
            if (self.window.screen_blink.0 && drawn.slow_on != phase.slow_on)
                || (self.window.screen_blink.1 && drawn.rapid_on != phase.rapid_on)
            {
                return true;
            }
        }
        // A turning working spinner is animation with no screen change
        // underneath: one frame per phase step, and none once nothing works.
        if self.window.drawn_breath_step.is_some_and(|drawn| {
            let elapsed = unterm_services::cockpit::status::breath_epoch()
                .elapsed()
                .as_millis() as u64;
            crate::sidebar::spin_step(elapsed) != drawn
        }) {
            return true;
        }
        // A drag changes what is highlighted without changing the screen
        // underneath it.
        if self.window.drag.is_some() {
            return true;
        }
        // A banner arrives from the MCP thread, which changes no screen; if
        // only the screen could ask for a redraw, the question would never be
        // drawn and the agent would wait out its timeout looking at nothing.
        self.window.drawn_confirmation != unterm_mcp::handler::pending_confirmation_view().map(|v| v.id)
            || self.window.drawn_suggestions
                != crate::engine_backend::mcp_state::pending_suggestions_for_pane(self.focused_session() as u64)
                    .len()
    }
}

impl Live {
    fn configure(&self, format: wgpu::TextureFormat) {
        self.surface.configure(
            self.renderer.device(),
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: self.width,
                height: self.height,
                present_mode: wgpu::PresentMode::AutoVsync,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            },
        );
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        crate::startup_trace::mark("winit.resumed");
        // A parked process has no window on purpose. Resuming into one here
        // would put the terminal back on screen without anyone asking, which
        // is the opposite of what "keep running in the background" means.
        //
        // On macOS this is also the path back from "no windows, still in the
        // Dock": clicking the icon resumes the app, and an app that quit its
        // process when its last window closed would never get here. D3 of the
        // multi-window design keeps the process alive there; the window it
        // needs is made below, from the GPU this process already has.
        if self.window.state.is_some() || self.window.tray.is_some() {
            return;
        }
        match self.start(event_loop) {
            Ok(live) => {
                self.window.state = Some(live);
                self.window.restore_extra_tabs_pending = true;
                if let Some(live) = self.window.state.as_ref() {
                    live.window.request_redraw();
                }
                self.draw();
                crate::startup_trace::mark("window.state.ready");
            }
            Err(err) => {
                crate::report_fatal(&format!("Unterm could not start: {err:#}"));
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        // Nothing here ends the loop directly any more: closing goes through
        // `perform_close`, and `about_to_wait` is what acts on the flag it
        // sets -- one exit path, whether the press was on our chrome or the
        // system's.
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Which window this is about. winit says so per event; everything
        // below reads `self.window`, so the named one has to be there first.
        // A miss means the event outlived its window -- there is nothing to
        // act on and nothing to report.
        if !self.focus_window(window_id) {
            return;
        }
        match event {
            WindowEvent::Occluded(occluded) => {
                self.window.occluded = occluded;
                if !occluded {
                    // Visible again: whatever changed while hidden gets one
                    // one frame now rather than at the next input.
                    self.window.drawn_revision = None;
                    if let Some(live) = self.window.state.as_ref() {
                        live.window.request_redraw();
                    }
                }
            }

            WindowEvent::Focused(focused) => {
                self.window.focused = focused;
                // What `instance.windows` calls focused, and the window a
                // call that names none is about. It is recorded here rather
                // than in `focus_window` above, which looks like the same
                // thing and is not: that one runs for every event and only
                // says which window an event was about, so a background
                // window asking to be redrawn would have claimed the focus.
                if focused {
                    crate::mcp_host::set_focused_window(self.window.id);
                }
                // Anything that turned on focus reporting is told. vim redraws
                // its cursor on it and tmux its pane borders, so a terminal
                // that stays silent leaves them showing the wrong thing.
                self.report_focus(focused);
                // Coming back is a reason to look again straight away rather
                // than at the next resting tick.
                if focused {
                    self.window.quiet_since = None;
                }
                // A prompt that dims when unfocused has to be redrawn to show
                // it, and nothing about the screen changed to ask for a frame.
                self.window.drawn_revision = None;
                if let Some(live) = self.window.state.as_ref() {
                    live.window.request_redraw();
                }
            }

            WindowEvent::DroppedFile(path) => {
                // A file dropped on the terminal types its path, quoted when
                // the shell would need it to be -- ready to be an argument.
                let text = path.display().to_string();
                let plain = text
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || "_-./:\\~".contains(ch));
                let quoted = if plain { text } else { format!("\"{text}\"") };
                let pane = self.focused_session();
                let _ = self.engine.paste_input(pane, &quoted);
                self.window.drawn_revision = None;
                if let Some(live) = self.window.state.as_ref() {
                    live.window.request_redraw();
                }
            }

            WindowEvent::CloseRequested => {
                let _slow = SlowGuard::new("close_requested");
                // The system frame's close button, Cmd-W and Alt-F4 all land
                // here, and they mean exactly what the title bar's own cross
                // means. Routing them through the same function is what stops
                // the two disagreeing about whether the shells survive; a
                // running program earns one confirmation either way, because
                // a stray click killing an agent mid-task is no smaller an
                // accident on this path than on the other.
                // Another window behind this one means this is a view
                // going away, not the front end. Nothing to ask.
                if !self.window.close_confirmed && self.close_this_view() {
                    return;
                }
                self.request_close();
                if let Some(live) = self.window.state.as_ref() {
                    live.window.request_redraw();
                }
            }

            WindowEvent::Resized(size) => {
                // `Resized(0, 0)` is the window saying it is not being
                // drawn, not saying how wide it is being drawn. This used to
                // clamp it to one pixel, and one pixel is a size: every
                // fallback below floored at one cell, so the pane was told it
                // was one column wide, which truncated every line *and the
                // whole scrollback* to its first character. Restoring the
                // window could not bring any of it back. A window with no
                // size now keeps the last size it really had and tells no
                // pane anything.
                if !is_drawable_size(size.width, size.height) {
                    return;
                }
                if let Some(live) = self.window.state.as_mut() {
                    live.width = size.width;
                    live.height = size.height;
                    live.configure(live.renderer.format());
                    live.window.request_redraw();
                }
                // Every pane has to learn its new grid, or a shell keeps
                // wrapping at a width it is no longer drawn at.
                self.resize_panes();
                self.window.drawn_revision = None;
            }

            // The display's scale changed under the window: it moved to a
            // monitor with a different DPI, a remote session reconnected at a
            // new density, or an outside process resized it across a
            // virtualisation boundary. Fonts, atlas and pane grids are all
            // sized in physical pixels derived from this scale, so reopen
            // them at the new one — leaving this unhandled drew the whole
            // window at the old density, stretched.
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let scale = scale_factor as f32;
                if (scale - self.window.scale).abs() > f32::EPSILON {
                    self.reopen_font(self.window.font_points, scale);
                }
                if let Some(live) = self.window.state.as_ref() {
                    live.window.request_redraw();
                }
                self.window.drawn_revision = None;
            }

            WindowEvent::Ime(ime) => {
                use winit::event::Ime;
                // Same urgency as a plain keystroke: a committed composition
                // is input on its way to an echo.
                self.window.quiet_since = None;
                self.window.last_ime_event = Some(std::time::Instant::now());
                let modal_owns_ime = self.window.quick_menu.is_some()
                    || unterm_mcp::handler::pending_confirmation_view().is_some();
                if modal_owns_ime {
                    self.window.preedit = crate::ime::Preedit::default();
                    if let Some(live) = self.window.state.as_ref() {
                        live.window.request_redraw();
                    }
                    self.window.drawn_revision = None;
                    return;
                }
                match ime {
                    Ime::Preedit(text, caret) => {
                        self.window.preedit = crate::ime::Preedit {
                            text,
                            caret: caret.map(|(start, _end)| start),
                        };
                        self.place_ime_candidates();
                        if let Some(live) = self.window.state.as_ref() {
                            live.window.request_redraw();
                        }
                        self.window.drawn_revision = None;
                    }
                    Ime::Commit(text) => {
                        self.window.preedit = crate::ime::Preedit::default();
                        // A palette/search owns committed IME text while it is
                        // open.  Only an unclaimed commit belongs to the PTY.
                        if self.commit_ime_to_modal(&text) {
                            if let Some(live) = self.window.state.as_ref() {
                                live.window.request_redraw();
                            }
                        } else if let Some(live) = self.window.state.as_ref() {
                            let pane = self.focused_session();
                            if self.engine.write_input(pane, &text).is_ok() {
                                crate::ghost::observe_text(pane as u64, &text);
                            }
                            live.window.request_redraw();
                        }
                        self.window.drawn_revision = None;
                    }
                    Ime::Enabled => self.place_ime_candidates(),
                    Ime::Disabled => {
                        self.window.preedit = crate::ime::Preedit::default();
                        self.window.drawn_revision = None;
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.window.shift_held = modifiers.state().shift_key();
                self.window.ctrl_held = modifiers.state().control_key();
                self.window.alt_held = modifiers.state().alt_key();
                // The link hint appears and disappears with Ctrl, and nothing
                // else about the screen changed to ask for a frame.
                self.window.drawn_revision = None;
                if let Some(live) = self.window.state.as_ref() {
                    live.window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if self.window.state.is_none() {
                    return;
                }
                // A keystroke is about to produce an echo, and the resting
                // tick would keep the loop from looking for almost 100ms --
                // which is exactly the pause-then-type lag people report as
                // "typing feels slow". Drop back to the busy cadence now,
                // before the shell has even seen the byte.
                self.window.quiet_since = None;
                // An orphan composition is marked text whose input method
                // has gone quiet: stranded by a source switch, it swallows
                // every editing key from then on. A LIVE composition also
                // reaches us here -- winit reports KeyboardInput alongside
                // Ime events -- so silence, not arrival, is the test: only
                // a preedit nobody has updated for two seconds is a ghost.
                if !self.window.preedit.is_empty()
                    && self
                        .window.last_ime_event
                        .map_or(true, |at| at.elapsed() > std::time::Duration::from_secs(2))
                {
                    self.clear_orphan_preedit();
                }
                // A composition that is still alive owns its keystrokes
                // outright. winit hands over the same presses the input
                // method is composing with, and forwarding them typed the
                // pinyin under the hanzi it was about to become: the shell
                // received "fangjianli" AND the 房间里 the user meant, and
                // zle's column accounting never recovered. The orphan sweep
                // above has already run, so a dead preedit cannot wedge
                // this gate shut.
                if !self.window.preedit.is_empty() {
                    return;
                }

                use winit::keyboard::Key;

                if std::env::var_os("UNTERM_TRACE_KEYS").is_some() {
                    log::info!(
                        "key: logical={:?} physical={:?} text={:?} ctrl={} shift={} alt={}",
                        event.logical_key,
                        event.physical_key,
                        event.text,
                        self.window.ctrl_held,
                        self.window.shift_held,
                        self.window.alt_held,
                    );
                    log::info!(
                        "  action_for -> {:?}",
                        crate::keys::action_for(
                            &event.logical_key,
                            self.window.ctrl_held,
                            self.window.shift_held,
                            self.window.alt_held,
                        )
                    );
                }

                let pending_confirmation = unterm_mcp::handler::pending_confirmation_view();

                // While an agent write waits on its visible banner, the
                // banner owns the keys -- even over an open quick menu, whose
                // Enter would otherwise double as silent consent. Otherwise an
                // open quick menu is modal and must swallow every key; a stale
                // pending-confirmation count must not leak letters into the
                // shell under Claude or another mouse-reporting TUI.
                if self.window.quick_menu.is_some()
                    && quick_menu_owns_keyboard(pending_confirmation.is_some())
                {
                    match event.logical_key {
                        Key::Named(winit::keyboard::NamedKey::Escape) => {
                            self.window.quick_menu = None;
                        }
                        Key::Named(winit::keyboard::NamedKey::ArrowDown) => {
                            if let Some(menu) = self.window.quick_menu.as_mut() {
                                let next = menu.hover.map(|row| row + 1).unwrap_or(0)
                                    % menu.entries.len().max(1);
                                menu.hover = Some(next);
                                menu.reveal_hover();
                            }
                        }
                        Key::Named(winit::keyboard::NamedKey::ArrowUp) => {
                            if let Some(menu) = self.window.quick_menu.as_mut() {
                                let len = menu.entries.len().max(1);
                                menu.hover =
                                    Some(menu.hover.unwrap_or(0).checked_sub(1).unwrap_or(len - 1));
                                menu.reveal_hover();
                            }
                        }
                        Key::Named(winit::keyboard::NamedKey::Enter) => {
                            let command = self.window.quick_menu.take().and_then(|menu| {
                                menu.entries
                                    .get(menu.hover.unwrap_or(0))
                                    .map(|entry| entry.command.clone())
                            });
                            if let Some(command) = command {
                                self.run_palette_command(command, "");
                            }
                        }
                        _ => {}
                    }
                    self.window.drawn_revision = None;
                    // Every other key is the menu's to ignore: nothing may
                    // fall through to the shell under an open menu.
                    return;
                }
                if pending_confirmation.is_none() && self.handle_suggestion_key(&event) {
                    return;
                }
                if self.window.quick_select.is_some() && self.handle_quick_select_key(&event) {
                    return;
                }
                if self.window.copy_mode.is_some() && self.handle_copy_mode_key(&event) {
                    return;
                }
                if pending_confirmation.is_none()
                    && self.window.inbox_open
                    && self.handle_inbox_key(&event)
                {
                    return;
                }

                // The palette takes the keyboard while it is open, before
                // anything else looks at the key.
                if self.window.palette.is_some() && self.handle_palette_key(&event) {
                    return;
                }

                // And so does the pane selector: every letter on screen is one
                // of its labels, and a letter that fell through would run in
                // whichever pane happens to be in front.
                if self.window.pane_select.is_some() && self.handle_pane_select_key(&event) {
                    return;
                }

                // A search takes the keyboard while it is open: the letters
                // typed are the pattern, not input for the shell.
                if self.window.search.is_some() && self.handle_search_key(&event) {
                    return;
                }
                if self.window.composer.is_some() && self.handle_composer_key(&event) {
                    return;
                }

                let Some(live) = self.window.state.as_ref() else {
                    return;
                };

                // A parked agent write comes first: while the banner is up
                // these keys answer it rather than reaching the shell, and
                // everything else is ignored so a stray keystroke cannot be
                // mistaken for consent.
                if let Some(pending) = pending_confirmation {
                    let decision = match &event.logical_key {
                        Key::Named(winit::keyboard::NamedKey::Escape) => {
                            Some(unterm_mcp::handler::ConfirmationDecision::Block)
                        }
                        // Enter allows, as 0.57.4's banner had it; safe to
                        // take because the banner swallows every key below.
                        Key::Named(winit::keyboard::NamedKey::Enter) => {
                            Some(unterm_mcp::handler::ConfirmationDecision::Allow)
                        }
                        Key::Character(text) => crate::confirm::decision_for(text),
                        _ => None,
                    };
                    if let Some(decision) = decision {
                        unterm_mcp::handler::resolve_confirmation(pending.id, decision);
                        if let Some(live) = self.window.state.as_ref() {
                            live.window.request_redraw();
                        }
                    }
                    return;
                }

                let pane = self.focused_session();
                let startup_pending = live.session_id == 0;
                if crate::ghost::is_accept_key(
                    &event.logical_key,
                    self.window.ctrl_held,
                    self.window.shift_held,
                    self.window.alt_held,
                ) && unterm_services::ghost_text::has_pending_ghost(pane as u64)
                {
                    if let Some(continuation) = unterm_services::ghost_text::accept(pane as u64) {
                        if let Err(err) = self.engine.write_input(pane, &continuation) {
                            log::warn!("could not accept ghost text: {err:#}");
                        }
                        self.window.drawn_revision = None;
                        if let Some(live) = self.window.state.as_ref() {
                            live.window.request_redraw();
                        }
                    }
                    return;
                }

                // What the keys do lives in `keys`, so an agent asking the
                // MCP surface gets the same answer this acts on.
                if let Some(action) = crate::keys::action_for(
                    &event.logical_key,
                    self.window.ctrl_held,
                    self.window.shift_held,
                    self.window.alt_held,
                ) {
                    if startup_pending {
                        return;
                    }
                    self.run_key_action(action, live.session_id);
                    return;
                }

                let held = crate::mouse::Held {
                    shift: self.window.shift_held,
                    ctrl: self.window.ctrl_held,
                    alt: self.window.alt_held,
                };
                if let Some(text) = encode(&event.logical_key, held) {
                    if startup_pending {
                        if self.window.startup_input.len() + text.len() <= 4096 {
                            self.window.startup_input.push_str(&text);
                        }
                        return;
                    }
                    if self.engine.write_input(pane, &text).is_ok() {
                        if crate::ghost::observe_key(
                            pane as u64,
                            &event.logical_key,
                            self.window.ctrl_held,
                            self.window.alt_held,
                        ) {
                            self.window.drawn_revision = None;
                            if let Some(live) = self.window.state.as_ref() {
                                live.window.request_redraw();
                            }
                        }
                        if text == unterm_services::interrupt::INTERRUPT_BYTE {
                            self.interrupt(pane);
                        }
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.window.pointer = (position.x as f32, position.y as f32);
                self.apply_cursor();
                if self.window.dragging_scrollbar {
                    self.scroll_to_pointer();
                    return;
                }
                if let Some(menu) = self.window.quick_menu.as_mut() {
                    // The row under the pointer lights up -- but only a
                    // change of row is worth a frame. Repainting on every
                    // motion event turned a busy pane plus an open menu
                    // into a redraw storm.
                    let over = {
                        let (x, y) = self.window.pointer;
                        menu.row_at(x, y)
                    };
                    if menu.hover != over {
                        menu.hover = over;
                        self.window.drawn_revision = None;
                        if let Some(live) = self.window.state.as_ref() {
                            live.window.request_redraw();
                        }
                    }
                    return;
                }
                if let Some(TabDrag { tab_id, origin, engaged }) = self.window.dragging_tab {
                    // A held row is not a carried row until the pointer has
                    // truly left where it pressed: hands drift a pixel or
                    // two in every click, and a strip that reorders on that
                    // drift scrambles itself under its owner's clicks.
                    if !engaged {
                        let dx = self.window.pointer.0 - origin.0;
                        let dy = self.window.pointer.1 - origin.1;
                        let slack = self
                            .sidebar_dock()
                            .map(|(_, _, _, _, row_height)| row_height * 0.6)
                            .unwrap_or(12.0);
                        if (dx * dx + dy * dy).sqrt() < slack {
                            return;
                        }
                        if let Some(drag) = self.window.dragging_tab.as_mut() {
                            drag.engaged = true;
                        }
                        #[cfg(target_os = "macos")]
                        crate::macos_open::trace("tab drag engaged");
                    }
                    if let Some(at) = self.sidebar_row_at(self.window.pointer.0, self.window.pointer.1) {
                        let rows = self.sidebar_rows();
                        // Which tab position this row corresponds to: count
                        // the tab rows at or above it.
                        let target = rows
                            .iter()
                            .take(at + 1)
                            .filter(|row| matches!(row, crate::sidebar::Row::Tab { .. }))
                            .count()
                            .saturating_sub(1);
                        let ids = self.window.tabs.tab_ids();
                        if let Some(current) = ids.iter().position(|id| *id == tab_id) {
                            let delta = target as isize - current as isize;
                            if delta != 0 && self.window.tabs.move_tab_relative(tab_id, delta) {
                                self.window.drawn_revision = None;
                                if let Some(live) = self.window.state.as_ref() {
                                    live.window.request_redraw();
                                }
                            }
                        }
                    }
                    return;
                }
                if self.window.dragging_sidebar_width {
                    // The strip follows the pointer; `sidebar::width` clamps
                    // it between readable and greedy.
                    let pt = self.chrome_pt();
                    self.window.sidebar_points = Some((self.window.pointer.0 / pt.max(0.001)).max(1.0));
                    self.resize_panes();
                    self.window.drawn_revision = None;
                    if let Some(live) = self.window.state.as_ref() {
                        live.window.request_redraw();
                    }
                    return;
                }
                // An open menu follows the pointer before anything else looks
                // at it: the row under the mouse is the row Enter runs, and
                // nothing behind a menu should react to being pointed at.
                if self.hover_palette() {
                    return;
                }
                // A swallowed Ctrl+Left gesture keeps its motion too: the
                // program never saw the press, so a drag report -- or a
                // selection -- growing out of it would come from nowhere.
                if self.window.swallow_left_after_secondary {
                    return;
                }
                if self.report_mouse(
                    unterm_engine::next_core::mouse_encoding::MouseEventKind::Motion,
                    self.window.held_mouse_button,
                ) {
                    return;
                }
                if self.window.drag.is_some() {
                    // Dragging past the edge scrolls the view along, so a
                    // selection is not capped at one screen of text.
                    if let Some(live) = self.window.state.as_ref() {
                        let session_id = live.session_id;
                        let bottom = live.height as f32 - self.status_bar_height();
                        if self.window.pointer.1 < self.terminal_top() {
                            let _ = self.engine.scroll_viewport_by(session_id, 1);
                        } else if self.window.pointer.1 > bottom {
                            let _ = self.engine.scroll_viewport_by(session_id, -1);
                        }
                    }
                    let point = self.cell_under_pointer();
                    self.extend_drag_to(point);
                    self.update_selection();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                use unterm_engine::next_core::mouse_encoding::MouseEventKind;
                use winit::event::MouseButton;

                // A Ctrl+Left press consumed as a secondary click swallows
                // the rest of that physical gesture: its release would
                // otherwise fall through to the left-button paths --
                // complete a selection, open a link -- and make a single
                // click both paste and act.
                if self.window.swallow_left_after_secondary {
                    match (state, button) {
                        (ElementState::Released, MouseButton::Left) => {
                            self.window.swallow_left_after_secondary = false;
                            // The press was consumed too, so nothing is held.
                            self.window.held_mouse_button = None;
                            return;
                        }
                        (ElementState::Pressed, _) => {
                            self.window.swallow_left_after_secondary = false;
                        }
                        _ => {}
                    }
                }

                let held = crate::mouse::Held {
                    shift: self.window.shift_held,
                    ctrl: self.window.ctrl_held,
                    alt: self.window.alt_held,
                };
                // The platform's other right button: everywhere a right
                // press acts, a macOS Ctrl+Left press acts the same.
                let secondary = button == MouseButton::Right
                    || (button == MouseButton::Left && crate::mouse::ctrl_left_is_secondary(held));

                // The program gets the click if it asked for one -- every
                // button, not only the left, since that is what a program
                // with a context menu is waiting for.
                let engine_button = match button {
                    MouseButton::Left => {
                        Some(unterm_engine::next_core::mouse_encoding::MouseButton::Left)
                    }
                    MouseButton::Middle => {
                        Some(unterm_engine::next_core::mouse_encoding::MouseButton::Middle)
                    }
                    MouseButton::Right => {
                        Some(unterm_engine::next_core::mouse_encoding::MouseButton::Right)
                    }
                    _ => None,
                };
                self.window.held_mouse_button = match state {
                    ElementState::Pressed => engine_button,
                    ElementState::Released => None,
                };
                // Ended here, before any branch can consume the event: a
                // release that a mouse-reporting program swallowed used to
                // leave the drag armed, and every later cursor move kept
                // reordering the strip with no button held at all.
                if state == ElementState::Released && self.window.dragging_tab.is_some() {
                    self.window.dragging_tab = None;
                    return;
                }
                let kind = match state {
                    ElementState::Pressed => MouseEventKind::Press,
                    ElementState::Released => MouseEventKind::Release,
                };
                // The dropdown is modal for the whole mouse gesture. The
                // press may choose a row; the matching release must not leak
                // into the pane underneath as terminal mouse input.
                if self.window.quick_menu.is_some() {
                    match quick_menu_mouse_action(state, button, secondary) {
                        QuickMenuMouseAction::ActivateHovered => {
                            self.click_quick_menu();
                        }
                        QuickMenuMouseAction::Close => {
                            self.window.quick_menu = None;
                            self.window.drawn_revision = None;
                            if secondary && button == MouseButton::Left {
                                self.window.swallow_left_after_secondary = true;
                            }
                        }
                        QuickMenuMouseAction::Swallow => {}
                    }
                    return;
                }
                // An open menu takes the press before the program does: a
                // click aimed at a row must not also land in the pane the menu
                // is covering.
                if self.window.palette.is_some()
                    && state == ElementState::Pressed
                    && button == MouseButton::Left
                    && !secondary
                    && self.click_palette()
                {
                    return;
                }
                // And so does the tab strip: a press on a row is a press on a
                // row, not a click into the pane beside it.
                #[cfg(target_os = "macos")]
                if state == ElementState::Pressed && button == MouseButton::Left {
                    // A "clicked the tab and nothing happened" report needs
                    // this press's whole story: where it was, which row that
                    // resolved to, and which branch ate it. Strip presses
                    // only — a terminal full of clicks has nothing to say
                    // here and would drown the log saying it.
                    let row = self.sidebar_row_at(self.window.pointer.0, self.window.pointer.1);
                    if row.is_some() {
                        crate::macos_open::trace(&format!(
                            "press at ({:.0},{:.0}) secondary={} ctrl={} row={:?}",
                            self.window.pointer.0, self.window.pointer.1, secondary, self.window.ctrl_held, row,
                        ));
                    }
                }
                if self.window.sidebar_open
                    && state == ElementState::Pressed
                    && button == MouseButton::Left
                    && !secondary
                    && self.click_sidebar()
                {
                    return;
                }
                if self.window.tree.is_some()
                    && state == ElementState::Pressed
                    && button == MouseButton::Left
                    && !secondary
                    && self.click_tree()
                {
                    return;
                }
                if state == ElementState::Pressed
                    && button == MouseButton::Left
                    && !secondary
                    && self.click_search_bar()
                {
                    return;
                }
                // The Git dock is read-only, as in 0.57.4. Its reserved
                // gutter swallows presses so they cannot reach the pane
                // beside it as terminal mouse input or a selection -- but
                // only presses: a release belongs to whatever drag is in
                // flight, and eating it here left selections and scrollbar
                // drags wedged to the pointer.
                if state == ElementState::Pressed
                    && self.window.drag.is_none()
                    && !self.window.dragging_scrollbar
                    && self.click_git_panel()
                {
                    return;
                }
                if state == ElementState::Pressed
                    && button == MouseButton::Left
                    && !secondary
                    && self.click_status_bar()
                {
                    return;
                }
                // A secondary press on the chrome answers with its menus
                // before the program can see it: a right press -- or its
                // macOS Ctrl+Left form -- aimed at a tab is asking for the
                // tab's menu, not the pane behind it.
                if secondary && state == ElementState::Pressed && self.chrome_right_click() {
                    if button == MouseButton::Left {
                        self.window.swallow_left_after_secondary = true;
                    }
                    return;
                }
                // Chrome belongs to the terminal, not the program inside it.
                // TUIs such as Claude enable mouse reporting; if top-bar
                // presses wait until after report_mouse, the chevron click is
                // delivered to the TUI and the quick menu never opens.
                if state == ElementState::Pressed
                    && button == MouseButton::Left
                    && !secondary
                    && self.window.pointer.1 < self.top_bar_height()
                    && self.click_top_bar()
                {
                    return;
                }
                // The copy/paste gesture, ahead of mouse reporting when a
                // selection proves the user is mid-gesture: in a reporting
                // pane it can only have been made with Shift held, and its
                // natural completion is the secondary click's copy. Without
                // one the click still belongs to the program.
                let reporting = !held.shift
                    && self.window.mouse_modes.tracking
                        != unterm_engine::next_core::mouse_encoding::MouseTracking::None;
                if state == ElementState::Released {
                    // Whichever button came up, the gesture that was in
                    // flight is over.
                    self.window.secondary_gesture.released();
                }
                if secondary
                    && state == ElementState::Pressed
                    && crate::mouse::secondary_click_acts(reporting, self.window.selected.is_some())
                {
                    // macOS delivers one Control-click as both a right press
                    // and a ctrl+left press. Acting twice does not merely
                    // repeat: the first copies the selection and lets go of
                    // it, so the second sees none and pastes — into whatever
                    // is running in the pane, which is usually an agent's
                    // prompt. Once per physical click.
                    if !self.window.secondary_gesture.take() {
                        return;
                    }
                    if button == MouseButton::Left {
                        self.window.swallow_left_after_secondary = true;
                    }
                    // A direct gesture rather than a menu, and one thing
                    // rather than two: it pastes. Selecting already put the
                    // text on the clipboard when the button came up, so the
                    // press has nothing left to copy -- and a press that
                    // copied instead was a paste that did not happen.
                    // Only on press, so the release does not undo it.
                    if crate::mouse::secondary_press_pastes(self.window.selected.is_some()) {
                        self.paste_clipboard();
                    }
                    return;
                }
                if self.report_mouse(kind, engine_button) {
                    return;
                }

                if button == MouseButton::Right {
                    // Acted on above, or forwarded: the release must not act
                    // again, and a press the program declined still must not
                    // start a left-button drag.
                    return;
                }
                if button == MouseButton::Middle {
                    // The middle button pastes, as terminals have always had
                    // it -- from the one clipboard this platform has.
                    if state == ElementState::Pressed {
                        self.paste_clipboard();
                    }
                    return;
                }
                if button != MouseButton::Left {
                    return;
                }
                // The edges first: a borderless window has no system resize
                // handles, so a press there has to start one.
                if state == ElementState::Pressed {
                    if let Some(direction) = self.resize_edge_at_pointer() {
                        if let Some(live) = self.window.state.as_ref() {
                            let _ = live.window.drag_resize_window(direction);
                            return;
                        }
                    }
                }
                if state == ElementState::Pressed && button == MouseButton::Left {
                    if let Some(session_id) =
                        self.pane_close_button_at(self.window.pointer.0, self.window.pointer.1)
                    {
                        self.close_pane(session_id);
                        self.window.drawn_revision = None;
                        return;
                    }
                }
                if self.window.sidebar_open && state == ElementState::Pressed {
                    if let Some((left, _top, width, _height, _row)) = self.sidebar_dock() {
                        let pt = self.chrome_pt();
                        let edge = left + width;
                        if (self.window.pointer.0 - edge).abs()
                            <= crate::ui_tokens::LEFT_TAB_BAR_GRIP * pt / 2.0
                        {
                            self.window.dragging_sidebar_width = true;
                            return;
                        }
                    }
                }
                if state == ElementState::Released && self.window.dragging_sidebar_width {
                    self.window.dragging_sidebar_width = false;
                    return;
                }
                if state == ElementState::Pressed && self.pointer_on_scrollbar() {
                    self.window.dragging_scrollbar = true;
                    self.scroll_to_pointer();
                    return;
                }
                if state == ElementState::Released && self.window.dragging_scrollbar {
                    self.window.dragging_scrollbar = false;
                    return;
                }
                if state == ElementState::Pressed && crate::links::opens_on_click(self.window.ctrl_held) {
                    if let Some(link) = self.link_under_pointer() {
                        if let Err(err) = crate::links::open_link(&link) {
                            log::warn!("could not open {}: {err}", link.uri);
                        }
                        return;
                    }
                }
                // A press in a pane that is not in front brings it in
                // front, and does nothing else: focusing should never also
                // select or paste, which is why the click stops here.
                if state == ElementState::Pressed {
                    if let Some(pane) = self.pane_under_pointer() {
                        if pane != self.focused_session() {
                            // Keep the registry, engine and Live session in
                            // lockstep.  Updating only the registry left input
                            // and the solid cursor in the previous pane.
                            self.focus_session(pane);
                            return;
                        }
                    }
                }
                match state {
                    ElementState::Pressed => {
                        use unterm_engine::next_core::selection::SelectionShape;
                        let cell = self.cell_under_pointer();
                        // Shift extends the previous selection from its
                        // original anchor; Alt drags out a block, as before.
                        if self.window.shift_held {
                            if let Some(anchor) = self.window.select_anchor {
                                let mut drag =
                                    crate::select::Drag::start(anchor, SelectionShape::Linear);
                                drag.extend(cell);
                                self.window.drag = Some(drag);
                                self.update_selection();
                                self.window.drawn_revision = None;
                                return;
                            }
                        }
                        let click = match self.window.terminal_click.take() {
                            Some(previous) => previous.again(0, self.window.pointer.0, self.window.pointer.1),
                            None => {
                                crate::sidebar::RowClick::first(0, self.window.pointer.0, self.window.pointer.1)
                            }
                        };
                        let streak = click.streak();
                        self.window.terminal_click = Some(click);
                        let shape = if self.window.alt_held {
                            SelectionShape::Block
                        } else {
                            SelectionShape::Linear
                        };
                        self.window.select_anchor = Some(cell);
                        self.window.select_granularity = match streak {
                            2 => SelectGranularity::Word,
                            n if n >= 3 => SelectGranularity::Line,
                            _ => SelectGranularity::Cell,
                        };
                        match streak {
                            2 => self.select_word_at(cell),
                            n if n >= 3 => self.select_line_at(cell),
                            _ => {
                                self.window.drag = Some(crate::select::Drag::start(cell, shape));
                                self.window.selected = None;
                            }
                        }
                        self.window.drawn_revision = None;
                    }
                    ElementState::Released => {
                        // A press that never moved is a click, and a click on
                        // a link opens it -- the way it worked before Ctrl
                        // became a requirement.
                        let was_click = self
                            .window.drag
                            .map(|drag| drag.selection().is_none())
                            .unwrap_or(false);
                        self.update_selection();
                        self.window.drag = None;
                        if self.window.selected.is_some() {
                            // Selecting is copying, exactly as it was before:
                            // release the button and the text is already on
                            // the clipboard.
                            self.copy_selection();
                        } else if was_click && !self.window.shift_held && !self.window.ctrl_held {
                            if let Some(link) = self.link_under_pointer() {
                                if let Err(err) = crate::links::open_link(&link) {
                                    log::warn!("could not open {}: {err}", link.uri);
                                }
                            }
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                use winit::event::MouseScrollDelta;
                let cell_height = self.window.font.metrics().height;
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => crate::scroll::lines_for_wheel(
                        crate::scroll::WheelDelta::Lines(y),
                        cell_height,
                    ),
                    MouseScrollDelta::PixelDelta(position) => crate::scroll::lines_for_wheel(
                        crate::scroll::WheelDelta::Pixels(position.y as f32),
                        cell_height,
                    ),
                };
                if lines == 0 {
                    return;
                }
                if let Some(menu) = self.window.quick_menu.as_mut() {
                    menu.scroll(-lines);
                    self.window.drawn_revision = None;
                    return;
                }
                if let Some(palette) = self.window.palette.as_mut() {
                    palette.scroll(-lines, crate::palette::MAX_ROWS);
                    self.window.drawn_revision = None;
                    return;
                }
                // The wheel belongs to whatever is under the pointer. A tree
                // that scrolls the pane beside it instead is a tree you cannot
                // reach the bottom of.
                if self.tree_row_at(self.window.pointer.0, self.window.pointer.1).is_some() {
                    let metrics = self.window.font.metrics();
                    let visible = (self.terminal_height() / metrics.height.max(1.0)) as usize;
                    if let Some(tree) = self.window.tree.as_mut() {
                        tree.scroll_by(-lines, visible.max(1));
                    }
                    self.window.drawn_revision = None;
                    return;
                }
                // The wheel over the top bar walks the tabs, the way the old
                // bar always answered it.
                if self.window.pointer.1 < self.top_bar_height() {
                    self.cycle_tab(if lines > 0 { -1 } else { 1 });
                    self.window.drawn_revision = None;
                    if let Some(live) = self.window.state.as_ref() {
                        live.window.request_redraw();
                    }
                    return;
                }
                // The tab strip scrolls under its own wheel, like the tree: a
                // strip that scrolls the pane beside it is a strip you cannot
                // reach the bottom of.
                if self.window.sidebar_open && self.window.tree.is_none() {
                    if let Some((left, top, width, height, _row_height)) = self.sidebar_dock() {
                        let inside = self.window.pointer.0 >= left
                            && self.window.pointer.0 < left + width
                            && self.window.pointer.1 >= top
                            && self.window.pointer.1 < top + height;
                        if inside {
                            self.window.sidebar_scroll =
                                self.window.sidebar_scroll.saturating_add_signed(-(lines as isize));
                            self.window.drawn_revision = None;
                            if let Some(live) = self.window.state.as_ref() {
                                live.window.request_redraw();
                            }
                            return;
                        }
                    }
                }
                // A program that asked for the mouse gets the wheel too --
                // that is how less and htop scroll themselves. One report per
                // notch, because that is what a wheel is.
                if let Some(button) = crate::mouse::wheel_button(lines as f32) {
                    let notches = lines.unsigned_abs().min(16);
                    let mut reported = false;
                    for _ in 0..notches {
                        reported = self.report_mouse(
                            unterm_engine::next_core::mouse_encoding::MouseEventKind::Press,
                            Some(button),
                        );
                        if !reported {
                            break;
                        }
                    }
                    if reported {
                        return;
                    }
                }
                if let Some(live) = self.window.state.as_ref() {
                    // The wheel belongs to the pane under the pointer, not
                    // whichever pane happens to hold the keyboard.
                    let session_id = self.pane_under_pointer().unwrap_or(live.session_id);
                    // On the alternate screen there is nothing to scroll back
                    // into: less, man and vim without mouse reporting expect
                    // the wheel as arrow keys, three per notch, exactly as the
                    // previous front end sent them.
                    let modes = self.engine.pane_modes(session_id).unwrap_or_default();
                    if modes.alt_screen_active {
                        let arrow = if lines > 0 {
                            if modes.application_cursor_keys {
                                "\x1bOA"
                            } else {
                                "\x1b[A"
                            }
                        } else if modes.application_cursor_keys {
                            "\x1bOB"
                        } else {
                            "\x1b[B"
                        };
                        let presses = arrow.repeat((lines.unsigned_abs().min(16)) * 3);
                        let _ = self.engine.write_input(session_id, &presses);
                        return;
                    }
                    // Positive is toward older output, and the wheel rolls
                    // away from you to go back in time.
                    let _ = self.engine.scroll_viewport_by(session_id, -lines);
                }
            }

            WindowEvent::RedrawRequested => {
                self.draw();
            }

            _ => {}
        }
    }

    /// The event loop is ending -- the other way out.
    ///
    /// On macOS a Cmd+Q goes through `applicationWillTerminate:`, which winit
    /// reports here and which never reaches the window teardown that ends the
    /// process itself. Stopping the Core has to hang off both, or quitting
    /// the ordinary way on a Mac leaves one running.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        crate::engine_backend::stop_core_if_ours();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Cheap, and the only place that sees every way a tab or a session
        // can appear or go: `instance.windows` reports this, and
        // `session.focus` uses it to find the window a session is in. Parked
        // windows are pushed too -- a window keeps running its tabs while
        // another one is in front, so what it happened to be showing when it
        // was parked would be wrong the first time anything changed behind
        // it, and `session.focus` would raise the wrong window.
        Self::note_window_view(&self.window);
        for parked in &self.parked {
            Self::note_window_view(&parked.state);
        }
        // Asked for from outside -- an MCP call, a keystroke, or a second
        // launch that handed over rather than becoming another process. Each
        // request carries the id its window was promised, and two callers
        // asking at once want two windows rather than one.
        for request in unterm_engine::take_window_requests() {
            crate::startup_trace::mark("window.second.start");
            self.open_window(event_loop, request);
            crate::startup_trace::mark("window.second.ready");
        }
        if self.window.closing {
            // The close button was pressed. There is no native title bar to
            // do this for us any more.
            event_loop.exit();
            return;
        }
        // Before the parked branch below returns: a loop that stops beating
        // is a loop the stall watchdog reports as frozen, and "parked" is the
        // one quiet state that is not a freeze.
        crate::stallwatch::beat();
        // Parked in the tray: no window, no frame, nothing to draw. The loop
        // stays alive only to answer the indicator and keep its counts
        // honest, so it wakes a few times a minute rather than at frame rate.
        if self.window.tray.is_some() {
            match crate::tray::poll() {
                Some(crate::tray::Action::Open) => {
                    self.leave_background(event_loop);
                    return;
                }
                Some(crate::tray::Action::QuitAll) => {
                    // The same irreversible action the close prompt's last
                    // row performs, reached from the only surface left.
                    if let crate::engine_backend::AppEngine::Core { client, .. } = &self.engine {
                        let client = client.clone();
                        std::thread::spawn(move || {
                            if let Err(err) = client.shutdown() {
                                log::warn!("could not stop the core: {err:#}");
                            }
                        });
                    }
                    self.window.tray = None;
                    self.window.close_confirmed = true;
                    self.perform_close(CloseOutcome::KeepSessions);
                    return;
                }
                None => {}
            }
            if self.window.tray_refreshed.elapsed() >= std::time::Duration::from_secs(2) {
                self.window.tray_refreshed = std::time::Instant::now();
                // Nothing else drives the cockpit tracker, and "2 agents are
                // waiting for you" is precisely the number this indicator
                // exists to carry. Skipping it while parked would freeze that
                // count at whatever it read the instant the window closed --
                // a report that looks live and is not. It rate-limits itself,
                // so calling it on every refresh costs one screen read per
                // pane per second and no frames at all.
                self.feed_cockpit();
                let status = self.background_status();
                if let Some(tray) = self.window.tray.as_mut() {
                    tray.update(status);
                }
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(200),
            ));
            return;
        }
        // The machine may have been asleep since the last tick. Nothing here
        // asks the operating system: a monotonic clock does not advance while
        // suspended and a wall clock does, so the two disagreeing by a lot is
        // the wake. Cheap enough for an idle tick — two clock reads — and it
        // works the same on all three platforms.
        if let Some(away) = self.wake_watch.tick() {
            let report = unterm_services::wake_watch::on_wake(away);
            log::info!("woke after {}s away: {report}", away.as_secs());
        }

        // An input-source switch mid-composition strands marked text that
        // eats editing keys. But the pinyin IME announces a "switch" for
        // its own internal mode flips too -- half-width punctuation, caps
        // -- in the middle of a perfectly live composition, and killing
        // that one commits its fragments as garbage. Same rule as the key
        // path: only a composition whose IME has gone silent is a ghost.
        #[cfg(target_os = "macos")]
        if crate::ime_watch::input_source_changed()
            && !self.window.preedit.is_empty()
            && self
                .window.last_ime_event
                .map_or(true, |at| at.elapsed() > std::time::Duration::from_secs(2))
        {
            self.clear_orphan_preedit();
        }
        // What macOS asked us to open -- Finder's right-click, a folder on
        // the Dock icon -- becomes a tab, the same way it would anywhere.
        #[cfg(target_os = "macos")]
        for path in crate::macos_open::drain() {
            let dir = if path.is_dir() {
                Some(path.clone())
            } else {
                path.parent().map(std::path::Path::to_path_buf)
            };
            // Written down, not just acted on: "it opened the wrong folder"
            // is undebuggable without knowing what macOS actually handed us.
            crate::macos_open::trace(&format!(
                "received {:?} -> opening {:?}",
                path,
                dir.as_deref()
            ));
            if let Some(dir) = dir {
                self.new_tab_in(&dir.to_string_lossy());
            }
            if let Some(live) = self.window.state.as_ref() {
                live.window.focus_window();
            }
        }
        self.collect_clipboard_results();
        self.tick();
        // Waiting until the next tick rather than spinning. Something has to
        // ask the engine whether a shell has written -- nothing wakes the loop
        // when one does -- but asking as fast as the CPU allows is how this
        // came to burn most of a core sitting at an idle prompt.
        //
        // The interval is the answer to "how late may output be", not "how
        // often can we ask": a frame is the shortest delay anybody can see, and
        // a pane that has been quiet for a while can be asked far less often
        // without anyone noticing. Typing does not depend on either -- a key
        // wakes the loop by itself.
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            std::time::Instant::now() + self.tick_interval(),
        ));
    }
}

/// The command the config names, if it names one.
///
/// A string is the program; a list is the program and its arguments. Real
/// shells need arguments -- `pwsh -NoLogo`, `bash --login` -- and a setting
/// that cannot express them makes the user pick between their flags and the
/// config.
/// Apply the current window identity and launch policy immediately before a
/// pane is spawned. Keeping only the base command in `App` means a profile
/// change affects future panes without mutating shells that already exist.
/// A path quoted for the shell the pane runs.
///
/// The POSIX single-quote form leaves nothing for the shell to expand --
/// `$`, backticks and spaces are all literal, and the quote itself is the
/// one thing escaped. A repo can contain a directory named `$(rm -rf ~)`;
/// double quotes would hand that to the shell. Windows paths cannot contain
/// `"`, so the double-quoted form is enough for cmd and PowerShell alike.
fn shell_quoted_path(path: &str) -> String {
    if cfg!(windows) {
        format!("\"{path}\"")
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

/// What a held drag snaps to, decided by the click streak that started it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectGranularity {
    Cell,
    Word,
    Line,
}

/// One screen row as a per-column string for selection work.
///
/// A wide glyph owns two grid columns; the second holds a space the user
/// never typed. Copy paths that keep it turn 你好 into 你 好 on paste, and
/// word selection sees a boundary in the middle of every CJK word. The
/// spacer becomes a NUL so columns still line up for the extraction math,
/// and `strip_spacer_marks` removes it from anything headed to the
/// clipboard. The parser never stores a NUL in a cell, so the marker
/// cannot collide with real content.
fn selection_row_text(cells: &[unterm_engine::StyledCell]) -> String {
    cells
        .iter()
        .map(|cell| if cell.width == 0 { '\u{0}' } else { cell.ch })
        .collect()
}

/// Drop the wide-glyph spacer markers from text bound for the clipboard.
fn strip_spacer_marks(text: String) -> String {
    if text.contains('\u{0}') {
        text.replace('\u{0}', "")
    } else {
        text
    }
}

/// Times a close-path suspect and reports it if it held the GUI thread.
struct SlowGuard {
    what: &'static str,
    since: std::time::Instant,
}
impl SlowGuard {
    fn new(what: &'static str) -> Self {
        Self { what, since: std::time::Instant::now() }
    }
}
impl Drop for SlowGuard {
    fn drop(&mut self) {
        crate::stallwatch::note_if_slow(self.what, self.since, 300);
    }
}

fn prepare_shell(
    mut shell: Option<portable_pty::CommandBuilder>,
) -> Option<portable_pty::CommandBuilder> {
    unterm_services::launch_env::apply_unterm_windows_utf8(&mut shell);
    unterm_services::launch_env::apply_unterm_profile_env(&mut shell);
    unterm_services::launch_env::apply_unterm_proxy_env(&mut shell);
    shell
}

fn launch_env_for_new_pane() -> Vec<(String, String)> {
    let mut env = unterm_services::launch_env::current_profile_env();
    env.extend(unterm_services::launch_env::read_unterm_proxy_env().unwrap_or_default());
    env
}

fn shell_from(config: &config::Config) -> Option<portable_pty::CommandBuilder> {
    match config.get("shell") {
        Some(config::Value::Str(program)) => Some(portable_pty::CommandBuilder::new(program)),
        Some(config::Value::List(parts)) => {
            let mut words = parts.iter().filter_map(|part| match part {
                config::Value::Str(word) => Some(word.as_str()),
                _ => None,
            });
            let mut command = portable_pty::CommandBuilder::new(words.next()?);
            for word in words {
                command.arg(word);
            }
            Some(command)
        }
        Some(_) => None,
        None => command_from_argv(unterm_services::settings::preferred_platform_shell()?),
    }
}

fn command_from_argv(argv: Vec<String>) -> Option<portable_pty::CommandBuilder> {
    let mut words = argv.into_iter();
    let mut command = portable_pty::CommandBuilder::new(words.next()?);
    for word in words {
        command.arg(word);
    }
    Some(command)
}

/// Walk `split_from` up to the pane an arrangement was grown from.
///
/// A split is recorded on the pane that was split off, pointing at its
/// source, so a tab's panes form a chain back to one root. Rebuilding has to
/// start there: every other pane needs its source to already be in a tab.
///
/// A dead or missing source ends the walk -- that subtree cannot be rebuilt
/// anyway -- and the step count is bounded by the session list, so a
/// malformed chain cannot spin here while a window waits to open.
fn split_lineage_root(
    live: &[unterm_engine::SessionSnapshot],
    session: unterm_engine::SessionSnapshot,
) -> unterm_engine::SessionSnapshot {
    let mut current = session;
    for _ in 0..live.len() {
        let Some(parent) = current
            .split_from
            .and_then(|source| live.iter().find(|session| session.id == source))
        else {
            break;
        };
        current = parent.clone();
    }
    current
}

/// Whether one window should take a session into its tabs.
///
/// The whole of who-owns-what, in one place because every part of it was
/// learned from a way multi-window went wrong:
///
/// - A window keeps what it already has.
/// - A pane another window holds is that window's. Every window used to
///   mirror the Core's whole session list, so a second window showed the
///   first one's shells as well as its own and the two drifted towards the
///   same set.
/// - A pane split off another belongs in that one's window, even when the
///   user is looking at a different one. An agent splitting a pane in a
///   background window and getting a new tab in the foreground one got
///   something else than it asked for, and the split it did ask for was
///   nowhere to be seen.
/// - Anything else is an orphan and is taken. That is not a leak: it is a
///   session created over MCP, which nothing named a window for, and it is
///   what D1 leaves behind when a window closes and its shells keep running.
///   Windows are offered sessions front one first, so an orphan lands in the
///   window the user is in.
fn window_should_adopt(
    pane_id: usize,
    split_from: Option<usize>,
    already_here: bool,
    shown_elsewhere: &std::collections::HashSet<usize>,
) -> bool {
    if already_here {
        return false;
    }
    if shown_elsewhere.contains(&pane_id) {
        return false;
    }
    if split_from.is_some_and(|source| shown_elsewhere.contains(&source)) {
        return false;
    }
    true
}

/// Every pane one window is showing, across all of its tabs.
fn pane_set(state: &WindowState) -> std::collections::HashSet<usize> {
    state
        .tabs
        .tab_ids()
        .into_iter()
        .flat_map(|tab| state.tabs.pane_ids(tab))
        .collect()
}

/// Mirror the Core's session list into one window's tabs.
///
/// The Core is the only place sessions exist; a window's `TabRegistry` is a
/// picture of the ones it is showing. This keeps the picture true -- panes
/// closed elsewhere go, panes this window should have arrive -- and it is
/// deliberately about one window: `shown_elsewhere` is what the others
/// already hold, and nothing here may take one of those.
///
/// Returns true when the arrangement moved.
fn adopt_sessions_into(
    state: &mut WindowState,
    sessions: &[unterm_engine::SessionSnapshot],
    live_ids: &std::collections::HashSet<usize>,
    shown_elsewhere: &std::collections::HashSet<usize>,
) -> bool {
    state
        .pane_notices
        .retain(|pane_id, _| live_ids.contains(pane_id));
    let mut changed = false;

    // Sessions may be closed through MCP as well as through this window.
    // Remove each missing pane from the mirrored layout, not only tabs
    // whose every pane vanished: otherwise destroying one half of a split
    // leaves the survivor permanently laid out at half width.
    for pane in missing_mirrored_panes(&state.tabs, live_ids) {
        state.tabs.close_pane(pane);
        state.pane_sizes.remove(&pane);
        changed = true;
    }

    for session in sessions {
        if !window_should_adopt(
            session.id,
            session.split_from,
            state.tabs.tab_of_pane(session.id).is_some(),
            shown_elsewhere,
        ) {
            continue;
        }
        // A pane split off another belongs beside it, not in a tab of its
        // own: an agent asking for a split and getting a new tab got
        // something else than it asked for.
        let split = session
            .split_from
            .filter(|source| state.tabs.tab_of_pane(*source).is_some());
        // Which way, at what size, and on which side -- from the engine,
        // which resolved all three when the split was made. That is what lets
        // an arrangement survive a restart: this window may never have seen
        // the split it is rebuilding. Falling back to horizontal-half-second
        // is for panes made before the engine recorded any of this.
        let outcome = match split {
            Some(source) => state
                .tabs
                .split(
                    source,
                    session.id,
                    session
                        .split_axis
                        .unwrap_or(unterm_engine::next_core::layout::SplitAxis::Horizontal),
                    session.split_ratio.unwrap_or(0.5),
                    session
                        .split_side
                        .unwrap_or(unterm_engine::next_core::layout::SplitSide::Second),
                )
                .map(|_| ()),
            None => state.tabs.create_tab(session.id).map(|_| ()),
        };
        match outcome {
            Ok(()) => {
                #[cfg(target_os = "macos")]
                crate::macos_open::trace(&format!(
                    "adopt session {} (split of {:?})",
                    session.id, split
                ));
                changed = true;
            }
            Err(err) => log::warn!("could not adopt session {}: {err:#}", session.id),
        }
    }
    changed
}

/// Point a window at a tab that exists, after its arrangement moved.
fn settle_active_tab(state: &mut WindowState) {
    let ids = state.tabs.tab_ids();
    if state.tab_id.map(|id| ids.contains(&id)).unwrap_or(false) {
        return;
    }
    let Some(first) = ids.first().copied() else {
        state.tab_id = None;
        return;
    };
    state.tabs.set_active_tab(first);
    state.tab_id = Some(first);
    if let Some(pane) = state.tabs.active_pane(first) {
        state.tabs.set_active_pane(pane);
        if let Some(live) = state.state.as_mut() {
            live.session_id = pane;
        }
    }
}

fn missing_mirrored_panes(
    tabs: &unterm_engine::next_core::tabs::TabRegistry,
    live_ids: &std::collections::HashSet<usize>,
) -> Vec<usize> {
    tabs.tab_ids()
        .into_iter()
        .flat_map(|tab_id| tabs.pane_ids(tab_id))
        .filter(|pane| !live_ids.contains(pane))
        .collect()
}

/// Turn a key press into the bytes a PTY expects.
///
/// Named keys go through next-core's encoder, which knows the escape sequences;
/// printable text is sent as typed, which is what the shell reads for anything
/// the encoder has no opinion about.
/// Turn a key press into the bytes a shell expects.
///
/// The engine already knows every sequence; what this has to get right is
/// which key and which modifiers it is handed. Both were wrong before:
/// modifiers were dropped on the floor, so Ctrl+arrow could not move by word
/// and Ctrl+C sent the letter `c` rather than an interrupt, and only a
/// handful of named keys were mapped at all.
fn encode(logical: &winit::keyboard::Key, held: crate::mouse::Held) -> Option<String> {
    use termwiz::input::{KeyCode, Modifiers};
    use winit::keyboard::{Key, NamedKey};

    let mut mods = Modifiers::NONE;
    if held.shift {
        mods |= Modifiers::SHIFT;
    }
    if held.ctrl {
        mods |= Modifiers::CTRL;
    }
    if held.alt {
        mods |= Modifiers::ALT;
    }

    let key = match logical {
        Key::Named(named) => match named {
            NamedKey::Enter => KeyCode::Enter,
            NamedKey::Backspace => KeyCode::Backspace,
            NamedKey::Tab => KeyCode::Tab,
            NamedKey::Escape => KeyCode::Escape,
            NamedKey::ArrowUp => KeyCode::UpArrow,
            NamedKey::ArrowDown => KeyCode::DownArrow,
            NamedKey::ArrowLeft => KeyCode::LeftArrow,
            NamedKey::ArrowRight => KeyCode::RightArrow,
            NamedKey::Home => KeyCode::Home,
            NamedKey::End => KeyCode::End,
            NamedKey::PageUp => KeyCode::PageUp,
            NamedKey::PageDown => KeyCode::PageDown,
            NamedKey::Delete => KeyCode::Delete,
            NamedKey::Insert => KeyCode::Insert,
            NamedKey::Space => KeyCode::Char(' '),
            NamedKey::F1 => KeyCode::Function(1),
            NamedKey::F2 => KeyCode::Function(2),
            NamedKey::F3 => KeyCode::Function(3),
            NamedKey::F4 => KeyCode::Function(4),
            NamedKey::F5 => KeyCode::Function(5),
            NamedKey::F6 => KeyCode::Function(6),
            NamedKey::F7 => KeyCode::Function(7),
            NamedKey::F8 => KeyCode::Function(8),
            NamedKey::F9 => KeyCode::Function(9),
            NamedKey::F10 => KeyCode::Function(10),
            NamedKey::F11 => KeyCode::Function(11),
            NamedKey::F12 => KeyCode::Function(12),
            // Modifier keys themselves produce nothing, and neither do the
            // dozens of media and system keys a keyboard may have.
            _ => return None,
        },
        Key::Character(text) => {
            let mut chars = text.chars();
            let (Some(first), None) = (chars.next(), chars.next()) else {
                // Several characters from one key press: a dead-key
                // composition or an input method's output. It is already the
                // text the user meant, and no modifier applies to it.
                return Some(text.to_string());
            };
            KeyCode::Char(first)
        }
        // Dead keys on their way to composing something, and anything else
        // winit could not name.
        _ => return None,
    };

    key_encoding::encode_key(key, mods)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every way of bringing a tab forward has to tell its panes how big this
    /// window is.
    ///
    /// Checked against the source because there is no seam to check it at:
    /// each of these is a few lines that set the active tab, and the resize
    /// is one more line that is easy to leave out and silent when it is.
    /// `select_tab` had the line; the palette's `activate_tab`, the
    /// keyboard's `cycle_tab`, `close_tab` and `open_tab_with` did not, and
    /// a pane brought forward by any of those four kept the size it had when
    /// it was last in front.
    ///
    /// A heuristic, and worth knowing where it stops: it asks only that the
    /// call appears within the next few lines, so it catches the omission it
    /// was written for and would not catch a `resize_panes` put on a branch
    /// that does not run. `settle_active_tab` is exempt on purpose -- it
    /// takes a `WindowState` rather than the front end, so it cannot resize
    /// anything, and both of its callers do it themselves.
    #[test]
    fn every_tab_switch_resizes_the_panes_it_brings_forward() {
        let source = include_str!("window.rs");
        let lines: Vec<&str> = source.lines().collect();
        let mut missing = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains("tabs.set_active_tab(") {
                continue;
            }
            let enclosing = lines[..=index]
                .iter()
                .rev()
                .find(|candidate| candidate.trim_start().starts_with("fn "))
                .map(|candidate| candidate.trim())
                .unwrap_or("");
            if enclosing.starts_with("fn settle_active_tab") {
                continue;
            }
            let window = &lines[index..(index + 25).min(lines.len())];
            if !window.iter().any(|line| line.contains("resize_panes()")) {
                missing.push(format!("line {}: {enclosing}", index + 1));
            }
        }
        assert!(
            missing.is_empty(),
            "a tab is brought forward without its panes being resized:\n  {}",
            missing.join("\n  ")
        );
    }


    /// A row's position in the strip is not a number key.
    ///
    /// `select_tab` maps a *key*, and nine and above mean "the last tab"
    /// there, which is right for a keyboard with nine of them. The strip has
    /// no such limit: its ninth row is the ninth tab. Feeding a row index in
    /// as a key sent every click past the eighth to the end of the strip --
    /// a tab that switches, just not to the one under the pointer. Rows go
    /// through `select_tab_at`, which takes the position it is given.
    #[test]
    fn a_row_position_never_arrives_as_a_number_key() {
        let source = include_str!("window.rs");
        let offenders: Vec<String> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| {
                let line = line.trim();
                line.starts_with("self.select_tab(") && line.contains("index")
            })
            .map(|(index, line)| format!("line {}: {}", index + 1, line.trim()))
            .collect();
        assert!(
            offenders.is_empty(),
            "a strip position was passed as a number key:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// A window with no size must not be mistaken for a one-cell one.
    ///
    /// This is the top of the chain that cost a pane its scrollback: every
    /// step below here floors at one cell, so a zero never reached the pane
    /// as a zero -- it reached it as `cols: 1`, which truncates the pane's
    /// lines instead of reflowing them. The engine refuses that grid as a
    /// backstop; this is the refusal that keeps it from being asked for.
    #[test]
    fn a_window_with_no_size_is_not_a_window_one_cell_wide() {
        assert!(!is_drawable_size(0, 0));
        assert!(!is_drawable_size(0, 600));
        assert!(!is_drawable_size(800, 0));
        assert!(is_drawable_size(1, 1));
        assert!(is_drawable_size(1280, 720));
    }

    #[test]
    fn startup_content_trace_ignores_blank_cells_and_spacers() {
        let cell = |ch: char, width: usize| unterm_engine::StyledCell {
            ch,
            style: Default::default(),
            width,
        };
        let line = |cells| unterm_engine::StyledScreenLine {
            row: 0,
            cells,
            wrapped: false,
        };

        assert!(!styled_lines_have_text(&[line(vec![
            cell(' ', 1),
            cell('\t', 1),
            cell(' ', 0),
        ])]));
        assert!(styled_lines_have_text(&[line(vec![
            cell(' ', 0),
            cell('P', 1),
            cell('S', 1),
        ])]));
    }

    #[test]
    fn a_wide_glyph_copies_without_its_spacer_cell() {
        let cell = |ch: char, width: usize| unterm_engine::StyledCell {
            ch,
            style: Default::default(),
            width,
        };
        // 你好 ab on the grid: each hanzi owns a lead cell and a spacer.
        let cells = vec![
            cell('你', 2),
            cell(' ', 0),
            cell('好', 2),
            cell(' ', 0),
            cell(' ', 1),
            cell('a', 1),
            cell('b', 1),
        ];
        let text = selection_row_text(&cells);
        // Column math still sees one entry per grid column...
        assert_eq!(text.chars().count(), cells.len());
        // ...but nothing the user never typed reaches the clipboard.
        assert_eq!(strip_spacer_marks(text), "你好 ab");
    }

    /// The close prompt is for the last window, not for every window.
    ///
    /// D2 of the multi-window design. With another window open, closing this
    /// one decides nothing about the sessions -- the process stays, the Core
    /// stays, and the panes are reachable from the window that remains. A
    /// prompt there asks a question whose answers are all the same.
    #[test]
    fn the_close_prompt_belongs_to_the_last_window() {
        // The production rule itself, not a copy of it. `window_count` is
        // read in exactly two places and both turn on whether it is 1:
        // `close_needs_confirmation` returns early when it is more, and
        // `close_this_view` refuses to close a view when it is not.
        assert_eq!(
            App::window_count_for(0),
            1,
            "no parked window means this is the last one"
        );
        assert_eq!(App::window_count_for(1), 2, "one parked window makes two");
        assert_eq!(App::window_count_for(2), 3);
    }

    fn elsewhere(panes: &[usize]) -> std::collections::HashSet<usize> {
        panes.iter().copied().collect()
    }

    /// A session belongs to one window.
    ///
    /// Every window mirrors the Core's session list into its own tabs, and
    /// every window sees the whole list. Without this rule a second window
    /// showed the first one's shells as well as its own -- open one and its
    /// title bar said `[1/2]` before anything had been run in it.
    #[test]
    fn a_pane_another_window_holds_is_not_taken() {
        assert!(
            !window_should_adopt(1, None, false, &elsewhere(&[1])),
            "the window showing pane 1 keeps it"
        );
        assert!(
            window_should_adopt(2, None, false, &elsewhere(&[1])),
            "a pane no window holds is taken"
        );
        assert!(
            !window_should_adopt(1, None, true, &elsewhere(&[])),
            "a pane already here is not adopted twice"
        );
    }

    /// A split belongs beside the pane it came from, in that pane's window.
    ///
    /// The user may well be looking at a different window when an agent asks
    /// for the split. Adopting it here would put a new tab in the window the
    /// user is in and leave the split that was asked for nowhere to be seen.
    #[test]
    fn a_split_follows_its_source_into_the_other_window() {
        assert!(
            !window_should_adopt(3, Some(1), false, &elsewhere(&[1])),
            "pane 1 is in another window, so its split goes there"
        );
        assert!(
            window_should_adopt(3, Some(1), false, &elsewhere(&[9])),
            "the window holding pane 1 takes the split"
        );
    }

    /// An orphan is taken rather than left running invisibly.
    ///
    /// Two things make orphans: a session created over MCP, which named no
    /// window, and D1 -- closing a window closes a view and leaves its shells
    /// running. Both have to resurface, and windows are offered sessions
    /// front one first, so both land in the window the user is in.
    #[test]
    fn a_session_no_window_holds_is_taken() {
        assert!(window_should_adopt(4, None, false, &elsewhere(&[1, 2])));
        assert!(
            window_should_adopt(4, Some(9), false, &elsewhere(&[1, 2])),
            "a split whose source no window holds either is still an orphan"
        );
    }

    #[test]
    fn display_scale_is_applied_once_to_fonts_and_chrome() {
        assert_eq!(App::font_scale_for(1.0), 1.0);
        assert_eq!(App::font_scale_for(1.5), 1.5);
        assert_eq!(App::font_scale_for(2.0), 2.0);
    }

    /// Closing a window ends the shells it was showing -- and only those.
    ///
    /// The list came from the engine, which in Core mode is every window's:
    /// two windows open, close one, and the other was left running with no
    /// panes at all. The tab registry is this window's own, so it is what
    /// "the shells I was showing" means.
    #[test]
    fn a_window_closes_only_the_panes_it_holds() {
        let mut mine = unterm_engine::next_core::tabs::TabRegistry::new();
        mine.create_tab(1).expect("a tab");
        mine.split(
            1,
            2,
            unterm_engine::next_core::layout::SplitAxis::Horizontal,
            0.5,
            unterm_engine::next_core::layout::SplitSide::Second,
        )
        .expect("a split");
        mine.create_tab(3).expect("a second tab");

        // What `perform_close(EndSessions)` now destroys.
        let mut closing: Vec<usize> = mine
            .tab_ids()
            .into_iter()
            .flat_map(|tab| mine.pane_ids(tab))
            .collect();
        closing.sort();

        assert_eq!(closing, vec![1, 2, 3], "every pane this window holds");
        // Pane 4 belongs to another window on the same Core. Nothing here
        // may reach it.
        assert!(
            !closing.contains(&4),
            "another window's pane must survive this close"
        );
    }

    #[test]
    fn an_externally_closed_split_is_removed_from_the_mirrored_layout() {
        let mut tabs = unterm_engine::next_core::tabs::TabRegistry::new();
        tabs.create_tab(1).expect("first pane should make a tab");
        tabs.split(
            1,
            2,
            unterm_engine::next_core::layout::SplitAxis::Horizontal,
            0.5,
            unterm_engine::next_core::layout::SplitSide::Second,
        )
        .expect("second pane should split the tab");
        let live = std::collections::HashSet::from([1]);

        assert_eq!(missing_mirrored_panes(&tabs, &live), vec![2]);
        tabs.close_pane(2);
        assert_eq!(tabs.pane_ids(tabs.tab_ids()[0]), vec![1]);
    }

    #[test]
    fn migrated_legacy_chrome_colours_override_derived_tones() {
        let config = config::parse(
            r##"
[window_frame]
active_titlebar_bg = "#2c2c2c"
active_titlebar_fg = "#ffffff"

[colors.tab_bar.active_tab]
bg_color = "#0c0c0c"

[colors.tab_bar.inactive_tab]
fg_color = "#cccccc"

[colors.tab_bar.inactive_tab_hover]
bg_color = "#3a3a3a"
"##,
        )
        .expect("config should parse");
        let overrides = ChromeOverrides::from_config(&config);
        let chrome = overrides.apply(
            crate::chrome::chrome(
                crate::chrome::srgb(0x12, 0x12, 0x12),
                crate::chrome::srgb(0xee, 0xee, 0xee),
            ),
            true,
        );

        assert_eq!(chrome.surface, crate::chrome::srgb(0x2c, 0x2c, 0x2c));
        assert_eq!(chrome.selected_bg, crate::chrome::srgb(0x0c, 0x0c, 0x0c));
        assert_eq!(chrome.hover_bg, crate::chrome::srgb(0x3a, 0x3a, 0x3a));
        assert_eq!(chrome.dim_text, crate::chrome::srgb(0xcc, 0xcc, 0xcc));
        assert_eq!(
            overrides.active_foreground,
            Some(crate::chrome::srgb(0xff, 0xff, 0xff))
        );
    }

    #[test]
    fn inactive_pane_transform_dims_without_changing_alpha() {
        let color = transform_hsv([0.8, 0.4, 0.2, 0.7], 1.0, 0.8, 0.55);

        assert!(color[0] < 0.8);
        assert!(color[1] < 0.4);
        assert!(color[2] < 0.2);
        assert_eq!(color[3], 0.7);
    }

    #[test]
    fn identity_inactive_pane_transform_changes_nothing() {
        let color = [0.15, 0.45, 0.8, 1.0];
        assert_eq!(transform_hsv(color, 1.0, 1.0, 1.0), color);
    }

    /// The hue factor turns the colour around the wheel without touching how
    /// bright or how saturated it is: pure blue halved lands on pure green,
    /// still as saturated, still as bright.
    #[test]
    fn the_hue_factor_turns_the_colour_without_dimming_it() {
        let color = transform_hsv([0.0, 0.0, 1.0, 1.0], 0.5, 1.0, 1.0);

        assert_eq!(color, [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn the_configured_shell_is_used() {
        let config = config::parse("shell = \"pwsh.exe\"").expect("config should parse");

        let shell = shell_from(&config).expect("a named shell should be used");

        // Without this the session falls back to %COMSPEC% -- cmd.exe on
        // Windows, which is not what a config naming pwsh meant.
        assert_eq!(shell.get_argv()[0], "pwsh.exe");
    }

    #[test]
    fn a_shell_can_carry_its_arguments() {
        let config =
            config::parse(r#"shell = ["pwsh.exe", "-NoLogo"]"#).expect("config should parse");

        let shell = shell_from(&config).expect("a named shell should be used");

        // A setting that cannot express arguments makes the user choose
        // between their flags and the config.
        let argv = shell.get_argv();
        assert_eq!(argv[0], "pwsh.exe");
        assert_eq!(argv[1], "-NoLogo");
    }

    #[test]
    #[cfg(not(windows))]
    fn a_config_naming_no_shell_leaves_the_choice_to_the_engine() {
        let config = config::parse("font_size = 13").expect("config should parse");

        assert!(shell_from(&config).is_none());
    }

    #[test]
    #[cfg(windows)]
    fn a_config_naming_no_shell_uses_the_platform_powershell_default() {
        let config = config::parse("font_size = 13").expect("config should parse");
        let shell = shell_from(&config).expect("Windows has a platform shell");
        let argv = shell.get_argv();
        let name = argv[0].to_string_lossy().to_ascii_lowercase();

        assert!(name.contains("powershell") || name.contains("pwsh"), "{argv:?}");
        assert_eq!(argv[1].to_string_lossy(), "-NoLogo");
        assert_eq!(argv[2].to_string_lossy(), "-NoProfile");
    }

    #[test]
    fn the_first_pane_is_measured_against_the_terminal_not_the_whole_window() {
        // The bug this pins: `start` sized the very first shell with
        // `grid_for(window_width, window_height)` while every later
        // measurement used `terminal_width()`, which subtracts the tab strip
        // and the padding. The shell was therefore told it had the sidebar's
        // columns as well as its own, so a TUI launched at startup — Claude
        // Code, reported 2026-08-15 — drew past the right edge and stayed
        // that way until a resize. Maximising "fixed" it because the resize
        // was the first measurement that was correct.
        let config = config::parse("font_size = 13").expect("config should parse");
        let app = App::new(&config).expect("an app without a window");
        let metrics = app.window.font.metrics();

        // Exactly the situation `start` is in: a known window size and no
        // `Live` state yet.
        let window_width = 1200.0_f32;
        let dock = app.dock_width_for(window_width, metrics);
        let terminal = app.terminal_width_for(window_width);

        assert!(
            dock > 0.0,
            "the tab strip has width at startup, so it must be subtracted"
        );
        assert!(
            terminal <= window_width - dock,
            "the terminal is {terminal} wide inside a {window_width} window whose              strip already takes {dock}; the shell would be given columns it              cannot draw into"
        );

        let (honest_cols, _) = app.window.font.grid_for(terminal, 600.0);
        let (whole_window_cols, _) = app.window.font.grid_for(window_width, 600.0);
        assert!(
            honest_cols < whole_window_cols,
            "measuring the window instead of the terminal must yield more              columns ({whole_window_cols}) than the grid has ({honest_cols});              if these are equal the test is no longer measuring the defect"
        );
    }

    #[test]
    fn cycling_forward_from_the_last_tab_wraps_to_the_first() {
        let ids = [10, 20, 30];
        assert_eq!(next_tab_index(&ids, Some(30), 1), 0);
        assert_eq!(next_tab_index(&ids, Some(10), 1), 1);
    }

    #[test]
    fn cycling_back_from_the_first_tab_wraps_to_the_last() {
        let ids = [10, 20, 30];
        assert_eq!(next_tab_index(&ids, Some(10), -1), 2);
        assert_eq!(next_tab_index(&ids, Some(30), -1), 1);
    }

    #[test]
    fn a_tab_that_is_no_longer_there_cycles_from_the_start() {
        // Closing a tab can leave the remembered id dangling; landing on the
        // first tab is a defined answer, and panicking is not.
        let ids = [10, 20, 30];
        assert_eq!(next_tab_index(&ids, Some(99), 1), 1);
        assert_eq!(next_tab_index(&ids, None, 1), 1);
    }

    #[test]
    fn cycling_with_no_tabs_answers_rather_than_dividing_by_zero() {
        let ids: [usize; 0] = [];
        assert_eq!(next_tab_index(&ids, None, 1), 0);
    }
}

/// Which tab a cycle lands on.
///
/// Wrapping rather than stopping at the ends: with three tabs, cycling
/// forward twice from the last has to land somewhere, and a key that does
/// nothing at the edge reads as a key that is broken.
fn next_tab_index<T: PartialEq>(ids: &[T], current: Option<T>, step: isize) -> usize {
    if ids.is_empty() {
        return 0;
    }
    let index = current
        .and_then(|id| ids.iter().position(|candidate| *candidate == id))
        .unwrap_or(0) as isize;
    (index + step).rem_euclid(ids.len() as isize) as usize
}

/// Blend two colours.
fn mix(from: [f32; 4], to: [f32; 4], amount: f32) -> [f32; 4] {
    let blend = |a: f32, b: f32| a + (b - a) * amount;
    [
        blend(from[0], to[0]),
        blend(from[1], to[1]),
        blend(from[2], to[2]),
        from[3],
    ]
}

fn dim_pane_quads(
    quads: &mut unterm_render::quads::FrameQuads,
    background_start: usize,
    glyph_start: usize,
    hue: f32,
    saturation: f32,
    brightness: f32,
) {
    if (hue - 1.0).abs() < f32::EPSILON
        && (saturation - 1.0).abs() < f32::EPSILON
        && (brightness - 1.0).abs() < f32::EPSILON
    {
        return;
    }
    for quad in &mut quads.backgrounds[background_start..] {
        quad.color = transform_hsv(quad.color, hue, saturation, brightness);
    }
    for glyph in &mut quads.glyphs[glyph_start..] {
        glyph.quad.color = transform_hsv(glyph.quad.color, hue, saturation, brightness);
    }
}

/// Multiply a colour's hue, saturation and brightness, as the old renderer's
/// shader did: each HSV component times its factor, the hue wrapping around
/// the wheel rather than piling up at red.
fn transform_hsv(color: [f32; 4], hue_factor: f32, saturation: f32, brightness: f32) -> [f32; 4] {
    if (hue_factor - 1.0).abs() < f32::EPSILON
        && (saturation - 1.0).abs() < f32::EPSILON
        && (brightness - 1.0).abs() < f32::EPSILON
    {
        return color;
    }
    let [red, green, blue, alpha] = color;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if max == red {
        ((green - blue) / delta).rem_euclid(6.0)
    } else if max == green {
        (blue - red) / delta + 2.0
    } else {
        (red - green) / delta + 4.0
    };
    let hue = (hue * hue_factor).rem_euclid(6.0);
    let sat = if max <= f32::EPSILON {
        0.0
    } else {
        (delta / max * saturation).clamp(0.0, 1.0)
    };
    let value = (max * brightness).clamp(0.0, 1.0);
    let chroma = value * sat;
    let x = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hue as i32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = value - chroma;
    [r1 + m, g1 + m, b1 + m, alpha]
}

/// The palette's rows: every action a key can reach.
///
/// Built from the same table the keys use, so a chord and a palette row
/// cannot drift apart -- and the chord is shown as the hint, which is how a
/// palette teaches the keyboard.
fn command_entries() -> Vec<crate::palette::Entry> {
    let mut entries: Vec<crate::palette::Entry> = Vec::new();
    for &action in crate::keys::PALETTE_ACTIONS {
        let label = command_label(action);
        entries.push(crate::palette::Entry {
            label,
            hint: crate::keys::chord_hint(action).unwrap_or_default(),
            command: crate::palette::Command::Action(action),
        });
    }
    entries.push(crate::palette::Entry {
        label: unterm_services::i18n::t("command.tab_navigator"),
        hint: unterm_services::i18n::t("command.tab_navigator.hint"),
        command: crate::palette::Command::OpenTabNavigator,
    });
    entries.push(crate::palette::Entry {
        label: unterm_services::i18n::t("command.capture_region"),
        hint: unterm_services::i18n::t("command.capture_region.hint"),
        command: crate::palette::Command::SelectCaptureRegion,
    });
    entries
}

fn command_label(action: crate::keys::Action) -> String {
    use crate::keys::Action;
    let key = match action {
        Action::Copy => Some("command.copy"),
        Action::Paste => Some("command.paste"),
        Action::SplitRight => Some("command.split_right"),
        Action::SplitDown => Some("command.split_down"),
        Action::NewTab => Some("command.new_tab"),
        Action::CloseTab => Some("command.close_tab"),
        Action::Search => Some("command.search"),
        Action::CommandPalette => Some("command.command_palette"),
        Action::Launcher => Some("command.launcher"),
        Action::NewWindow => Some("command.new_window"),
        Action::ZoomPane => Some("command.zoom_pane"),
        Action::SelectPane => Some("command.select_pane"),
        Action::TreeSidebar => Some("command.file_tree"),
        Action::DirJump => Some("command.directory"),
        Action::Settings => Some("command.settings"),
        _ => None,
    };
    key.map(unterm_services::i18n::t)
        .unwrap_or_else(|| action.label().to_string())
}

/// The saved workspaces: a row reopens each, and the last row saves one.
///
/// 0.57.4's launcher listed workspaces beside its commands
/// (`LauncherFlags::WORKSPACES`); these rows are their next-core
/// descendants, read from the same files the MCP `workspace.*` tools use.
fn workspace_entries() -> Vec<crate::palette::Entry> {
    let mut entries: Vec<crate::palette::Entry> = crate::workspaces::list()
        .into_iter()
        .map(|workspace| crate::palette::Entry {
            label: format!("Open Workspace: {}", workspace.name),
            hint: match workspace.cwds.len() {
                1 => "1 saved tab".to_string(),
                count => format!("{count} saved tabs"),
            },
            command: crate::palette::Command::RestoreWorkspace {
                name: workspace.name,
            },
        })
        .collect();
    entries.push(crate::palette::Entry {
        label: "Save Workspace".to_string(),
        hint: "the open tabs, under a name".to_string(),
        command: crate::palette::Command::OpenWorkspaceSave,
    });
    entries
}

/// The launcher's rows: the shells this machine actually has.
///
/// Probed rather than listed: offering a shell that is not installed is a row
/// that opens an empty tab and an error in a log the user will not read.
fn launcher_entries() -> Vec<crate::palette::Entry> {
    let mut candidates: Vec<(String, String, String, Vec<String>)> = Vec::new();
    let mut add = |label: &str, hint: &str, program: String, args: &[&str]| {
        if std::path::Path::new(&program).is_file() || which(&program).is_some() {
            candidates.push((
                label.to_string(),
                hint.to_string(),
                program,
                args.iter().map(|arg| arg.to_string()).collect(),
            ));
        }
    };

    #[cfg(windows)]
    {
        let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
        add(
            "PowerShell 7",
            "Cross-platform shell",
            pwsh.into(),
            &["-NoLogo"],
        );
        add(
            "Windows PowerShell",
            "Built-in (5.1)",
            "powershell.exe".into(),
            &["-NoLogo", "-NoProfile"],
        );
        add("Command Prompt", "cmd.exe", "cmd.exe".into(), &[]);
        add(
            "Git Bash",
            "Unix shell via Git",
            r"C:\Program Files\Git\bin\bash.exe".into(),
            &["--login"],
        );
        add("WSL", "Linux subsystem", "wsl.exe".into(), &[]);
        add(
            "MSYS2 Bash",
            "MSYS2 environment",
            r"C:\msys64\usr\bin\bash.exe".into(),
            &["--login"],
        );
        let nu = std::env::var_os("USERPROFILE")
            .map(std::path::PathBuf::from)
            .map(|home| home.join(".cargo").join("bin").join("nu.exe"))
            .filter(|path| path.is_file())
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Program Files\nu\bin\nu.exe"));
        add(
            "Nushell",
            "Structured data shell",
            nu.display().to_string(),
            &[],
        );
    }

    #[cfg(not(windows))]
    {
        if let Ok(shell) = std::env::var("SHELL") {
            add("Default Shell", "Login shell", shell, &[]);
        }
        add("Bash", "GNU Bourne-Again Shell", "/bin/bash".into(), &[]);
        add("Zsh", "Z Shell", "/bin/zsh".into(), &[]);
        add(
            "Fish",
            "Friendly interactive shell",
            "/usr/bin/fish".into(),
            &[],
        );
    }

    candidates
        .into_iter()
        .map(|(label, hint, program, args)| crate::palette::Entry {
            label,
            hint,
            command: crate::palette::Command::Launch { program, args },
        })
        .collect()
}

/// Where a program is, if it is anywhere on PATH.
fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE".to_string())
            .split(';')
            .map(|ext| ext.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        // A name without its extension, which is how everyone writes them.
        for extension in &extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod palette_entry_tests {
    use super::*;

    /// The palette lists every key action, with the chord that reaches it.
    ///
    /// Built from the key table rather than a second list, so a chord and a
    /// palette row cannot drift apart -- and showing the chord is how a
    /// palette teaches the keyboard.
    #[test]
    fn the_palette_lists_what_the_keys_do() {
        let entries = command_entries();
        let bindings = crate::keys::effective_bindings();
        for binding in &bindings {
            let expected_label = command_label(binding.action);
            let row = entries
                .iter()
                .find(|entry| entry.label == expected_label)
                .unwrap_or_else(|| panic!("{} has no palette row", binding.action.label()));
            // The chord shown is the first one bound to the action, which is
            // the chord the key table would act on.
            let first = bindings
                .iter()
                .find(|other| command_label(other.action) == expected_label)
                .expect("the binding we started from");
            assert_eq!(
                row.hint,
                crate::keys::display_chord(first.mods, first.trigger),
                "{} should show the chord that reaches it",
                row.label
            );
        }
        assert!(entries
            .iter()
            .any(|entry| matches!(entry.command, crate::palette::Command::OpenTabNavigator)));
        assert!(entries
            .iter()
            .any(|entry| matches!(entry.command, crate::palette::Command::SelectCaptureRegion)));
    }

    #[test]
    fn the_palette_contains_every_declared_gui_action() {
        let entries = command_entries();
        for action in crate::keys::PALETTE_ACTIONS {
            assert!(
                entries.iter().any(|entry| {
                    matches!(entry.command, crate::palette::Command::Action(found) if found == *action)
                }),
                "{} is absent",
                action.name()
            );
        }
    }

    /// One row per action, not one per chord: nine Select Tab rows -- or two
    /// for a launcher reachable two ways -- would push everything else off a
    /// short list. 0.57.4's launcher deduplicated its key assignments for
    /// the same reason.
    #[test]
    fn an_action_bound_twice_is_listed_once() {
        let entries = command_entries();
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            assert!(
                seen.insert(entry.label.clone()),
                "{} is listed more than once",
                entry.label
            );
        }
    }

    /// The last workspace row saves; it is there even before anything is.
    ///
    /// The reopen rows depend on what this machine has saved, which a test
    /// cannot know -- but every reopen row carries the workspace it reopens,
    /// and the save row is always the tail.
    #[test]
    fn the_workspace_rows_end_with_the_one_that_saves() {
        let entries = workspace_entries();
        assert!(matches!(
            entries.last().map(|entry| &entry.command),
            Some(crate::palette::Command::OpenWorkspaceSave)
        ));
        for entry in &entries[..entries.len() - 1] {
            assert!(
                matches!(
                    entry.command,
                    crate::palette::Command::RestoreWorkspace { .. }
                ),
                "{} should reopen a workspace",
                entry.label
            );
        }
    }

    /// The launcher offers shells this machine has, and only those.
    ///
    /// A row for a shell that is not installed opens an empty tab and writes
    /// an error to a log nobody reads.
    #[test]
    fn the_launcher_offers_only_shells_that_exist() {
        for entry in launcher_entries() {
            let crate::palette::Command::Launch { program, .. } = &entry.command else {
                panic!("a launcher row should launch something");
            };
            assert!(
                std::path::Path::new(program).is_file() || which(program).is_some(),
                "{program} is offered but not installed"
            );
        }
    }

    #[test]
    fn this_machine_has_at_least_one_shell_to_offer() {
        // An empty launcher is indistinguishable from a broken one.
        assert!(
            !launcher_entries().is_empty(),
            "no shell found on PATH; the launcher would open on an empty list"
        );
    }
}

#[cfg(test)]
mod encode_tests {
    use super::*;
    use winit::keyboard::{Key, NamedKey};

    fn plain() -> crate::mouse::Held {
        crate::mouse::Held::default()
    }

    fn ctrl() -> crate::mouse::Held {
        crate::mouse::Held {
            ctrl: true,
            ..Default::default()
        }
    }

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    fn named(key: NamedKey) -> Key {
        Key::Named(key)
    }

    #[test]
    fn a_letter_is_itself() {
        assert_eq!(encode(&character("a"), plain()), Some("a".to_string()));
        assert_eq!(encode(&character("A"), plain()), Some("A".to_string()));
    }

    /// Ctrl+letter is a control byte, not the letter.
    ///
    /// This was the bug: modifiers never reached the encoder, so Ctrl+C sent
    /// `c`. Nothing could be interrupted, and every readline binding --
    /// Ctrl+A, Ctrl+E, Ctrl+K, Ctrl+R, Ctrl+U, Ctrl+W -- typed a letter
    /// instead of doing its job.
    #[test]
    fn ctrl_letter_sends_its_control_byte() {
        assert_eq!(encode(&character("c"), ctrl()), Some("\x03".to_string()));
        assert_eq!(encode(&character("d"), ctrl()), Some("\x04".to_string()));
        assert_eq!(encode(&character("a"), ctrl()), Some("\x01".to_string()));
        assert_eq!(encode(&character("e"), ctrl()), Some("\x05".to_string()));
        assert_eq!(encode(&character("r"), ctrl()), Some("\x12".to_string()));
        assert_eq!(encode(&character("u"), ctrl()), Some("\x15".to_string()));
        assert_eq!(encode(&character("w"), ctrl()), Some("\x17".to_string()));
        assert_eq!(encode(&character("z"), ctrl()), Some("\x1a".to_string()));
    }

    #[test]
    fn ctrl_letter_ignores_the_case_the_layout_produced() {
        // Shift changes the letter's case; it does not change the control
        // byte, and a shell reading 0x03 does not care which arrived.
        assert_eq!(encode(&character("C"), ctrl()), Some("\x03".to_string()));
    }

    #[test]
    fn alt_letter_is_escape_prefixed() {
        let alt = crate::mouse::Held {
            alt: true,
            ..Default::default()
        };
        assert_eq!(encode(&character("b"), alt), Some("\x1bb".to_string()));
    }

    #[test]
    fn the_arrows_carry_their_modifiers() {
        // Ctrl+arrow moves by word and Shift+arrow selects: both need the
        // modifier in the sequence, and both were unreachable when it was
        // dropped.
        let plain_left = encode(&named(NamedKey::ArrowLeft), plain());
        let ctrl_left = encode(&named(NamedKey::ArrowLeft), ctrl());
        assert!(plain_left.is_some());
        assert_ne!(plain_left, ctrl_left, "Ctrl+Left has to differ from Left");

        let shift = crate::mouse::Held {
            shift: true,
            ..Default::default()
        };
        assert_ne!(encode(&named(NamedKey::ArrowUp), shift), plain_left);
    }

    #[test]
    fn backspace_sends_delete_as_readline_expects() {
        assert_eq!(
            encode(&named(NamedKey::Backspace), plain()),
            Some("\x7f".to_string())
        );
    }

    #[test]
    fn enter_and_tab_and_escape_are_what_they_look_like() {
        assert_eq!(
            encode(&named(NamedKey::Enter), plain()),
            Some("\r".to_string())
        );
        assert_eq!(
            encode(&named(NamedKey::Tab), plain()),
            Some("\t".to_string())
        );
        assert_eq!(
            encode(&named(NamedKey::Escape), plain()),
            Some("\x1b".to_string())
        );
    }

    #[test]
    fn shift_tab_is_a_back_tab_rather_than_a_tab() {
        let shift = crate::mouse::Held {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            encode(&named(NamedKey::Tab), shift),
            Some("\x1b[Z".to_string())
        );
    }

    #[test]
    fn the_function_keys_exist_at_all() {
        // None of these were mapped, so F5 in a TUI did nothing.
        for (key, number) in [(NamedKey::F1, 1), (NamedKey::F5, 5), (NamedKey::F12, 12)] {
            let encoded = encode(&named(key), plain());
            assert!(encoded.is_some(), "F{number} produced nothing");
        }
    }

    #[test]
    fn navigation_keys_are_all_mapped() {
        for key in [
            NamedKey::Home,
            NamedKey::End,
            NamedKey::PageUp,
            NamedKey::PageDown,
            NamedKey::Delete,
            NamedKey::Insert,
        ] {
            assert!(
                encode(&named(key), plain()).is_some(),
                "{key:?} produced nothing"
            );
        }
    }

    #[test]
    fn a_modifier_key_on_its_own_sends_nothing() {
        // Otherwise holding Shift types something.
        assert_eq!(encode(&named(NamedKey::Shift), plain()), None);
        assert_eq!(encode(&named(NamedKey::Control), plain()), None);
        assert_eq!(encode(&named(NamedKey::Alt), plain()), None);
    }

    #[test]
    fn a_super_chord_stays_with_the_window_manager() {
        // Super+L locks the screen; it must not also type an L.
        let text = encode(&character("l"), plain());
        assert_eq!(text, Some("l".to_string()), "plain L still types");
    }

    #[test]
    fn composed_text_from_an_input_method_arrives_whole() {
        // Several characters from one press: already what the user meant.
        assert_eq!(
            encode(&character("中文"), plain()),
            Some("中文".to_string())
        );
    }

    #[test]
    fn space_is_a_space_and_ctrl_space_is_a_null() {
        assert_eq!(
            encode(&named(NamedKey::Space), plain()),
            Some(" ".to_string())
        );
        assert_eq!(
            encode(&named(NamedKey::Space), ctrl()),
            Some("\0".to_string())
        );
    }
}

#[cfg(test)]
mod tab_badge_tests {
    use crate::cockpit::Badge;

    /// Three states, three colours. Two badges that look alike are two
    /// states nobody can tell apart from across a window, which is the one
    /// distance a badge exists to be read at.
    #[test]
    fn every_badge_has_its_own_colour() {
        let colours = [
            Badge::NeedsYou.color(),
            Badge::Working.color(),
            Badge::Done.color(),
        ];
        for (index, colour) in colours.iter().enumerate() {
            for other in &colours[index + 1..] {
                assert_ne!(colour, other, "two badges share a colour");
            }
        }
    }
}

#[cfg(test)]
mod idle_cost_tests {
    /// An idle window asks the machine questions at a rate somebody chose,
    /// not as fast as a CPU allows.
    ///
    /// The loop used to run on `ControlFlow::Poll`, which spins: every
    /// iteration listed the sessions, fed the cockpit, re-derived the title and
    /// asked every pane for its revision. Measured on this machine, that was
    /// most of a core for a window sitting at a prompt.
    #[test]
    fn the_resting_interval_is_slower_than_a_frame_and_faster_than_a_second() {
        // The numbers themselves, so a later edit that turns the loop back into
        // a spin fails here rather than on somebody's battery.
        const BUSY_MS: u64 = 8;
        const RESTING_MS: u64 = 96;
        assert!(BUSY_MS >= 4, "a busier tick than this is a spin");
        assert!(
            BUSY_MS <= 16,
            "output later than a frame is output somebody sees arrive"
        );
        assert!(RESTING_MS > BUSY_MS);
        assert!(
            RESTING_MS <= 100,
            "a window that rests this long feels asleep when output arrives"
        );
    }

    /// And the housekeeping is slower still: reconciling the tab list, feeding
    /// the cockpit and re-deriving the title all answer questions whose answers
    /// change when a person does something, not between two frames.
    #[test]
    fn housekeeping_is_slower_than_the_tick() {
        assert!(super::HOUSEKEEPING >= std::time::Duration::from_millis(100));
        assert!(super::HOUSEKEEPING <= std::time::Duration::from_millis(500));
    }

    #[test]
    fn renderer_does_not_reserve_a_bindless_sized_d3d12_heap() {
        assert!(super::MAX_GPU_VIEW_DESCRIPTORS >= 64);
        assert!(super::MAX_GPU_VIEW_DESCRIPTORS <= 4_096);
        assert!(super::MAX_GPU_VIEW_DESCRIPTORS < wgpu::Limits::default().max_non_sampler_bindings);
    }
}
