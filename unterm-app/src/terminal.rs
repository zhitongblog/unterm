//! A terminal on a surface: next-core's screen, unterm-render's pixels.
//!
//! Deliberately holds no window and no event loop, so the part that decides
//! what a frame looks like can be tested without opening anything.

use crate::fonts::FontStack;
use unterm_engine::next_core::font_raster::FontFace;
use unterm_engine::next_core::{config::Config, font_discovery};
use unterm_engine::{StyledBlink, StyledCell, StyledScreenSnapshot};
use unterm_render::atlas::{GlyphAtlas, GlyphKey};
use unterm_render::quads::{build_row, CellMetrics, FrameColors, FrameQuads, Quad};

/// Pixels per em for a size in points on a display at `scale`.
///
/// A point is 1/72 inch, and winit reports scale against 96 dpi -- so the
/// pixel size is points * 96 * scale / 72. The previous front end did the same
/// arithmetic; this one skipped it and used the point size as pixels, which on
/// a 1.5x display drew every glyph at half the size it should be. That is most
/// of what "it still looks very different" was.
pub fn pixels_for_points(points: f32, scale: f32) -> u32 {
    const POINTS_PER_INCH: f32 = 72.0;
    // macOS speaks in its own points -- a 13pt font in Terminal, iTerm and
    // 0.57.4 alike is 13 logical pixels. Everything else keeps the 96dpi
    // convention the same number means on Windows.
    const NOMINAL_DPI: f32 = if cfg!(target_os = "macos") {
        72.0
    } else {
        96.0
    };
    // Never zero: a face opened at no pixels rasterizes nothing, and the
    // window comes up blank with no error to explain it.
    (((points.max(1.0) * NOMINAL_DPI * scale.max(0.1)) / POINTS_PER_INCH).round() as u32).max(1)
}

/// How a cursor is drawn when the program has not asked for a shape.
///
/// The config's own names, as the previous front end spelled them:
/// `SteadyBlock`, `BlinkingBlock`, `SteadyUnderline`, `BlinkingUnderline`,
/// `SteadyBar`, `BlinkingBar`. A program's own escape sequence still wins --
/// this is only what it looks like when nothing has said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blinking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blinking: false,
        }
    }
}

impl CursorStyle {
    /// Parse one of the config's names. Anything else is the default rather
    /// than an error: a typo should not stop the terminal opening, and a block
    /// cursor is the one nobody is surprised by.
    pub fn parse(name: &str) -> Self {
        let lowered = name.trim().to_lowercase();
        let blinking = lowered.starts_with("blinking");
        let shape = if lowered.ends_with("underline") {
            CursorShape::Underline
        } else if lowered.ends_with("bar") {
            CursorShape::Bar
        } else {
            CursorShape::Block
        };
        if !lowered.starts_with("blinking") && !lowered.starts_with("steady") {
            return Self::default();
        }
        Self { shape, blinking }
    }

    /// The style from the config, and how fast it blinks.
    ///
    /// Zero milliseconds means no blinking, which is how the setting has
    /// always been turned off.
    pub fn from_config(config: &Config) -> (Self, u64) {
        let style = config
            .str_of("default_cursor_style")
            .ok()
            .flatten()
            .map(|name| Self::parse(&name))
            .unwrap_or_default();
        let rate = config
            .float_of("cursor_blink_rate")
            .ok()
            .flatten()
            .map(|value| value.max(0.0) as u64)
            .unwrap_or(800);
        (style, rate)
    }
}

/// Whether a blinking cursor is showing at this moment.
///
/// Half the period on, half off. A rate of zero is not a blink of no length --
/// it is the setting turned off, and the cursor stays put.
pub fn blink_is_on(elapsed_ms: u128, rate_ms: u64) -> bool {
    if rate_ms == 0 {
        return true;
    }
    (elapsed_ms / rate_ms as u128) % 2 == 0
}

/// Which halves of the two text blink cadences are showing this frame.
///
/// SGR 5 and SGR 6 tick independently, each at its own configured rate. A
/// rate of zero is that cadence turned off, and its cells stay visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlinkPhase {
    pub slow_on: bool,
    pub rapid_on: bool,
}

impl BlinkPhase {
    /// Everything visible, for frames that do not animate.
    #[cfg(test)]
    pub const STEADY: Self = Self {
        slow_on: true,
        rapid_on: true,
    };

    pub fn at(elapsed_ms: u128, slow_rate_ms: u64, rapid_rate_ms: u64) -> Self {
        Self {
            slow_on: blink_is_on(elapsed_ms, slow_rate_ms),
            rapid_on: blink_is_on(elapsed_ms, rapid_rate_ms),
        }
    }

    /// Whether a cell with this blink attribute is in its invisible half.
    pub fn conceals(&self, blink: Option<StyledBlink>) -> bool {
        match blink {
            None => false,
            Some(StyledBlink::Slow) => !self.slow_on,
            Some(StyledBlink::Rapid) => !self.rapid_on,
        }
    }
}

/// The text blink rates from the config: (slow, rapid), in milliseconds.
///
/// The previous front end's defaults -- 500ms for SGR 5, 250ms for SGR 6 --
/// and zero turns a cadence off, as `text_blink_rate` always has.
pub fn text_blink_rates(config: &Config) -> (u64, u64) {
    let rate = |key: &str, fallback: f64| {
        config
            .float_of(key)
            .ok()
            .flatten()
            .unwrap_or(fallback)
            .max(0.0) as u64
    };
    (
        rate("text_blink_rate", 500.0),
        rate("text_blink_rate_rapid", 250.0),
    )
}

/// Which blink cadences the screen is using: (slow, rapid).
///
/// So the window can ask for frames only while something on screen actually
/// blinks -- a screen without blinking cells must not cost a redraw per phase.
pub fn blinking_cells(snapshot: &StyledScreenSnapshot) -> (bool, bool) {
    let mut slow = false;
    let mut rapid = false;
    for line in &snapshot.lines {
        for cell in &line.cells {
            match cell.style.blink {
                Some(StyledBlink::Slow) => slow = true,
                Some(StyledBlink::Rapid) => rapid = true,
                None => {}
            }
        }
    }
    (slow, rapid)
}

/// <https://developer.mozilla.org/en-US/docs/Web/CSS/easing-function>, by the
/// names the previous front end's config used. An unknown name is `Ease`,
/// which was its default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Easing {
    Linear,
    #[default]
    Ease,
    EaseIn,
    EaseInOut,
    EaseOut,
    Constant,
}

impl Easing {
    pub fn parse(name: &str) -> Self {
        match name.trim().to_lowercase().as_str() {
            "linear" => Self::Linear,
            "easein" => Self::EaseIn,
            "easeinout" => Self::EaseInOut,
            "easeout" => Self::EaseOut,
            "constant" => Self::Constant,
            _ => Self::Ease,
        }
    }

    /// The curve's value at `position` in 0..=1, the same cubic bezier
    /// arithmetic the previous front end used.
    fn evaluate(&self, position: f32) -> f32 {
        let [a, b, c, d] = match self {
            Self::Constant => [0.0, 0.0, 0.0, 0.0],
            Self::Linear => [0.0, 0.0, 1.0, 1.0],
            Self::Ease => [0.25, 0.1, 0.25, 1.0],
            Self::EaseIn => [0.42, 0.0, 1.0, 1.0],
            Self::EaseInOut => [0.42, 0.0, 0.58, 1.0],
            Self::EaseOut => [0.0, 0.0, 0.58, 1.0],
        };
        let x = position;
        (1.0 - x).powi(3) * a
            + 3.0 * (1.0 - x).powi(2) * x * b
            + 3.0 * (1.0 - x) * x.powi(2) * c
            + x.powi(3) * d
    }
}

/// What the visual bell colours: the whole background, or the cursor's cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BellTarget {
    #[default]
    BackgroundColor,
    CursorColor,
}

/// The visual bell: how its flash rises and falls, and what it colours.
///
/// The previous front end's `visual_bell` table, keys and defaults intact --
/// both durations zero by default, which is the bell not flashing at all.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VisualBell {
    pub fade_in_ms: u64,
    pub fade_out_ms: u64,
    pub fade_in: Easing,
    pub fade_out: Easing,
    pub target: BellTarget,
}

impl VisualBell {
    pub fn from_config(config: &Config) -> Self {
        let ms = |key: &str| config.float_of(key).ok().flatten().unwrap_or(0.0).max(0.0) as u64;
        let easing = |key: &str| {
            config
                .str_of(key)
                .ok()
                .flatten()
                .map(|name| Easing::parse(&name))
                .unwrap_or_default()
        };
        Self {
            fade_in_ms: ms("visual_bell.fade_in_duration_ms"),
            fade_out_ms: ms("visual_bell.fade_out_duration_ms"),
            fade_in: easing("visual_bell.fade_in_function"),
            fade_out: easing("visual_bell.fade_out_function"),
            target: match config.str_of("visual_bell.target").ok().flatten() {
                Some(name) if name.eq_ignore_ascii_case("cursorcolor") => BellTarget::CursorColor,
                _ => BellTarget::BackgroundColor,
            },
        }
    }

    /// A bell that would never show should never start a flash.
    pub fn disabled(&self) -> bool {
        self.fade_in_ms == 0 && self.fade_out_ms == 0
    }

    /// How strongly the flash shows `elapsed_ms` after the bell rang.
    ///
    /// One shot: rise over the fade-in, fall over the fade-out, then `None`
    /// -- the flash is over and stops asking for frames.
    pub fn intensity_at(&self, elapsed_ms: u128) -> Option<f32> {
        let elapsed = elapsed_ms as f32;
        let fade_in = self.fade_in_ms as f32;
        if elapsed < fade_in {
            return Some(self.fade_in.evaluate(elapsed / fade_in));
        }
        let completion = (elapsed - fade_in) / self.fade_out_ms as f32;
        // A zero fade-out divides to NaN or infinity; either way it is done.
        if !completion.is_finite() || completion >= 1.0 {
            return None;
        }
        Some(1.0 - self.fade_out.evaluate(completion))
    }
}

/// How far the cell is stretched around its glyphs.
///
/// Both default to one, which is the font's own metrics. They exist because a
/// terminal's readability is as much about the space between lines as about
/// the letters, and the config has always been able to say so.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub line_height: f32,
    pub cell_width: f32,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            line_height: 1.0,
            cell_width: 1.0,
        }
    }
}

impl Shape {
    /// From the config, refusing anything that would collapse the cell.
    ///
    /// A zero or negative multiplier is a grid with no rows, which divides by
    /// nothing and draws nothing; the clamp keeps a typo from producing a
    /// blank window with no explanation.
    pub fn from_config(config: &Config) -> Self {
        let number = |key: &str, fallback: f32| {
            config
                .float_of(key)
                .ok()
                .flatten()
                .map(|value| (value as f32).clamp(0.5, 4.0))
                .unwrap_or(fallback)
        };
        Self {
            line_height: number("line_height", 1.0),
            cell_width: number("cell_width", 1.0),
        }
    }
}

/// The font and the cell it dictates.
///
/// Cell size comes from the font rather than the other way round: a terminal
/// grid that does not match its glyphs either clips them or leaves gaps, and
/// both are visible on every character.
pub struct TerminalFont {
    stack: FontStack,
    metrics: CellMetrics,
    /// Which stack this is, for the atlas.
    ///
    /// Two fonts are open at once: the terminal's, and the chrome's at its own
    /// size. A face index means nothing without saying whose stack it indexes,
    /// and without this the two overwrite each other in the atlas whenever they
    /// land on the same pixel size.
    stack_id: u8,
    /// Each fallback glyph's natural `(advance, ink width, ink height)` at
    /// the base pixel size, measured once.
    ///
    /// `glyph_pixel_size` needs these every frame for every fallback glyph on
    /// screen, and measuring means rasterizing; without the cache a screen of
    /// CJK would re-rasterize itself once per glyph per frame just to decide
    /// what size to draw at.
    naturals: std::collections::HashMap<(usize, u32), (i32, i32, i32)>,
}

/// And how far down, when the ink would otherwise overflow its cell box.
///
/// Below this the glyph is unreadably small, and a face whose metrics ask for
/// it is a face whose metrics are broken; drawing slightly too big is the
/// lesser harm.
const MIN_FIT_SCALE: f32 = 0.5;

impl TerminalFont {
    /// Open the machine's default monospace face at `pixel_size`.
    #[cfg(test)]
    pub fn open(pixel_size: u32) -> anyhow::Result<Self> {
        let index = font_discovery::FontIndex::cached();
        let entry = index
            .default_monospace()
            .ok_or_else(|| anyhow::anyhow!("no monospace font found on this machine"))?;
        let face = FontFace::open_indexed(&entry.path, entry.face_index, pixel_size)?;
        Ok(Self::from_face(face, &[], pixel_size))
    }

    /// Open a named family, or the machine's default monospace.
    ///
    /// A name that is not installed falls back rather than refusing to start:
    /// a config naming a font from another machine is the ordinary case, and a
    /// terminal that will not open is no way to find out about it.
    pub fn open_named(
        family: Option<&str>,
        pixel_size: u32,
        fallbacks: &[String],
        shape: Shape,
    ) -> anyhow::Result<Self> {
        let index = font_discovery::FontIndex::cached();
        if let Some(name) = family {
            if index.family(name).is_empty() {
                log::warn!("font {name:?} is not installed; using the default");
            }
        }
        // A named family, at its regular weight -- `first()` here once handed
        // out whatever weight happened to be filed first. With nothing named,
        // the bundled JetBrains Mono is the default, as it was in 0.57.4: its
        // generous line gap is most of what made the old rows breathe.
        let entry = family.and_then(|name| index.best_in_family(name));
        let face = match entry {
            Some(entry) => FontFace::open_indexed(&entry.path, entry.face_index, pixel_size)?,
            None => match crate::fonts::bundled_face("JetBrainsMono-Regular.ttf", pixel_size) {
                Some(face) => face,
                None => {
                    let fallback = index.default_monospace().ok_or_else(|| {
                        anyhow::anyhow!("no monospace font found on this machine")
                    })?;
                    FontFace::open_indexed(&fallback.path, fallback.face_index, pixel_size)?
                }
            },
        };
        Ok(Self::from_face_shaped(face, fallbacks, pixel_size, shape))
    }

    #[cfg(test)]
    pub fn from_face(face: FontFace, fallbacks: &[String], pixel_size: u32) -> Self {
        Self::from_face_shaped(face, fallbacks, pixel_size, Shape::default())
    }

    /// As `from_face`, with the cell stretched by the config's multipliers.
    pub fn from_face_shaped(
        mut face: FontFace,
        fallbacks: &[String],
        pixel_size: u32,
        shape: Shape,
    ) -> Self {
        // The advance still comes from a real glyph — hinting and rounding
        // mean `M`'s advance is what the grid actually has to be — but the
        // row's height and baseline are the face's own line metrics. The
        // previous front end drew with the font's metrics, and inventing a
        // height from the capital's bearing drew every face 15-20% tighter
        // than its designer intended: the whole window read as cramped.
        let advance = match face.rasterize('M') {
            Ok(glyph) if glyph.advance_x > 0 => glyph.advance_x as f32,
            _ => face.pixel_size() as f32 * 0.6,
        };
        let (height, baseline) = match face.line_metrics() {
            Some((ascender, _descender, line_height)) if ascender > 0.0 => (line_height, ascender),
            _ => {
                let size = face.pixel_size() as f32;
                (size * 1.2, size)
            }
        };

        // The cell can be stretched without changing the glyphs: `line_height`
        // opens the text up without making it bigger, and `cell_width` is how
        // a narrow font is given room to breathe. The baseline moves with the
        // extra height so the text stays centred in the taller cell rather
        // than sitting on its old line with a gap underneath.
        let height = height * shape.line_height;
        let baseline = baseline + (height - baseline) * (shape.line_height - 1.0) * 0.5;

        Self {
            stack: FontStack::new(face, fallbacks, pixel_size),
            metrics: CellMetrics {
                width: advance * shape.cell_width,
                height,
                baseline,
            },
            stack_id: 0,
            naturals: std::collections::HashMap::new(),
        }
    }

    /// The same face, filed in the atlas under a different stack.
    ///
    /// Used for the chrome's own font: it is a different stack at a different
    /// size, and the atlas has to be able to tell the two apart.
    pub fn as_stack(mut self, stack_id: u8) -> Self {
        self.stack_id = stack_id;
        self
    }

    pub fn stack_id(&self) -> u8 {
        self.stack_id
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn stack_mut(&mut self) -> &mut FontStack {
        &mut self.stack
    }

    pub fn pixel_size(&self) -> u32 {
        self.stack.pixel_size()
    }

    /// The pixel size one glyph should be rasterized at so it fills the
    /// `span` columns its character occupies.
    ///
    /// The primary face *is* the grid -- its advance defined the cell -- so
    /// its glyphs are always taken at the base size, byte-identical to what
    /// this renderer always drew. A fallback face was designed for its own
    /// em: taken at the primary's size, a double-width CJK glyph is one em
    /// wide (about the pixel size) while its two cells add up to about 1.2
    /// ems of a monospace primary. Every hanzi came out visibly small, with
    /// a gap trailing it -- "星期日" read as "星 期 日".
    ///
    /// The rule: a fallback glyph keeps its natural size -- at the same
    /// pixel size as the primary it already agrees with the latin around it,
    /// and a CJK glyph sitting in 1.2 ems of cells with a little air at its
    /// sides is exactly how 0.57.4 set it. Enlarging it to *fill* the cells
    /// made every hanzi tower over the line it sits in. The only scaling is
    /// downward: the ink must stay inside `span` columns across and one row
    /// down, which is what shrinks a square symbol glyph into its single
    /// cell instead of letting it spill over its neighbour.
    ///
    /// This is the *one* place the size is decided. The ensure passes and
    /// the placement lookups all key the atlas through it, because two
    /// copies of this arithmetic disagreeing means a glyph filed under a key
    /// nothing asks for: it silently misses the atlas and the character
    /// vanishes.
    pub fn glyph_pixel_size(&mut self, face: usize, glyph_index: u32, span: usize) -> u32 {
        let base = self.stack.pixel_size();
        // The chrome's stack draws proportional text at the face's own
        // advances; it has no cells to fill. Only the terminal's grid fits.
        if face == 0 || self.stack_id != 0 {
            return base;
        }
        let (_advance, ink_width, ink_height) = match self.naturals.get(&(face, glyph_index)) {
            Some(natural) => *natural,
            None => {
                let natural = self
                    .stack
                    .rasterize_index(face, glyph_index)
                    .map(|glyph| (glyph.advance_x, glyph.width as i32, glyph.height as i32))
                    .unwrap_or((0, 0, 0));
                self.naturals.insert((face, glyph_index), natural);
                natural
            }
        };

        let target = self.metrics.width * span.max(1) as f32;
        // Natural size, shrink-only: ink decides everything. Growing a
        // hanzi toward its two cells was tried (2026-08-09) and measured
        // against macOS Terminal: the native rule is that CJK renders at
        // the SAME point size as the latin beside it — 都是一样大小 — and
        // air inside a wide grid's double cell is the grid's own geometry,
        // not a glyph too small.
        let mut scale = 1.0f32;
        // And never overflow the cell box: the ink itself has to stay inside
        // `span` columns and one row, which can pull the scale below 1.0 for
        // a glyph that was already too big.
        if ink_width > 0 {
            scale = scale.min(target / ink_width as f32);
        }
        if ink_height > 0 {
            scale = scale.min(self.metrics.height / ink_height as f32);
        }
        let scale = scale.max(MIN_FIT_SCALE);

        // Floored rather than rounded: rounding up re-crosses the line the
        // caps just drew.
        ((base as f32 * scale).floor() as u32).max(1)
    }

    /// How many cells fit in a window of this size.
    ///
    /// At least one of each: a zero-sized grid makes the PTY reject the resize
    /// and the shell draw nothing.
    pub fn grid_for(&self, width: f32, height: f32) -> (usize, usize) {
        // A window sized to exactly N cells divides to 23.999... rather than
        // 24, and flooring that loses a row: an empty strip along the bottom,
        // and a shell that believes it is shorter than the window shows. The
        // slack is far below a pixel, so it cannot claim a cell that is not
        // there.
        const SLACK: f32 = 1e-3;
        // Floored at the grid the engine will accept, not at one cell. This
        // is the last place a shrinking window is still a number: below here
        // it is a pane's size, and a pane told it is one column wide throws
        // away every line it holds and its scrollback with them. A window too
        // small to show two cells has chrome drawn over the grid; that is a
        // frame someone can fix by dragging, where the truncation is not.
        let floor = unterm_engine::MIN_SESSION_GRID as f32;
        let cols = (width / self.metrics.width + SLACK).floor().max(floor) as usize;
        let rows = (height / self.metrics.height + SLACK).floor().max(floor) as usize;
        (cols, rows)
    }
}

/// Turn a screen snapshot into the quads for one frame.
#[cfg(test)]
pub fn frame_quads(
    snapshot: &StyledScreenSnapshot,
    font: &mut TerminalFont,
    atlas: &mut GlyphAtlas,
    colors: FrameColors,
) -> FrameQuads {
    let mut quads = FrameQuads::default();
    append_pane(
        snapshot,
        font,
        atlas,
        colors,
        (0.0, 0.0),
        true,
        CursorStyle::default(),
        BlinkPhase::STEADY,
        &mut quads,
    );
    quads
}

/// Add one pane's quads at `origin`, in pixels from the window's top-left.
///
/// Panes are drawn into the same buffer rather than one each, so a window of
/// four splits is still one draw call. The origin is what a split needs and
/// the single-pane case gets for free at (0, 0).
#[allow(clippy::too_many_arguments)]
/// The part of a screen that fits in `cols` x `rows`, or `None` when all of it
/// already does -- the usual case, which then copies nothing.
///
/// A cursor outside the kept part is left where it is: `push_cursor` already
/// declines to draw one beyond the snapshot's own size, which this shrinks.
pub fn clip_to_grid(
    snapshot: &StyledScreenSnapshot,
    cols: usize,
    rows: usize,
) -> Option<StyledScreenSnapshot> {
    let fits = snapshot.lines.len() <= rows
        && snapshot.lines.iter().all(|line| line.cells.len() <= cols);
    if fits {
        return None;
    }
    let mut clipped = snapshot.clone();
    clipped.lines.truncate(rows);
    for line in &mut clipped.lines {
        line.cells.truncate(cols);
    }
    clipped.cols = clipped.cols.min(cols);
    clipped.rows = clipped.rows.min(rows);
    Some(clipped)
}

pub fn append_pane(
    snapshot: &StyledScreenSnapshot,
    font: &mut TerminalFont,
    atlas: &mut GlyphAtlas,
    colors: FrameColors,
    origin: (f32, f32),
    solid_cursor: bool,
    cursor: CursorStyle,
    blink: BlinkPhase,
    quads: &mut FrameQuads,
) {
    let metrics = font.metrics();

    // A blinking cell in its invisible half is drawn the way `hidden` is
    // drawn: background kept, glyph withheld. Rows without one borrow their
    // cells untouched, so a screen with nothing blinking -- almost every
    // screen -- shapes exactly what the snapshot holds, copying nothing.
    let lines: Vec<std::borrow::Cow<'_, [StyledCell]>> = snapshot
        .lines
        .iter()
        .map(|line| {
            if line
                .cells
                .iter()
                .any(|cell| blink.conceals(cell.style.blink))
            {
                std::borrow::Cow::Owned(
                    line.cells
                        .iter()
                        .map(|cell| {
                            let mut cell = cell.clone();
                            if blink.conceals(cell.style.blink) {
                                cell.style.hidden = true;
                            }
                            cell
                        })
                        .collect(),
                )
            } else {
                std::borrow::Cow::Borrowed(line.cells.as_slice())
            }
        })
        .collect();

    // Shape every row and rasterize everything this frame needs *before*
    // building any quads. The atlas grows, and growing it renormalizes every
    // texture coordinate -- so a glyph placed before a later one made the
    // atlas bigger ends up sampling the wrong pixels. It shows up as
    // characters missing from the middle of a word, which is how this was
    // found: "example.com" rendered as "exampl  com".
    let mut shaped_rows: Vec<Vec<(usize, ShapedRun)>> = Vec::with_capacity(lines.len());
    for cells in &lines {
        shaped_rows.push(shape_row(cells, font));
    }
    for cells in &lines {
        for cell in cells.iter() {
            ensure_glyph(font, atlas, cell.ch);
        }
    }
    for (row, shaped) in shaped_rows.iter().enumerate() {
        let cells = &lines[row];
        for (face, run) in shaped {
            for glyph in &run.glyphs {
                let (_, span) = glyph_cell(cells, &run.run, glyph.cluster as usize);
                ensure_shaped_glyph(font, atlas, *face, glyph.glyph_index, span);
            }
        }
    }

    // The key each character was filed under, resolved once, so the lookup
    // below matches it exactly -- face, index and fitted pixel size alike.
    let mut key_of: std::collections::HashMap<char, GlyphKey> = std::collections::HashMap::new();
    for cells in &lines {
        for cell in cells.iter() {
            if let std::collections::hash_map::Entry::Vacant(slot) = key_of.entry(cell.ch) {
                slot.insert(glyph_key(font, cell.ch));
            }
        }
    }

    // The cursor goes in before the glyphs so text lands on top of it, which is
    // what makes an inverted cell readable.
    let inverted = push_cursor(
        snapshot,
        metrics,
        colors,
        origin,
        solid_cursor,
        cursor,
        quads,
    );

    for (row, cells) in lines.iter().enumerate() {
        let top = origin.1 + row as f32 * metrics.height;

        // Backgrounds and any cell the shaper could not draw. Shaping runs
        // first so it can claim the columns it drew; what it leaves is drawn
        // a character at a time, which is right for a font with no ligatures
        // and the only option for one the shaper will not open.
        let shaped = place_shaped_row(
            &shaped_rows[row],
            cells,
            origin.0,
            top,
            metrics,
            colors,
            font,
            atlas,
            quads,
        );
        build_row(
            cells,
            origin.0,
            top,
            metrics,
            colors,
            atlas,
            |ch| key_of.get(&ch).and_then(|key| atlas.get(*key)),
            quads,
            &shaped,
        );
    }

    if let Some((column, row)) = inverted {
        // The cursor's own cell, and nothing around it. A glyph is placed by
        // its bearing, so it can start slightly left of its cell and its top
        // sits well above the cell's -- which is why this compares against
        // the cell the glyph *belongs to* rather than against a box drawn
        // around the cursor. A window of plus-or-minus one cell, which is
        // what this used to be, silently painted the row above the cursor in
        // the background colour: characters directly over the prompt
        // disappeared, and only there.
        let cell_of = |left: f32, top: f32| {
            (
                ((left - origin.0) / metrics.width.max(1.0))
                    .floor()
                    .max(0.0) as usize,
                ((top - origin.1) / metrics.height.max(1.0))
                    .floor()
                    .max(0.0) as usize,
            )
        };
        for glyph in &mut quads.glyphs {
            // Measured from the glyph's baseline row rather than its top: a
            // tall glyph starts above its own cell.
            let baseline_top = glyph.quad.top + glyph.quad.height;
            let (glyph_column, glyph_row) = cell_of(glyph.quad.left, baseline_top - 1.0);
            if glyph_column == column && glyph_row == row {
                glyph.quad.color = colors.background;
            }
        }
    }
}

/// Add a run of plain text at `origin`, in the given colour.
///
/// For the front end's own furniture -- a banner, a label -- which has no
/// cells and no styles, only characters that have to land on the same grid as
/// everything else.
pub fn append_text(
    text: &str,
    font: &mut TerminalFont,
    atlas: &mut GlyphAtlas,
    color: [f32; 4],
    origin: (f32, f32),
    quads: &mut FrameQuads,
) {
    for ch in text.chars() {
        ensure_glyph(font, atlas, ch);
    }
    let metrics = font.metrics();
    // A character's real width, not one cell each. Anything but Latin has
    // double-width characters in it, and a label that calls them one cell
    // wide draws the next character on top of the last -- which is what the
    // quick menu looked like the first time it was translated.
    let cells: Vec<StyledCell> = text
        .chars()
        .map(|ch| StyledCell {
            ch,
            style: Default::default(),
            width: column_width(ch),
        })
        .collect();
    // Keyed the same way `ensure_glyph` filed them. Working the key out
    // separately here is how the front end's own text came to be looked up by
    // code point while it was stored by glyph index: every label, banner and
    // bar drew whatever glyph happened to have that number, which is not the
    // letter asked for and often nothing at all.
    let mut key_of: std::collections::HashMap<char, GlyphKey> = std::collections::HashMap::new();
    for ch in text.chars() {
        if let std::collections::hash_map::Entry::Vacant(slot) = key_of.entry(ch) {
            slot.insert(glyph_key(font, ch));
        }
    }

    let before = quads.glyphs.len();
    build_row(
        &cells,
        origin.0,
        origin.1,
        metrics,
        FrameColors {
            foreground: color,
            // Transparent to the row builder: the caller has already drawn
            // whatever this sits on.
            background: color,
            // Furniture names its own colour; nothing here is themed.
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        },
        atlas,
        |ch| key_of.get(&ch).and_then(|key| atlas.get(*key)),
        quads,
        // Plain text, drawn a character at a time: the front end's own
        // furniture has no ligatures to find.
        &std::collections::HashSet::new(),
    );
    for glyph in &mut quads.glyphs[before..] {
        glyph.quad.color = color;
    }
}

/// Draw the cursor.
///
/// A block cursor sits *under* the character rather than over it, and the cell
/// beneath is drawn inverted: the block takes the foreground colour and the
/// character the background. Painting an opaque block on top would hide the
/// character the user is about to edit, which is the one they most need to see.
fn push_cursor(
    snapshot: &StyledScreenSnapshot,
    metrics: CellMetrics,
    colors: FrameColors,
    origin: (f32, f32),
    // Whether to draw the solid cursor rather than its outline. Two things
    // make it hollow: the pane not having the keyboard, and a blinking cursor
    // being mid-blink. An outline in both cases rather than nothing at all --
    // a cursor that vanishes entirely is one people lose track of, and the
    // outline still says where it is.
    solid: bool,
    style: CursorStyle,
    quads: &mut FrameQuads,
) -> Option<(usize, usize)> {
    let cursor = &snapshot.cursor;
    if !cursor.visible {
        return None;
    }
    // A negative row means the viewport is scrolled away from the cursor; it
    // has no place on screen and drawing it at row 0 would be a lie.
    let row = usize::try_from(cursor.y).ok()?;
    if row >= snapshot.rows || cursor.x >= snapshot.cols.max(1) {
        return None;
    }

    // A wide character owns two columns and its cursor must own both: a
    // one-cell block on a hanzi reads as a cursor wedged against half the
    // glyph. A cursor reported on the continuation cell snaps back to the
    // lead so the block covers the character rather than its tail.
    let mut x = cursor.x;
    let mut cells_wide = 1;
    if let Some(line) = snapshot.lines.get(row) {
        if x > 0 && line.cells.get(x).map_or(false, |cell| cell.width == 0) {
            x -= 1;
        }
        cells_wide = line.cells.get(x).map_or(1, |cell| cell.width.max(1));
    }
    let left = origin.0 + x as f32 * metrics.width;
    let top = origin.1 + row as f32 * metrics.height;
    let cell_span = metrics.width * cells_wide as f32;

    // This is how the previous front end said which pane had the keyboard --
    // rather than dimming the pane itself, which leaves a visible brightness
    // step down the split seam and into the status bar.
    if !solid {
        quads.backgrounds.extend(unterm_render::strokes::rectangle(
            left,
            top,
            cell_span,
            metrics.height,
            (metrics.height / 14.0).round().max(1.0),
            colors.foreground,
        ));
        // Nothing is inverted: the character underneath stays as it was.
        return None;
    }

    // Shapes as the escape sequences name them, falling back to what the
    // config asked for. An unknown shape draws a block rather than nothing: a
    // missing cursor is worse than an unexpected one.
    let named = if cursor.shape.trim().is_empty() {
        match style.shape {
            CursorShape::Bar => "Bar",
            CursorShape::Underline => "Underline",
            CursorShape::Block => "Block",
        }
    } else {
        cursor.shape.as_str()
    };
    let quad = match named {
        shape if shape.contains("Bar") => Quad {
            left,
            top,
            width: (metrics.width * 0.15).max(1.0),
            height: metrics.height,
            color: colors.foreground,
        },
        shape if shape.contains("Underline") => Quad {
            left,
            top: top + metrics.height - (metrics.height * 0.12).max(1.0),
            width: cell_span,
            height: (metrics.height * 0.12).max(1.0),
            color: colors.foreground,
        },
        _ => Quad {
            left,
            top,
            width: cell_span,
            height: metrics.height,
            color: colors.foreground,
        },
    };

    quads.backgrounds.push(quad);

    // Only a block covers the whole cell, so only a block needs the character
    // inverted to stay readable.
    let covers_cell = quad.width >= metrics.width && quad.height >= metrics.height;
    covers_cell.then_some((x, row))
}

/// Put a character in the atlas if it is not already there.
///
/// Keyed by character rather than by shaped glyph index: shaping comes later,
/// and a terminal's grid means most cells are one character to one glyph.
/// How many cells a character occupies.
///
/// From termwiz's tables rather than a guess at ranges: the kernel measures
/// the grid the same way, and furniture that disagrees with the grid it sits
/// on is furniture in the wrong place.
pub fn column_width(ch: char) -> usize {
    let mut buffer = [0u8; 4];
    termwiz::cell::unicode_column_width(ch.encode_utf8(&mut buffer), None).max(1)
}

fn ensure_glyph(font: &mut TerminalFont, atlas: &mut GlyphAtlas, ch: char) {
    if ch == ' ' || ch == '\0' {
        return;
    }
    let key = glyph_key(font, ch);
    if atlas.get(key).is_some() {
        return;
    }
    // At the key's own size, which is the fitted one: rasterizing at the base
    // size here would file a glyph whose picture disagrees with its key, and
    // the fitted lookup would find the wrong-sized bitmap.
    if let Some((_, glyph)) = font.stack_mut().rasterize_at(ch, key.pixel_size) {
        atlas.insert(key, &glyph);
    }
}

/// A run of a row, shaped.
pub struct ShapedRun {
    run: crate::shape::Run,
    glyphs: Vec<unterm_engine::next_core::font_shaper::ShapedGlyph>,
}

/// Shape every run of a row, without touching the atlas.
///
/// Separate from placing so that a whole frame can be shaped and rasterized
/// before any quad is built: the atlas grows, and growing it invalidates the
/// texture coordinates of everything already placed.
fn shape_row(cells: &[StyledCell], font: &mut TerminalFont) -> Vec<(usize, ShapedRun)> {
    let runs = {
        let stack = font.stack_mut();
        crate::shape::runs(cells, |ch| stack.face_for(ch))
    };

    let mut out = Vec::new();
    for run in runs {
        let Some(first) = run.text.chars().next() else {
            continue;
        };
        let face = font.stack_mut().face_for(first);
        let Some(glyphs) = font.stack_mut().shape(face, &run.text) else {
            continue;
        };
        out.push((face, ShapedRun { run, glyphs }));
    }
    out
}

/// Place a row's shaped glyphs, and say which columns they covered.
///
/// Glyphs go at the cell their cluster came from rather than at the pen
/// position shaping would have used. That is what keeps a ligature inside the
/// columns its characters occupied, and keeps a font whose advances drift from
/// the cell width from pulling the row out of alignment.
#[allow(clippy::too_many_arguments)]
fn place_shaped_row(
    rows: &[(usize, ShapedRun)],
    cells: &[StyledCell],
    left_origin: f32,
    top: f32,
    metrics: CellMetrics,
    colors: FrameColors,
    font: &mut TerminalFont,
    atlas: &GlyphAtlas,
    quads: &mut FrameQuads,
) -> std::collections::HashSet<usize> {
    let mut drawn = std::collections::HashSet::new();

    for (face, shaped) in rows {
        for glyph in &shaped.glyphs {
            let (column, span) = glyph_cell(cells, &shaped.run, glyph.cluster as usize);
            let Some(slot) = atlas.get(shaped_glyph_key(font, *face, glyph.glyph_index, span))
            else {
                // Not in the atlas: leave the column to the per-character
                // path rather than claiming it and drawing nothing.
                continue;
            };
            drawn.insert(column);
            if slot.width == 0 || slot.height == 0 {
                continue;
            }
            let cell = cells_at(cells, column);
            let (foreground, _) =
                unterm_render::quads::resolve_style(cell.map(|cell| &cell.style), colors);
            let mut pen = left_origin + column as f32 * metrics.width + glyph.x_offset as f32;
            if *face != 0 {
                // A fallback glyph sits centred in its columns: at natural
                // size a hanzi leaves ~0.3 cell of air in its two cells, and
                // left-flush placement piles all of it on the right -- the
                // date read as characters each dragging a gap. Split, the
                // air becomes even CJK letter-spacing.
                let air = (span as f32 * metrics.width - slot.width as f32).max(0.0);
                pen += air / 2.0 - slot.bearing_x as f32;
            }
            quads.glyphs.push(unterm_render::quads::glyph_quad(
                slot,
                pen,
                top + metrics.baseline - glyph.y_offset as f32,
                foreground,
                atlas,
            ));
        }
    }
    drawn
}

/// The column a shaped glyph's cluster came from, and how many columns that
/// cell occupies.
///
/// One function for the ensure pass and the placement pass both: the span
/// feeds the atlas key's pixel size, and the two passes computing it
/// differently would file a glyph under a key the lookup never asks for --
/// which is a character silently missing from the screen.
fn glyph_cell(cells: &[StyledCell], run: &crate::shape::Run, cluster: usize) -> (usize, usize) {
    let column = run.column_of(cluster);
    let span = cells_at(cells, column)
        .map(|cell| cell.width.max(1))
        .unwrap_or(1);
    (column, span)
}

/// The cell at a column, counting wide cells as the columns they occupy.
fn cells_at(cells: &[StyledCell], column: usize) -> Option<&StyledCell> {
    let mut at = 0usize;
    for cell in cells {
        // Spacers after wide characters hold no columns of their own.
        let width = cell.width;
        if width == 0 {
            continue;
        }
        if column < at + width {
            return Some(cell);
        }
        at += width;
    }
    None
}

/// Put a shaped glyph in the atlas, by the index the shaper reported.
///
/// A shaped glyph has no character: a ligature is one glyph for several, and
/// a positional form is a different glyph for the same one. So it is filed by
/// face and index, which is what the key was always made of.
fn ensure_shaped_glyph(
    font: &mut TerminalFont,
    atlas: &mut GlyphAtlas,
    face: usize,
    glyph_index: u32,
    span: usize,
) -> bool {
    let key = shaped_glyph_key(font, face, glyph_index, span);
    if atlas.get(key).is_some() {
        return true;
    }
    match font
        .stack_mut()
        .rasterize_index_at(face, glyph_index, key.pixel_size)
    {
        Some(glyph) => {
            atlas.insert(key, &glyph);
            true
        }
        None => false,
    }
}

/// Where a shaped glyph lives in the atlas.
///
/// The pixel size in the key is the fitted one, worked out in
/// `glyph_pixel_size` and nowhere else: the ensure pass files under this key
/// and `place_shaped_row` asks by it, so the two cannot drift.
fn shaped_glyph_key(
    font: &mut TerminalFont,
    face: usize,
    glyph_index: u32,
    span: usize,
) -> GlyphKey {
    GlyphKey {
        stack: font.stack_id(),
        face,
        glyph_index,
        pixel_size: font.glyph_pixel_size(face, glyph_index, span),
    }
}

/// Where a character lives in the atlas.
///
/// The face is part of the key because two faces' glyphs for the same
/// character are different pictures; filing a fallback glyph under the primary
/// would show one where the other belongs.
fn glyph_key(font: &mut TerminalFont, ch: char) -> GlyphKey {
    let stack = font.stack_id();
    let face = font.stack_mut().face_for(ch);
    // The face's own index for the character, not its code point. The
    // shaped path files glyphs by real index, and a code point standing
    // in for one collides with whatever glyph actually has that number:
    // the two entries overwrite each other and characters disappear from
    // the middle of a word.
    let glyph_index = font
        .stack_mut()
        .glyph_index_for(face, ch)
        .unwrap_or_default();
    GlyphKey {
        stack,
        face,
        glyph_index,
        // Fitted to the columns the character occupies, same as the shaped
        // path. A fallback glyph taken at the primary's size sits small in
        // its cells; one keyed at a size nothing rasterized vanishes.
        pixel_size: font.glyph_pixel_size(face, glyph_index, column_width(ch)),
    }
}

/// Frame colours: the theme, with the config's own colours on top.
///
/// A theme is the whole scheme -- background, foreground and the sixteen
/// colours programs ask for -- and `colors.background` / `colors.foreground`
/// still win where they are set, because someone who wrote those meant them.
pub fn colors_from(config: &Config) -> FrameColors {
    use unterm_engine::next_core::color::parse_hex;

    // The config first, then whatever was last chosen in the picker, then the
    // default. One order, decided here, so the picker and the config file
    // cannot each believe they won.
    let theme = config
        .str_of("theme")
        .ok()
        .flatten()
        .and_then(|id| crate::theme::by_id(&id))
        .or_else(|| crate::theme::remembered().and_then(|id| crate::theme::by_id(&id)))
        .unwrap_or_else(crate::theme::default_theme);

    let from_config = |key: &str| {
        config
            .str_of(key)
            .ok()
            .flatten()
            .and_then(parse_hex)
            .map(|color| {
                [
                    color.red as f32 / 255.0,
                    color.green as f32 / 255.0,
                    color.blue as f32 / 255.0,
                    1.0,
                ]
            })
    };

    FrameColors {
        background: from_config("colors.background").unwrap_or(theme.background),
        foreground: from_config("colors.foreground").unwrap_or(theme.foreground),
        palette: &theme.ansi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unterm_engine::{CellStyle, StyledCell, StyledScreenLine};

    fn font() -> Option<TerminalFont> {
        TerminalFont::open(16).ok()
    }

    fn snapshot(text: &str) -> StyledScreenSnapshot {
        StyledScreenSnapshot {
            lines: vec![StyledScreenLine {
                row: 0,
                wrapped: false,
                cells: text
                    .chars()
                    .map(|ch| StyledCell {
                        ch,
                        style: CellStyle::default(),
                        width: 1,
                    })
                    .collect(),
            }],
            cursor: unterm_engine::CursorSnapshot {
                x: 0,
                y: 0,
                visible: true,
                shape: "block".to_string(),
            },
            cols: text.chars().count(),
            rows: 1,
            scrollback_rows: 0,
            revision: 1,
            dirty_rows: None,
            mouse: Default::default(),
            bells: 0,
            notifications: 0,
            last_notification: None,
            focus_reporting: false,
            clipboard_request: None,
        }
    }

    #[test]
    fn a_cell_is_as_wide_as_the_font_says() {
        let Some(font) = font() else {
            return;
        };

        // A grid that does not match its glyphs clips them or leaves gaps, and
        // either is visible on every character.
        assert!(font.metrics().width > 0.0);
        assert!(font.metrics().height > font.metrics().width);
        assert!(font.metrics().baseline < font.metrics().height);
    }

    #[test]
    fn a_window_of_a_given_size_holds_a_sensible_grid() {
        let Some(font) = font() else {
            return;
        };
        let metrics = font.metrics();

        let (cols, rows) = font.grid_for(metrics.width * 80.0, metrics.height * 24.0);

        assert_eq!((cols, rows), (80, 24));
    }

    #[test]
    fn a_window_sized_to_exactly_n_cells_gets_n() {
        let Some(font) = font() else {
            return;
        };
        let metrics = font.metrics();

        // Dividing by the cell size lands just under the whole number, and
        // flooring that costs a row -- an empty strip along the bottom and a
        // shell that thinks it is shorter than the window.
        //
        // From `MIN_SESSION_GRID` up: below it the answer is the floor rather
        // than the arithmetic, which is what
        // `a_window_too_small_for_a_grid_still_asks_for_the_smallest_one`
        // covers.
        for rows in [unterm_engine::MIN_SESSION_GRID, 5, 24, 60] {
            assert_eq!(
                font.grid_for(metrics.width * 80.0, metrics.height * rows as f32)
                    .1,
                rows
            );
        }
    }

    #[test]
    fn a_window_too_small_for_a_grid_still_asks_for_the_smallest_one() {
        let Some(font) = font() else {
            return;
        };

        // A zero-sized grid makes the PTY reject the resize and the shell draw
        // nothing at all -- and a one-column one is worse than nothing, since
        // resizing a pane to a single column truncates every line it holds and
        // the whole scrollback behind it. The floor is what the engine accepts.
        let floor = unterm_engine::MIN_SESSION_GRID;
        assert_eq!(font.grid_for(1.0, 1.0), (floor, floor));
        assert_eq!(font.grid_for(0.0, 0.0), (floor, floor));
    }

    #[test]
    fn text_becomes_glyph_quads() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };

        let quads = frame_quads(&snapshot("hi"), &mut font, &mut atlas, colors);

        assert_eq!(quads.glyphs.len(), 2);
        // Left to right, one cell apart.
        assert!(quads.glyphs[1].quad.left > quads.glyphs[0].quad.left);
    }

    #[test]
    fn every_glyph_is_rasterized_before_any_quad_is_built() {
        let Some(mut font) = font() else {
            return;
        };
        // An atlas small enough that a line of text has to grow it.
        let mut atlas = GlyphAtlas::new(32, 32);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };

        let quads = frame_quads(&snapshot("abcdefghij"), &mut font, &mut atlas, colors);

        // Growing midway would leave the earlier quads' texture coordinates
        // pointing at the wrong pixels, and those glyphs would come out as
        // slices of their neighbours.
        let height = atlas.height() as f32;
        for glyph in &quads.glyphs {
            let expected_bottom = glyph.tex_bottom * height;
            assert!(
                expected_bottom <= height + 0.001,
                "texture coordinate outside the atlas: {glyph:?}"
            );
        }
    }

    #[test]
    fn a_character_the_primary_face_lacks_still_gets_ink() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };

        // Two Han characters a programming font almost never carries.
        let quads = frame_quads(&snapshot("\u{6f22}\u{5b57}"), &mut font, &mut atlas, colors);

        if quads.glyphs.is_empty() {
            // No CJK-capable face installed; nothing to assert on this machine.
            return;
        }

        // Without fallback these rasterize to glyph 0 -- the empty box -- which
        // is exactly what CJK looked like in the window before.
        for glyph in &quads.glyphs {
            assert!(
                glyph.quad.width > 0.0 && glyph.quad.height > 0.0,
                "a fallback glyph should have pixels: {glyph:?}"
            );
        }

        // And the ink has to be in the atlas, not just a slot with a size.
        let any_ink =
            (0..atlas.height()).any(|y| (0..atlas.width()).any(|x| atlas.pixel(x, y) > 0));
        assert!(any_ink, "the fallback face should have left ink");
    }

    fn snapshot_with_cursor(
        text: &str,
        x: usize,
        y: isize,
        visible: bool,
        shape: &str,
    ) -> StyledScreenSnapshot {
        let mut snap = snapshot(text);
        snap.cursor = unterm_engine::CursorSnapshot {
            x,
            y,
            visible,
            shape: shape.to_string(),
        };
        snap
    }

    #[test]
    fn the_cursor_is_drawn_where_the_screen_says() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let metrics = font.metrics();

        let quads = frame_quads(
            &snapshot_with_cursor("abc", 2, 0, true, "Block"),
            &mut font,
            &mut atlas,
            colors,
        );

        let cursor = quads
            .backgrounds
            .iter()
            .find(|quad| quad.color == colors.foreground)
            .expect("a visible cursor should be drawn");
        assert!((cursor.left - metrics.width * 2.0).abs() < 0.01);
    }

    #[test]
    fn a_block_cursor_on_a_wide_character_covers_both_its_cells() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let metrics = font.metrics();
        let wide_line = || {
            vec![
                StyledCell {
                    ch: '地',
                    style: CellStyle::default(),
                    width: 2,
                },
                StyledCell {
                    ch: ' ',
                    style: CellStyle::default(),
                    width: 0,
                },
                StyledCell {
                    ch: 'b',
                    style: CellStyle::default(),
                    width: 1,
                },
            ]
        };

        let mut snap = snapshot_with_cursor("xxx", 0, 0, true, "Block");
        snap.lines[0].cells = wide_line();
        let quads = frame_quads(&snap, &mut font, &mut atlas, colors);
        let cursor = quads
            .backgrounds
            .iter()
            .find(|quad| quad.color == colors.foreground)
            .expect("a visible cursor should be drawn");
        assert!((cursor.left - 0.0).abs() < 0.01);
        assert!((cursor.width - metrics.width * 2.0).abs() < 0.01);

        // Reported on the continuation cell, the cursor snaps to the lead
        // rather than straddling the glyph's tail and the next character.
        let mut snap = snapshot_with_cursor("xxx", 1, 0, true, "Block");
        snap.lines[0].cells = wide_line();
        let quads = frame_quads(&snap, &mut font, &mut atlas, colors);
        let cursor = quads
            .backgrounds
            .iter()
            .find(|quad| quad.color == colors.foreground)
            .expect("a visible cursor should be drawn");
        assert!((cursor.left - 0.0).abs() < 0.01);
        assert!((cursor.width - metrics.width * 2.0).abs() < 0.01);
    }

    #[test]
    fn a_hidden_cursor_is_not_drawn() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };

        let quads = frame_quads(
            &snapshot_with_cursor("abc", 1, 0, false, "Block"),
            &mut font,
            &mut atlas,
            colors,
        );

        assert!(!quads
            .backgrounds
            .iter()
            .any(|quad| quad.color == colors.foreground));
    }

    #[test]
    fn a_block_cursor_leaves_its_character_readable() {
        // A hosted CI runner has no desktop font install to probe -- what
        // this asserts is the machine, and the machines it speaks for are
        // the ones people run the product on.
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return;
        }
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };

        let quads = frame_quads(
            &snapshot_with_cursor("abc", 1, 0, true, "Block"),
            &mut font,
            &mut atlas,
            colors,
        );

        // The character under the block is the one the user is about to edit;
        // an opaque block on top of it hides exactly what they need to see.
        // Nearest-to-the-cell rather than first-within-a-cell: a face whose
        // `a` carries a left bearing would land that neighbour in a naive
        // window before the glyph the cursor is actually on.
        let under = quads
            .glyphs
            .iter()
            .min_by(|a, b| {
                let off = |glyph: &&unterm_render::quads::GlyphQuad| {
                    (glyph.quad.left - font.metrics().width).abs()
                };
                off(a).total_cmp(&off(b))
            })
            .expect("the character under the cursor should still be drawn");
        assert_eq!(under.quad.color, colors.background);
    }

    #[test]
    fn a_bar_cursor_does_not_cover_its_cell() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let metrics = font.metrics();

        let quads = frame_quads(
            &snapshot_with_cursor("abc", 0, 0, true, "SteadyBar"),
            &mut font,
            &mut atlas,
            colors,
        );

        let cursor = quads
            .backgrounds
            .iter()
            .find(|quad| quad.color == colors.foreground)
            .expect("a bar cursor should be drawn");
        assert!(cursor.width < metrics.width / 2.0);
        // A bar leaves the character alone, so it keeps the foreground colour.
        assert!(quads
            .glyphs
            .iter()
            .all(|glyph| glyph.quad.color == colors.foreground));
    }

    #[test]
    fn an_underline_cursor_sits_at_the_bottom_of_its_cell() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let metrics = font.metrics();

        let quads = frame_quads(
            &snapshot_with_cursor("abc", 0, 0, true, "SteadyUnderline"),
            &mut font,
            &mut atlas,
            colors,
        );

        let cursor = quads
            .backgrounds
            .iter()
            .find(|quad| quad.color == colors.foreground)
            .expect("an underline cursor should be drawn");
        assert!(cursor.top > metrics.height / 2.0);
        assert!(cursor.top + cursor.height <= metrics.height + 0.01);
    }

    #[test]
    fn a_cursor_off_the_screen_is_not_drawn() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };

        // Scrolled back far enough that the cursor is above the viewport.
        let quads = frame_quads(
            &snapshot_with_cursor("abc", 0, -3, true, "Block"),
            &mut font,
            &mut atlas,
            colors,
        );

        // Drawing it at row 0 instead would put the cursor somewhere it is not.
        assert!(!quads
            .backgrounds
            .iter()
            .any(|quad| quad.color == colors.foreground));
    }

    #[test]
    fn spaces_cost_nothing() {
        let Some(mut font) = font() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };

        let quads = frame_quads(&snapshot("   "), &mut font, &mut atlas, colors);

        assert!(quads.glyphs.is_empty());
        assert!(atlas.is_empty());
    }

    #[test]
    fn colours_come_from_the_config_when_it_states_them() {
        let config = unterm_engine::next_core::config::parse(
            "[colors]\nbackground = \"#102030\"\nforeground = \"#a0b0c0\"",
        )
        .expect("config should parse");

        let colors = colors_from(&config);

        assert!((colors.background[0] - 0x10 as f32 / 255.0).abs() < 0.01);
        assert!((colors.foreground[2] - 0xc0 as f32 / 255.0).abs() < 0.01);
    }

    #[test]
    fn a_config_without_colours_still_gives_readable_ones() {
        let config = unterm_engine::next_core::config::parse("font_size = 13").unwrap();

        let colors = colors_from(&config);

        // Foreground and background must differ, or the window is blank.
        assert_ne!(colors.foreground, colors.background);
    }
}

#[cfg(test)]
mod shaped_row_tests {
    use super::*;
    use unterm_engine::{CellStyle, StyledCell};

    /// Shape a row and place it, as `append_pane` does in two passes.
    fn shape_and_place(
        cells: &[StyledCell],
        font: &mut TerminalFont,
        atlas: &mut GlyphAtlas,
        colors: FrameColors,
        quads: &mut FrameQuads,
    ) -> std::collections::HashSet<usize> {
        let rows = shape_row(cells, font);
        for (face, run) in &rows {
            for glyph in &run.glyphs {
                let (_, span) = glyph_cell(cells, &run.run, glyph.cluster as usize);
                ensure_shaped_glyph(font, atlas, *face, glyph.glyph_index, span);
            }
        }
        let metrics = font.metrics();
        place_shaped_row(&rows, cells, 0.0, 0.0, metrics, colors, font, atlas, quads)
    }

    fn cells(text: &str) -> Vec<StyledCell> {
        text.chars()
            .map(|ch| StyledCell {
                ch,
                style: CellStyle::default(),
                width: if ch.is_ascii() { 1 } else { 2 },
            })
            .collect()
    }

    /// The shaper draws the row, not the per-character fallback.
    ///
    /// Both paths produce readable text, so a silent fall back to the old one
    /// looks identical on screen -- and takes ligatures and every complex
    /// script with it. What is checked here is that shaping claimed the
    /// columns, which is the only externally visible difference.
    #[test]
    fn shaping_claims_the_columns_it_drew() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let mut quads = FrameQuads::default();

        let drawn = shape_and_place(&cells("hello"), &mut font, &mut atlas, colors, &mut quads);

        assert_eq!(
            drawn.len(),
            5,
            "every column of a plain word should come from the shaper"
        );
        assert_eq!(quads.glyphs.len(), 5);
    }

    #[test]
    fn a_fallback_face_is_shaped_by_that_face() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        if font.stack_mut().rasterize('中').is_none() {
            return;
        }
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let mut quads = FrameQuads::default();

        let drawn = shape_and_place(&cells("中文"), &mut font, &mut atlas, colors, &mut quads);

        // Two characters, two columns apart -- a wide cell takes two.
        assert!(drawn.contains(&0), "first character drawn");
        assert!(drawn.contains(&2), "second character at column 2, not 1");
        assert_eq!(quads.glyphs.len(), 2);
    }
}

#[cfg(test)]
mod fallback_fit_tests {
    use super::*;
    use unterm_engine::{CellStyle, CursorSnapshot, StyledCell, StyledScreenLine};

    /// A one-line snapshot whose cells carry their real column widths, the
    /// way the engine reports them -- the module-level helper calls every
    /// cell one column wide, which is exactly the lie these tests exist to
    /// catch.
    fn snapshot_of(text: &str) -> StyledScreenSnapshot {
        let cells: Vec<StyledCell> = text
            .chars()
            .map(|ch| StyledCell {
                ch,
                style: CellStyle::default(),
                width: column_width(ch),
            })
            .collect();
        let cols = cells.iter().map(|cell| cell.width.max(1)).sum();
        StyledScreenSnapshot {
            lines: vec![StyledScreenLine {
                row: 0,
                wrapped: false,
                cells,
            }],
            cursor: CursorSnapshot {
                x: 0,
                y: 99,
                visible: false,
                shape: "Default".to_string(),
            },
            cols,
            rows: 1,
            scrollback_rows: 0,
            revision: 1,
            dirty_rows: None,
            mouse: Default::default(),
            bells: 0,
            notifications: 0,
            last_notification: None,
            focus_reporting: false,
            clipboard_request: None,
        }
    }

    fn colors() -> FrameColors {
        FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        }
    }

    /// A double-width character from a fallback face fills its two cells.
    ///
    /// Rasterizing every face at the primary's pixel size left a CJK em --
    /// about one pixel-size wide -- covering ~83% of two monospace cells:
    /// "星期日" read as "星 期 日", a visible gap trailing every hanzi.
    /// The grid stores a spacer cell after every wide character. It owns no
    /// columns: the character after CJK text must land immediately after the
    /// wide cells, not one phantom blank per hanzi later.
    #[test]
    fn a_spacer_after_a_wide_character_adds_no_column() {
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return;
        }
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(512, 512);

        let mut snapshot = snapshot_of("星b");
        // The spacer the engine stores under the wide character's tail.
        snapshot.lines[0].cells.insert(
            1,
            StyledCell {
                ch: ' ',
                style: CellStyle::default(),
                width: 0,
            },
        );
        let quads = frame_quads(&snapshot, &mut font, &mut atlas, colors());
        let metrics = font.metrics();
        let b = quads
            .glyphs
            .iter()
            .filter(|glyph| glyph.quad.width < metrics.width * 1.2)
            .min_by(|a, b| a.quad.left.total_cmp(&b.quad.left))
            .expect("the ascii after the hanzi should draw");
        // 'b' belongs to column 2. With the spacer miscounted it lands at
        // column 3, a full cell adrift of everything the program aligned.
        assert!(
            (b.quad.left - 2.0 * metrics.width).abs() < metrics.width * 0.9,
            "'b' should start at column 2, not {}",
            b.quad.left / metrics.width
        );
    }

    #[test]
    fn a_wide_fallback_glyph_fills_its_two_cells() {
        // A hosted CI runner has no CJK font install to probe.
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return;
        }
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        if font.stack_mut().face_for('星') == 0 {
            return; // The primary has its own CJK; nothing to fit.
        }
        let mut atlas = GlyphAtlas::new(512, 512);

        let quads = frame_quads(&snapshot_of("星"), &mut font, &mut atlas, colors());

        let metrics = font.metrics();
        let glyph = quads
            .glyphs
            .iter()
            .max_by(|a, b| a.quad.width.total_cmp(&b.quad.width))
            .expect("the hanzi should draw");
        assert!(
            glyph.quad.width > 1.1 * metrics.width,
            "a fallback hanzi is wider than one cell at its natural size: ink {} in {}-wide cells",
            glyph.quad.width,
            metrics.width
        );
        assert!(
            glyph.quad.width <= 2.0 * metrics.width + 1.0,
            "and never spill past them: ink {} in {}-wide cells",
            glyph.quad.width,
            metrics.width
        );
        assert!(
            glyph.quad.height <= metrics.height + 1.0,
            "nor grow taller than the row: ink {} in {}-tall cells",
            glyph.quad.height,
            metrics.height
        );
    }

    /// A single-width symbol from a fallback face stays inside its one cell.
    ///
    /// The bundled Nerd Font's icons are near-square: at their natural size
    /// they are wider than a monospace cell, and unfitted they overflowed
    /// into the neighbouring column.
    #[test]
    fn a_single_width_fallback_symbol_stays_in_its_cell() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        // The bundled symbols face: guaranteed present, never the primary.
        let ch = '\u{f07b}';
        if font.stack_mut().face_for(ch) == 0 {
            return; // No fallback carries it here; nothing to fit.
        }
        assert_eq!(column_width(ch), 1, "the test premise: a one-column icon");
        let mut atlas = GlyphAtlas::new(512, 512);

        let quads = frame_quads(
            &snapshot_of(&ch.to_string()),
            &mut font,
            &mut atlas,
            colors(),
        );

        let metrics = font.metrics();
        for glyph in &quads.glyphs {
            assert!(
                glyph.quad.left + glyph.quad.width <= metrics.width + 1.0,
                "a one-cell symbol must not overflow its cell: right edge {} vs cell {}",
                glyph.quad.left + glyph.quad.width,
                metrics.width
            );
            assert!(
                glyph.quad.height <= metrics.height + 1.0,
                "nor its row: ink {} in {}-tall cells",
                glyph.quad.height,
                metrics.height
            );
        }
    }

    /// ASCII from the primary face is untouched by the fitting.
    ///
    /// The primary's advance defined the grid, so its glyphs are the
    /// reference the fix must not move: keys stay at the base pixel size and
    /// every quad sits exactly where the unfitted arithmetic put it.
    #[test]
    fn ascii_from_the_primary_face_is_untouched() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        let base = font.pixel_size();
        let mut atlas = GlyphAtlas::new(512, 512);

        let quads = frame_quads(&snapshot_of("Hi"), &mut font, &mut atlas, colors());

        let metrics = font.metrics();
        let mut glyphs: Vec<_> = quads.glyphs.iter().collect();
        glyphs.sort_by(|a, b| a.quad.left.total_cmp(&b.quad.left));
        assert_eq!(glyphs.len(), 2);
        for (column, ch) in "Hi".chars().enumerate() {
            let key = glyph_key(&mut font, ch);
            assert_eq!(
                key.pixel_size, base,
                "{ch:?} must be keyed at the base size"
            );
            let (face, natural) = font.stack_mut().rasterize(ch).expect("ASCII rasterizes");
            assert_eq!(face, 0, "ASCII comes from the primary");
            // The exact quad the pre-fitting renderer built: cell origin plus
            // the natural bearing, the natural ink size, on the baseline.
            let quad = &glyphs[column].quad;
            assert_eq!(
                quad.left,
                column as f32 * metrics.width + natural.bearing_x as f32
            );
            assert_eq!(quad.top, metrics.baseline - natural.bearing_y as f32);
            assert_eq!(quad.width, natural.width as f32);
            assert_eq!(quad.height, natural.height as f32);
        }
    }
}

#[cfg(test)]
mod missing_glyph_regression {
    use super::*;
    use unterm_engine::{CellStyle, CursorSnapshot, StyledCell, StyledScreenLine};

    /// Every non-blank column of a line gets a glyph.
    ///
    /// "see https://example.com here" rendered as "see https://exampl  com
    /// here" -- two characters silently gone from the middle of a word. This
    /// is the whole pipeline, because each piece checked out on its own.
    #[test]
    fn every_column_of_a_line_is_drawn() {
        for pixel_size in [12, 13, 14, 16, 18, 20] {
            check_every_column_drawn(pixel_size);
        }
    }

    fn check_every_column_drawn(pixel_size: u32) {
        let Ok(mut font) = TerminalFont::open(pixel_size) else {
            return;
        };
        // Three rows, because the same text drew correctly on the first row
        // and lost two characters on the third.
        let rows = [
            "see https://example.com here",
            "aaa bbbbbbbbbbbbbbbbbbb ccc",
            "see https://exampleXcom here",
        ];
        let text = rows[2];
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let mut quads = FrameQuads::default();

        let snapshot = StyledScreenSnapshot {
            lines: rows
                .iter()
                .enumerate()
                .map(|(index, line)| StyledScreenLine {
                    row: index as i64,
                    wrapped: false,
                    cells: line
                        .chars()
                        .map(|ch| StyledCell {
                            ch,
                            style: CellStyle::default(),
                            width: 1,
                        })
                        .collect(),
                })
                .collect(),
            cursor: CursorSnapshot {
                x: 0,
                y: 99,
                visible: false,
                shape: "Default".to_string(),
            },
            cols: text.len(),
            rows: rows.len(),
            scrollback_rows: 0,
            revision: 1,
            dirty_rows: None,
            mouse: Default::default(),
            bells: 0,
            notifications: 0,
            last_notification: None,
            focus_reporting: false,
            clipboard_request: None,
        };

        append_pane(
            &snapshot,
            &mut font,
            &mut atlas,
            colors,
            (0.0, 0.0),
            true,
            CursorStyle::default(),
            BlinkPhase::STEADY,
            &mut quads,
        );

        let metrics = font.metrics();
        let last_row_top = 2.0 * metrics.height;
        let drawn: std::collections::HashSet<usize> = quads
            .glyphs
            .iter()
            .filter(|glyph| glyph.quad.top >= last_row_top - metrics.height * 0.5)
            .map(|glyph| (glyph.quad.left / metrics.width).round() as usize)
            .collect();
        let missing: Vec<(usize, char)> = text
            .chars()
            .enumerate()
            .filter(|(column, ch)| *ch != ' ' && !drawn.contains(column))
            .collect();
        assert!(
            missing.is_empty(),
            "at {pixel_size}px, columns with no glyph: {missing:?}"
        );
    }
}

#[cfg(test)]
mod cursor_inversion_tests {
    use super::*;
    use unterm_engine::{CellStyle, CursorSnapshot, StyledCell, StyledScreenLine};

    fn snapshot(rows: &[&str], cursor: (usize, isize)) -> StyledScreenSnapshot {
        StyledScreenSnapshot {
            lines: rows
                .iter()
                .enumerate()
                .map(|(index, line)| StyledScreenLine {
                    row: index as i64,
                    wrapped: false,
                    cells: line
                        .chars()
                        .map(|ch| StyledCell {
                            ch,
                            style: CellStyle::default(),
                            width: 1,
                        })
                        .collect(),
                })
                .collect(),
            cursor: CursorSnapshot {
                x: cursor.0,
                y: cursor.1,
                visible: true,
                shape: "Default".to_string(),
            },
            cols: rows.iter().map(|line| line.len()).max().unwrap_or(1),
            rows: rows.len(),
            scrollback_rows: 0,
            revision: 1,
            dirty_rows: None,
            mouse: Default::default(),
            bells: 0,
            notifications: 0,
            last_notification: None,
            focus_reporting: false,
            clipboard_request: None,
        }
    }

    /// A screen bigger than its pane is drawn only as far as the pane goes.
    ///
    /// A shrink waits before it reaches the pane, so for a moment the screen
    /// can outsize the space it is drawn in -- and drawing all of it put its
    /// last rows on the status bar and its right-hand columns on the pane
    /// beside it.
    #[test]
    fn a_screen_is_clipped_to_the_pane_it_is_drawn_in() {
        let fits = snapshot(&["abc", "def"], (0, 0));
        assert!(clip_to_grid(&fits, 3, 2).is_none(), "nothing to clip, nothing copied");

        let big = snapshot(&["abcdef", "ghijkl", "mnopqr"], (5, 2));
        let clipped = clip_to_grid(&big, 4, 2).expect("wider and taller than the pane");
        let rows: Vec<String> = clipped
            .lines
            .iter()
            .map(|line| line.cells.iter().map(|cell| cell.ch).collect())
            .collect();
        assert_eq!(rows, vec!["abcd", "ghij"]);
        assert_eq!((clipped.cols, clipped.rows), (4, 2));
        // The cursor is outside what is kept, and `push_cursor` measures it
        // against the clipped size -- so it is not drawn on the pane's edge.
        assert!(clipped.cursor.x >= clipped.cols || clipped.cursor.y as usize >= clipped.rows);
    }

    /// The block cursor inverts its own cell and no other.
    ///
    /// It used to invert everything within one cell in each direction, which
    /// painted the row above the cursor in the background colour: characters
    /// sitting directly over the prompt disappeared, and only there. It looked
    /// like a shaping bug for a long time because the text was fine, the
    /// quads were fine, and the pixels were not.
    #[test]
    fn the_cursor_inverts_its_own_cell_and_no_other() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let mut quads = FrameQuads::default();

        // Text on the row above, cursor at column 2 of the row below.
        let snapshot = snapshot(&["abcde", "abcde"], (2, 1));
        append_pane(
            &snapshot,
            &mut font,
            &mut atlas,
            colors,
            (0.0, 0.0),
            true,
            CursorStyle::default(),
            BlinkPhase::STEADY,
            &mut quads,
        );

        let metrics = font.metrics();
        let above: Vec<_> = quads
            .glyphs
            .iter()
            .filter(|glyph| glyph.quad.top + glyph.quad.height < metrics.height * 1.5)
            .collect();
        assert_eq!(above.len(), 5, "the row above should be fully drawn");
        for glyph in above {
            assert_eq!(
                glyph.quad.color, colors.foreground,
                "a glyph on the row above the cursor was painted the background colour"
            );
        }

        let inverted = quads
            .glyphs
            .iter()
            .filter(|glyph| glyph.quad.color == colors.background)
            .count();
        assert_eq!(inverted, 1, "exactly the cursor's own cell");
    }
}

#[cfg(test)]
mod furniture_tests {
    use super::*;

    /// The front end's own text has to be looked up the way it was stored.
    ///
    /// These were two separate calculations, and they drifted: `ensure_glyph`
    /// filed by the face's real glyph index while `append_text` asked by code
    /// point. Every label, banner and bar then drew whichever glyph happened
    /// to carry that number -- a scattering of wrong letters where the status
    /// bar's path should have been. One key, worked out in one place.
    #[test]
    fn a_labels_glyphs_are_found_where_they_were_put() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return; // No usable system font on this machine.
        };
        let mut atlas = GlyphAtlas::new(256, 256);

        let text = r"D:\code\unterm";
        for ch in text.chars() {
            ensure_glyph(&mut font, &mut atlas, ch);
        }
        for ch in text.chars().filter(|ch| *ch != ' ') {
            let key = glyph_key(&mut font, ch);
            assert!(
                atlas.get(key).is_some(),
                "{ch:?} was stored under a key nothing asks for"
            );
        }
    }

    /// And the text actually reaches the frame: a lookup that quietly misses
    /// produces no glyph and no error, which is exactly how this hid.
    #[test]
    fn a_label_draws_one_glyph_per_visible_character() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let mut quads = FrameQuads::default();

        append_text(
            "mcp:7",
            &mut font,
            &mut atlas,
            [1.0, 1.0, 1.0, 1.0],
            (0.0, 0.0),
            &mut quads,
        );
        assert_eq!(quads.glyphs.len(), 5, "one per character, none dropped");
    }

    /// Laid out left to right on the grid, so a bar's columns line up with
    /// the terminal's.
    #[test]
    fn a_labels_characters_advance_by_one_cell_each() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let mut quads = FrameQuads::default();
        let width = font.metrics().width;

        append_text(
            "abc",
            &mut font,
            &mut atlas,
            [1.0, 1.0, 1.0, 1.0],
            (0.0, 0.0),
            &mut quads,
        );
        let lefts: Vec<f32> = quads.glyphs.iter().map(|g| g.quad.left).collect();
        assert_eq!(lefts.len(), 3);
        for (index, left) in lefts.iter().enumerate() {
            let expected = index as f32 * width;
            assert!(
                (left - expected).abs() <= width,
                "character {index} sits at {left}, not near {expected}"
            );
        }
    }
}

#[cfg(test)]
mod width_tests {
    use super::*;

    /// A label in Chinese, Japanese or Korean is mostly double-width
    /// characters. Drawing each one cell wide puts the next character on top
    /// of the last, which is what the translated quick menu looked like.
    #[test]
    fn a_wide_character_takes_two_cells() {
        assert_eq!(column_width('中'), 2);
        assert_eq!(column_width('あ'), 2);
        assert_eq!(column_width('한'), 2);
    }

    #[test]
    fn latin_takes_one() {
        assert_eq!(column_width('a'), 1);
        assert_eq!(column_width(' '), 1);
        assert_eq!(column_width('→'), 1);
    }

    /// Never zero: a combining mark that advances nothing would stack the
    /// whole rest of the label in one cell.
    #[test]
    fn nothing_is_narrower_than_a_cell() {
        for ch in ['\u{0301}', '\u{200b}', '\u{0}'] {
            assert!(column_width(ch) >= 1, "{ch:?} advanced nothing");
        }
    }

    /// And the label lays out accordingly: two Latin characters after a wide
    /// one start three cells in, not two.
    #[test]
    fn a_label_after_a_wide_character_is_not_drawn_on_top_of_it() {
        // A hosted CI runner has no desktop font install to probe -- what
        // this asserts is the machine, and the machines it speaks for are
        // the ones people run the product on.
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return;
        }
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let mut quads = FrameQuads::default();
        let width = font.metrics().width;

        append_text(
            "中a",
            &mut font,
            &mut atlas,
            [1.0; 4],
            (0.0, 0.0),
            &mut quads,
        );
        assert_eq!(quads.glyphs.len(), 2);
        let gap = quads.glyphs[1].quad.left - quads.glyphs[0].quad.left;
        assert!(
            gap >= width * 1.5,
            "the Latin character sits {gap} from the wide one, cell is {width}"
        );
    }
}

#[cfg(test)]
mod dpi_tests {
    use super::*;

    /// What a point means is the platform's convention: on macOS 13pt IS
    /// 13px, as Terminal and iTerm read the same number; everywhere else it
    /// is the 96dpi kind, where using the point size as pixels comes out a
    /// third smaller than asked for.
    #[test]
    fn points_follow_the_platforms_convention() {
        if cfg!(target_os = "macos") {
            assert_eq!(pixels_for_points(13.0, 1.0), 13);
            assert_eq!(pixels_for_points(12.0, 1.0), 12);
        } else {
            assert_eq!(pixels_for_points(13.0, 1.0), 17);
            assert_eq!(pixels_for_points(12.0, 1.0), 16);
        }
    }

    /// And the scale multiplies. This is the one that was missing entirely.
    #[test]
    fn a_scaled_display_gets_proportionally_more_pixels() {
        // Within a pixel of proportional: the rounding happens after the
        // scale is applied, not to the 1x answer first.
        let one = pixels_for_points(13.0, 1.0) as f32;
        let two = pixels_for_points(13.0, 2.0) as f32;
        let one_and_a_half = pixels_for_points(13.0, 1.5) as f32;
        assert!((two - one * 2.0).abs() <= 1.0, "{one} then {two}");
        assert!(
            (one_and_a_half - one * 1.5).abs() <= 1.0,
            "a 1.5x panel at 13pt: {one} then {one_and_a_half}"
        );
    }

    /// Never zero, whatever nonsense arrives: a face opened at zero pixels
    /// rasterizes nothing and the window is blank.
    #[test]
    fn nothing_rounds_away_to_no_font_at_all() {
        assert!(pixels_for_points(0.0, 1.0) > 0);
        assert!(pixels_for_points(13.0, 0.0) > 0);
        assert!(pixels_for_points(-5.0, -5.0) > 0);
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    fn face() -> Option<FontFace> {
        let index = font_discovery::FontIndex::scan();
        FontFace::open(&index.default_monospace()?.path, 20).ok()
    }

    /// The default is the font's own metrics, untouched.
    #[test]
    fn the_default_shape_changes_nothing() {
        let Some(plain) = face().map(|f| TerminalFont::from_face(f, &[], 20)) else {
            return;
        };
        let Some(shaped) =
            face().map(|f| TerminalFont::from_face_shaped(f, &[], 20, Shape::default()))
        else {
            return;
        };
        assert_eq!(plain.metrics(), shaped.metrics());
    }

    /// A taller line is taller, and no wider.
    #[test]
    fn line_height_opens_the_rows_without_widening_them() {
        let Some(plain) = face().map(|f| TerminalFont::from_face(f, &[], 20)) else {
            return;
        };
        let shape = Shape {
            line_height: 1.4,
            cell_width: 1.0,
        };
        let tall = TerminalFont::from_face_shaped(face().unwrap(), &[], 20, shape);
        assert!(tall.metrics().height > plain.metrics().height);
        assert_eq!(tall.metrics().width, plain.metrics().width);
    }

    /// And a wider cell is wider, and no taller.
    #[test]
    fn cell_width_opens_the_columns_without_heightening_them() {
        let Some(plain) = face().map(|f| TerminalFont::from_face(f, &[], 20)) else {
            return;
        };
        let shape = Shape {
            line_height: 1.0,
            cell_width: 1.3,
        };
        let wide = TerminalFont::from_face_shaped(face().unwrap(), &[], 20, shape);
        assert!(wide.metrics().width > plain.metrics().width);
        assert_eq!(wide.metrics().height, plain.metrics().height);
    }

    /// The text stays inside the taller cell rather than sitting on its old
    /// line with the gap all underneath.
    #[test]
    fn a_taller_line_keeps_its_text_off_the_bottom() {
        let Some(_) = face() else { return };
        let shape = Shape {
            line_height: 1.6,
            cell_width: 1.0,
        };
        let tall = TerminalFont::from_face_shaped(face().unwrap(), &[], 20, shape);
        let metrics = tall.metrics();
        assert!(metrics.baseline < metrics.height, "{metrics:?}");
        assert!(metrics.baseline > metrics.height * 0.4, "{metrics:?}");
    }
}

#[cfg(test)]
mod focus_cursor_tests {
    use super::*;
    use unterm_engine::{CellStyle, CursorSnapshot, StyledCell, StyledScreenLine};

    fn screen() -> StyledScreenSnapshot {
        StyledScreenSnapshot {
            lines: vec![StyledScreenLine {
                row: 0,
                wrapped: false,
                cells: "ab"
                    .chars()
                    .map(|ch| StyledCell {
                        ch,
                        style: CellStyle::default(),
                        width: 1,
                    })
                    .collect(),
            }],
            cursor: CursorSnapshot {
                x: 0,
                y: 0,
                visible: true,
                shape: "block".to_string(),
            },
            cols: 2,
            rows: 1,
            scrollback_rows: 0,
            revision: 1,
            dirty_rows: None,
            mouse: Default::default(),
            bells: 0,
            notifications: 0,
            last_notification: None,
            focus_reporting: false,
            clipboard_request: None,
        }
    }

    fn drawn(focused: bool) -> Option<FrameQuads> {
        let mut font = TerminalFont::open(16).ok()?;
        let mut atlas = GlyphAtlas::new(256, 256);
        let mut quads = FrameQuads::default();
        let snapshot = screen();
        append_pane(
            &snapshot,
            &mut font,
            &mut atlas,
            FrameColors {
                foreground: [1.0; 4],
                background: [0.0, 0.0, 0.0, 1.0],
                palette: &unterm_render::quads::DEFAULT_PALETTE,
            },
            (0.0, 0.0),
            focused,
            CursorStyle::default(),
            BlinkPhase::STEADY,
            &mut quads,
        );
        Some(quads)
    }

    /// Which pane has the keyboard is said by the cursor, not by dimming the
    /// pane: dimming leaves a brightness step down the split seam and into the
    /// status bar, which is why the previous front end chose this instead.
    #[test]
    fn an_unfocused_pane_gets_the_outline_of_its_cursor() {
        let Some(focused) = drawn(true) else {
            return; // No usable system font on this machine.
        };
        let unfocused = drawn(false).unwrap();

        let solid = |quads: &FrameQuads| {
            quads
                .backgrounds
                .iter()
                .any(|quad| quad.width > 4.0 && quad.height > 8.0 && quad.color == [1.0; 4])
        };
        assert!(
            solid(&focused),
            "the focused pane's cursor is a solid block"
        );
        assert!(!solid(&unfocused), "the unfocused one's is not");
        assert!(
            unfocused.backgrounds.len() > 1,
            "but it is drawn: {:?}",
            unfocused.backgrounds
        );
    }

    /// And the character under it stays as it was: an outline does not
    /// invert, so an unfocused pane reads exactly as its text.
    #[test]
    fn an_outlined_cursor_does_not_invert_its_character() {
        let Some(focused) = drawn(true) else {
            return;
        };
        let unfocused = drawn(false).unwrap();
        let inverted = |quads: &FrameQuads| {
            quads
                .glyphs
                .iter()
                .any(|glyph| glyph.quad.color == [0.0, 0.0, 0.0, 1.0])
        };
        assert!(inverted(&focused), "the focused cursor inverts its cell");
        assert!(!inverted(&unfocused), "the outlined one leaves it alone");
    }
}

#[cfg(test)]
mod cursor_style_tests {
    use super::*;

    /// The config's own six names.
    #[test]
    fn every_named_style_parses_to_what_it_says() {
        let cases = [
            ("SteadyBlock", CursorShape::Block, false),
            ("BlinkingBlock", CursorShape::Block, true),
            ("SteadyUnderline", CursorShape::Underline, false),
            ("BlinkingUnderline", CursorShape::Underline, true),
            ("SteadyBar", CursorShape::Bar, false),
            ("BlinkingBar", CursorShape::Bar, true),
        ];
        for (name, shape, blinking) in cases {
            let style = CursorStyle::parse(name);
            assert_eq!(style.shape, shape, "{name}");
            assert_eq!(style.blinking, blinking, "{name}");
        }
    }

    /// Case is not the point: a config written by hand says `steadybar` as
    /// often as `SteadyBar`.
    #[test]
    fn the_names_are_not_case_sensitive() {
        assert_eq!(CursorStyle::parse("steadybar").shape, CursorShape::Bar);
        assert_eq!(CursorStyle::parse("BLINKINGBAR").blinking, true);
    }

    /// A typo is a block cursor, not a refusal to open: nobody is surprised by
    /// a block, and a terminal that will not start is no way to report one.
    #[test]
    fn an_unknown_name_is_the_ordinary_cursor() {
        assert_eq!(CursorStyle::parse("wobbly"), CursorStyle::default());
        assert_eq!(CursorStyle::parse(""), CursorStyle::default());
        assert_eq!(CursorStyle::default().shape, CursorShape::Block);
    }

    /// Half the period on, half off.
    #[test]
    fn a_blink_is_on_for_half_its_period() {
        assert!(blink_is_on(0, 800));
        assert!(blink_is_on(799, 800));
        assert!(!blink_is_on(800, 800));
        assert!(!blink_is_on(1599, 800));
        assert!(blink_is_on(1600, 800));
    }

    /// A rate of zero is the setting turned off, not a blink of no length --
    /// which would divide by nothing and flicker every frame.
    #[test]
    fn a_rate_of_zero_leaves_the_cursor_alone() {
        for elapsed in [0, 1, 999_999] {
            assert!(blink_is_on(elapsed, 0), "at {elapsed}");
        }
    }
}

#[cfg(test)]
mod text_blink_tests {
    use super::*;
    use unterm_engine::next_core::config::parse;
    use unterm_engine::{CellStyle, CursorSnapshot, StyledCell, StyledScreenLine};

    #[test]
    fn the_two_cadences_tick_independently() {
        // At 600ms a 500ms slow blink is off but a 250ms rapid one is back on.
        let phase = BlinkPhase::at(600, 500, 250);
        assert!(!phase.slow_on);
        assert!(phase.rapid_on);
        assert!(phase.conceals(Some(StyledBlink::Slow)));
        assert!(!phase.conceals(Some(StyledBlink::Rapid)));
        assert!(!phase.conceals(None));
    }

    /// Zero is the cadence turned off: its cells stay visible forever.
    #[test]
    fn a_rate_of_zero_never_conceals() {
        for elapsed in [0, 1, 250, 999_999] {
            let phase = BlinkPhase::at(elapsed, 0, 0);
            assert!(!phase.conceals(Some(StyledBlink::Slow)), "at {elapsed}");
            assert!(!phase.conceals(Some(StyledBlink::Rapid)), "at {elapsed}");
        }
    }

    /// The previous front end's defaults: 500ms slow, 250ms rapid.
    #[test]
    fn the_rates_default_and_read_from_the_config() {
        let empty = parse("").expect("empty config parses");
        assert_eq!(text_blink_rates(&empty), (500, 250));

        let set = parse("text_blink_rate = 0\ntext_blink_rate_rapid = 100").expect("config parses");
        assert_eq!(text_blink_rates(&set), (0, 100));
    }

    fn snapshot_with(blink: Option<StyledBlink>) -> StyledScreenSnapshot {
        let mut style = CellStyle::default();
        style.blink = blink;
        StyledScreenSnapshot {
            lines: vec![StyledScreenLine {
                row: 0,
                wrapped: false,
                cells: "ab"
                    .chars()
                    .map(|ch| StyledCell {
                        ch,
                        style: style.clone(),
                        width: 1,
                    })
                    .collect(),
            }],
            cursor: CursorSnapshot {
                x: 0,
                y: 0,
                visible: false,
                shape: "Default".to_string(),
            },
            cols: 2,
            rows: 1,
            scrollback_rows: 0,
            revision: 1,
            dirty_rows: None,
            mouse: Default::default(),
            bells: 0,
            notifications: 0,
            last_notification: None,
            focus_reporting: false,
            clipboard_request: None,
        }
    }

    #[test]
    fn only_the_scanner_reports_what_actually_blinks() {
        assert_eq!(blinking_cells(&snapshot_with(None)), (false, false));
        assert_eq!(
            blinking_cells(&snapshot_with(Some(StyledBlink::Slow))),
            (true, false)
        );
        assert_eq!(
            blinking_cells(&snapshot_with(Some(StyledBlink::Rapid))),
            (false, true)
        );
    }

    /// The invisible half withholds the glyphs and keeps the cell -- exactly
    /// what `hidden` does, because it is drawn through the same path.
    #[test]
    fn the_invisible_half_conceals_the_glyphs_and_not_the_cells() {
        let Ok(mut font) = TerminalFont::open(16) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(256, 256);
        let colors = FrameColors {
            foreground: [1.0; 4],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &unterm_render::quads::DEFAULT_PALETTE,
        };
        let snapshot = snapshot_with(Some(StyledBlink::Slow));

        let count_glyphs = |phase: BlinkPhase, font: &mut TerminalFont, atlas: &mut GlyphAtlas| {
            let mut quads = FrameQuads::default();
            append_pane(
                &snapshot,
                font,
                atlas,
                colors,
                (0.0, 0.0),
                true,
                CursorStyle::default(),
                phase,
                &mut quads,
            );
            quads.glyphs.len()
        };

        let shown = count_glyphs(BlinkPhase::STEADY, &mut font, &mut atlas);
        let concealed = count_glyphs(
            BlinkPhase {
                slow_on: false,
                rapid_on: true,
            },
            &mut font,
            &mut atlas,
        );
        assert!(shown > 0, "the visible half draws the text");
        assert_eq!(concealed, 0, "the invisible half draws none of it");
    }
}

#[cfg(test)]
mod visual_bell_tests {
    use super::*;
    use unterm_engine::next_core::config::parse;

    /// The previous front end's defaults: both durations zero -- no flash.
    #[test]
    fn the_default_bell_never_shows() {
        let bell = VisualBell::from_config(&parse("").expect("empty config parses"));
        assert!(bell.disabled());
        assert_eq!(bell.intensity_at(0), None);
        assert_eq!(bell.target, BellTarget::BackgroundColor);
        assert_eq!(bell.fade_in, Easing::Ease);
        assert_eq!(bell.fade_out, Easing::Ease);
    }

    #[test]
    fn the_flash_rises_falls_and_ends() {
        let config = parse(
            "[visual_bell]\n\
             fade_in_duration_ms = 100\n\
             fade_out_duration_ms = 100\n\
             fade_in_function = \"Linear\"\n\
             fade_out_function = \"Linear\"\n\
             target = \"CursorColor\"",
        )
        .expect("config parses");
        let bell = VisualBell::from_config(&config);
        assert!(!bell.disabled());
        assert_eq!(bell.target, BellTarget::CursorColor);
        assert_eq!(bell.intensity_at(0), Some(0.0));
        assert_eq!(bell.intensity_at(50), Some(0.5));
        assert_eq!(bell.intensity_at(150), Some(0.5));
        assert_eq!(bell.intensity_at(200), None);
        assert_eq!(bell.intensity_at(999_999), None);
    }

    /// Only a fade-out, which is what the old hardcoded flash was: full
    /// brightness at the bell, gone when the fade completes.
    #[test]
    fn a_fade_out_alone_starts_at_full() {
        let config =
            parse("[visual_bell]\nfade_out_duration_ms = 120\nfade_out_function = \"Linear\"")
                .expect("config parses");
        let bell = VisualBell::from_config(&config);
        assert_eq!(bell.intensity_at(0), Some(1.0));
        assert_eq!(bell.intensity_at(60), Some(0.5));
        assert_eq!(bell.intensity_at(120), None);
    }

    /// An unknown curve is `Ease`, the old default, not a refusal to flash.
    #[test]
    fn easing_names_parse_and_typos_fall_back() {
        assert_eq!(Easing::parse("Linear"), Easing::Linear);
        assert_eq!(Easing::parse("easeinout"), Easing::EaseInOut);
        assert_eq!(Easing::parse("Constant"), Easing::Constant);
        assert_eq!(Easing::parse("wobbly"), Easing::Ease);
    }
}

/// Draw chrome text at the font's own advances, and say how wide it came out.
///
/// Not on the terminal's grid. Chrome is drawn in a proportional face, and
/// placing each character on a fixed cell spreads a word out into `u n t e r m`
/// -- which is exactly what the sidebar looked like the first time it was drawn
/// this way. Every glyph goes where its own advance puts it.
///
/// `top` is the top of the row; the baseline is taken from the font, so a row
/// of text and a row of icons sit on the same line.
pub fn append_chrome_text(
    text: &str,
    font: &mut TerminalFont,
    atlas: &mut GlyphAtlas,
    color: [f32; 4],
    origin: (f32, f32),
    quads: &mut FrameQuads,
) -> f32 {
    for ch in text.chars() {
        ensure_glyph(font, atlas, ch);
    }
    let baseline = font.metrics().baseline;
    let mut pen = origin.0;
    for ch in text.chars() {
        if ch == ' ' {
            pen += space_advance(font);
            continue;
        }
        let key = glyph_key(font, ch);
        let Some(slot) = atlas.get(key) else {
            pen += space_advance(font);
            continue;
        };
        if slot.width > 0 && slot.height > 0 {
            quads.glyphs.push(unterm_render::quads::glyph_quad(
                slot,
                pen,
                origin.1 + baseline,
                color,
                atlas,
            ));
        }
        pen += slot.advance_x as f32;
    }
    pen - origin.0
}

/// How wide chrome text will be, without drawing it.
///
/// Measured the same way it is drawn, so a label that is measured as fitting
/// fits. Anything measured on the terminal's grid instead is wrong for a
/// proportional face, in whichever direction the face happens to differ.
pub fn chrome_text_width(text: &str, font: &mut TerminalFont, atlas: &mut GlyphAtlas) -> f32 {
    for ch in text.chars() {
        ensure_glyph(font, atlas, ch);
    }
    let mut width = 0.0;
    for ch in text.chars() {
        if ch == ' ' {
            width += space_advance(font);
            continue;
        }
        width += match atlas.get(glyph_key(font, ch)) {
            Some(slot) => slot.advance_x as f32,
            None => space_advance(font),
        };
    }
    width
}

/// A space's advance in this face, falling back to a third of the size for a
/// face that reports none.
fn space_advance(font: &mut TerminalFont) -> f32 {
    match font.stack_mut().rasterize(' ') {
        Some((_, glyph)) if glyph.advance_x > 0 => glyph.advance_x as f32,
        _ => font.pixel_size() as f32 * 0.32,
    }
}

#[cfg(test)]
mod chrome_text_tests {
    use super::*;

    /// Chrome text is placed at the face's own advances. On a fixed cell grid a
    /// proportional word comes out as `u n t e r m`, which is what the sidebar
    /// looked like the first time it was drawn that way.
    #[test]
    fn a_narrow_letter_takes_less_room_than_a_wide_one() {
        let Ok(mut font) = crate::chrome_font::open(&[], 1.0) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(512, 512);
        let narrow = chrome_text_width("iiii", &mut font, &mut atlas);
        let wide = chrome_text_width("WWWW", &mut font, &mut atlas);
        assert!(
            wide > narrow,
            "four W came out no wider than four i: {wide} vs {narrow}"
        );
    }

    /// And what is measured is what is drawn, or a label that measures as
    /// fitting runs off the end of its row.
    #[test]
    fn what_is_measured_is_what_is_drawn() {
        let Ok(mut font) = crate::chrome_font::open(&[], 1.0) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(512, 512);
        let mut quads = FrameQuads::default();
        let measured = chrome_text_width("unterm", &mut font, &mut atlas);
        let drawn = append_chrome_text(
            "unterm",
            &mut font,
            &mut atlas,
            [1.0; 4],
            (0.0, 0.0),
            &mut quads,
        );
        assert!((measured - drawn).abs() < 0.5, "{measured} then {drawn}");
        assert!(!quads.glyphs.is_empty(), "nothing was drawn");
    }

    /// Every glyph lands inside the run it was asked for, left to right.
    #[test]
    fn glyphs_run_left_to_right_from_the_origin() {
        let Ok(mut font) = crate::chrome_font::open(&[], 1.0) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(512, 512);
        let mut quads = FrameQuads::default();
        let width = append_chrome_text(
            "abcdef",
            &mut font,
            &mut atlas,
            [1.0; 4],
            (100.0, 50.0),
            &mut quads,
        );
        let mut previous = 0.0f32;
        for glyph in &quads.glyphs {
            assert!(
                glyph.quad.left >= 99.0,
                "{:?} is before the origin",
                glyph.quad
            );
            assert!(
                glyph.quad.left <= 100.0 + width + 2.0,
                "{:?} is past the run",
                glyph.quad
            );
            assert!(
                glyph.quad.left >= previous - 1.0,
                "the glyphs went backwards"
            );
            previous = glyph.quad.left;
        }
    }

    /// A space moves the pen without drawing anything.
    #[test]
    fn a_space_advances_without_drawing() {
        let Ok(mut font) = crate::chrome_font::open(&[], 1.0) else {
            return;
        };
        let mut atlas = GlyphAtlas::new(512, 512);
        let mut quads = FrameQuads::default();
        let width =
            append_chrome_text(" ", &mut font, &mut atlas, [1.0; 4], (0.0, 0.0), &mut quads);
        assert!(width > 0.0, "a space took no room");
        assert!(quads.glyphs.is_empty(), "a space drew something");
    }
}
