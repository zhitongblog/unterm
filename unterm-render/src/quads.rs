//! From a styled screen to the quads a GPU draws.
//!
//! This is the layer where rendering bugs are both easiest to introduce and
//! hardest to read: everything compiles, the frame is submitted, and what
//! appears is subtly or completely wrong. It is deliberately pure -- styled
//! cells and atlas slots in, vertices out -- so it can be checked by assertion
//! rather than by looking at a window.
//!
//! One trap in particular is pinned by a test here: texture coordinates are
//! normalized against the atlas size, and the atlas grows. Coordinates
//! computed before a grow and reused after it point at the wrong pixels, which
//! shows up as glyphs turning into slices of their neighbours.

use crate::atlas::{GlyphAtlas, GlyphSlot};
use unterm_engine::{CellStyle, StyledCell, StyledColor};

/// A solid rectangle in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub color: [f32; 4],
}

/// A glyph, with where to sample it from the atlas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphQuad {
    pub quad: Quad,
    /// Texture coordinates in 0..1, normalized against the atlas *as it is
    /// now*. Recomputed per frame rather than cached, because the atlas grows.
    pub tex_left: f32,
    pub tex_top: f32,
    pub tex_right: f32,
    pub tex_bottom: f32,
}

/// A picture a program put on the screen (kitty graphics, iTerm2, sixel),
/// drawn from its own texture.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineImageQuad {
    /// Where it lands, already clipped to its pane. The colour's alpha is
    /// how much of it shows -- an inactive pane dims its pictures with it.
    pub glyph: GlyphQuad,
    /// The picture's name, the key its texture is cached under.
    pub image: String,
    /// The pane it belongs to, so a missing texture can be fetched from it.
    pub pane: usize,
}

/// How big a cell is and where its baseline sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the top of the cell down to the baseline.
    pub baseline: f32,
}

/// The colours a frame falls back to when a cell does not name its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameColors {
    pub foreground: [f32; 4],
    pub background: [f32; 4],
    /// The sixteen colours programs actually ask for, and the rest of the 256
    /// derived from them.
    ///
    /// Carried here rather than looked up from a global, so a theme can change
    /// what `ls --color` looks like. A borrow because themes are `'static` and
    /// this struct is copied per row -- sixteen colours by value would be a
    /// quarter of a kilobyte moved for every line drawn.
    pub palette: &'static Palette,
}

/// A terminal's sixteen colours.
pub type Palette = [[f32; 4]; 16];

/// The colours a terminal has when nothing has said otherwise.
///
/// xterm's, as the kernel reports them -- so a front end that never sets a
/// theme still draws what every other terminal draws.
pub static DEFAULT_PALETTE: std::sync::LazyLock<Palette> = std::sync::LazyLock::new(|| {
    std::array::from_fn(|index| {
        let rgb = unterm_engine::next_core::color::palette_rgb(index as u8);
        [
            rgb.red as f32 / 255.0,
            rgb.green as f32 / 255.0,
            rgb.blue as f32 / 255.0,
            1.0,
        ]
    })
});

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameQuads {
    /// A picture behind everything, if one is configured.
    ///
    /// Its own list rather than a flag on a background quad, because it is the
    /// one thing drawn from a different texture: the atlas holds coverage for
    /// glyphs, and a photograph is not coverage.
    pub image: Option<GlyphQuad>,
    pub backgrounds: Vec<Quad>,
    /// Pictures programs put on the screen: over the cell backgrounds, under
    /// the text, the way kitty stacks them by default.
    pub inline_images: Vec<InlineImageQuad>,
    pub glyphs: Vec<GlyphQuad>,
    /// Drawn after everything above, so a panel can cover what is behind it.
    ///
    /// Without this an overlay's background goes down with every other
    /// background -- before any text -- and the terminal's own characters show
    /// straight through the panel that is supposed to be on top of them.
    pub overlay_backgrounds: Vec<Quad>,
    pub overlay_glyphs: Vec<GlyphQuad>,
    /// Drawn after the overlays: a true modal -- the command palette, a
    /// tooltip -- covers even the panels. Two tiers because a tier is
    /// backgrounds-then-glyphs: a modal in the overlay tier has the dock's
    /// text bleeding through its card, since every glyph in a tier lands on
    /// top of every background in it.
    pub modal_backgrounds: Vec<Quad>,
    pub modal_glyphs: Vec<GlyphQuad>,
}

/// How much had been drawn at a moment, so what came after can be raised.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
    backgrounds: usize,
    glyphs: usize,
}

impl FrameQuads {
    /// Remember how much has been drawn so far.
    pub fn mark(&self) -> Mark {
        Mark {
            backgrounds: self.backgrounds.len(),
            glyphs: self.glyphs.len(),
        }
    }

    /// Move everything drawn since `mark` in front of the terminal.
    ///
    /// Overlays are built with the same helpers as everything else and simply
    /// drawn last; this is what makes "last" mean "on top" rather than "in the
    /// same layer, after".
    pub fn raise_since(&mut self, mark: Mark) {
        self.overlay_backgrounds
            .extend(self.backgrounds.drain(mark.backgrounds..));
        self.overlay_glyphs.extend(self.glyphs.drain(mark.glyphs..));
    }

    /// Move everything drawn since `mark` in front of the overlays too.
    pub fn raise_since_modal(&mut self, mark: Mark) {
        self.modal_backgrounds
            .extend(self.backgrounds.drain(mark.backgrounds..));
        self.modal_glyphs.extend(self.glyphs.drain(mark.glyphs..));
    }
}

/// The foreground and background a cell is drawn in.
///
/// Public because a shaped row resolves the colour for a glyph that may cover
/// several cells, and has to reach the same answer this does for one.
pub fn resolve_style(style: Option<&CellStyle>, colors: FrameColors) -> ([f32; 4], [f32; 4]) {
    match style {
        Some(style) => resolve(style, colors),
        None => (colors.foreground, colors.background),
    }
}

/// Resolve a cell colour, honouring inverse.
///
/// Inverse swaps foreground and background rather than picking a third colour,
/// which is what every terminal does and what a selection highlight relies on.
fn resolve(style: &CellStyle, colors: FrameColors) -> ([f32; 4], [f32; 4]) {
    let foreground = style
        .fg
        .map(|color| to_rgba(color, colors.palette))
        .unwrap_or(colors.foreground);
    let background = style
        .bg
        .map(|color| to_rgba(color, colors.palette))
        .unwrap_or(colors.background);

    let (foreground, background) = if style.inverse {
        (background, foreground)
    } else {
        (foreground, background)
    };
    // SGR 8: the text is there to select and copy, not to see.
    if style.hidden {
        return (background, background);
    }
    // SGR 2. Parsed since the first kernel and never drawn, so everything a
    // program meant to recede -- Claude Code's suggested prompt and
    // placeholders, a shell's autosuggestion, a dimmed help line -- came out
    // exactly as bright as what the user typed next to it. Halfway to the
    // background, as xterm and most terminals render it; bold and faint
    // together stay faint, since the brightness is the part being asked for.
    if style.faint {
        return (faint_of(foreground, background), background);
    }
    (foreground, background)
}

/// Faint text: the colour halfway to the cell's background.
fn faint_of(foreground: [f32; 4], background: [f32; 4]) -> [f32; 4] {
    const SHARE: f32 = 0.5;
    [
        background[0] + (foreground[0] - background[0]) * SHARE,
        background[1] + (foreground[1] - background[1]) * SHARE,
        background[2] + (foreground[2] - background[2]) * SHARE,
        foreground[3],
    ]
}

/// A cell's colour as the GPU wants it.
///
/// Palette indices are resolved through the kernel's table rather than
/// dropped. They used to fall back to the frame colour, which meant every
/// colour a program actually sends -- `ls --color`, git's diffs, any prompt --
/// came out the same shade as ordinary text. Only truecolor worked, and
/// truecolor is the one programs use least.
fn to_rgba(color: StyledColor, palette: &Palette) -> [f32; 4] {
    match color {
        StyledColor::Rgb(red, green, blue) => [
            red as f32 / 255.0,
            green as f32 / 255.0,
            blue as f32 / 255.0,
            1.0,
        ],
        // The first sixteen come from the theme; the rest of the 256 are the
        // cube and the greys, which no theme redefines.
        StyledColor::Palette(index) if (index as usize) < palette.len() => palette[index as usize],
        StyledColor::Palette(index) => {
            let rgb = unterm_engine::next_core::color::palette_rgb(index);
            [
                rgb.red as f32 / 255.0,
                rgb.green as f32 / 255.0,
                rgb.blue as f32 / 255.0,
                1.0,
            ]
        }
    }
}

/// How a glyph is set in the columns it belongs to.
///
/// One answer for both drawing paths -- the shaper's and the per-character
/// one -- so a character lands in the same place whichever drew it. They
/// used to disagree: IME preedit drew a hanzi flush left in its two cells,
/// and the same hanzi committed a moment later sat centred, so the text
/// jumped by a fraction of a cell on Enter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlyphFit {
    /// At the pen, by the glyph's own bearing: the primary face, whose
    /// advance *is* the cell.
    #[default]
    Pen,
    /// Centred across its columns, and nudged down (or up) to stay inside its
    /// row when it fits: a fallback face, designed around an em of its own.
    Centre,
}

/// Where the pen goes for a glyph set in `span_width` pixels starting at
/// `cell_left`.
///
/// For `Centre`, the ink is centred: the air the glyph leaves is split
/// either side rather than piled up on the right. A glyph wider than its
/// span starts at the span's left edge, which is where the clip keeps it.
pub fn fit_pen(slot: GlyphSlot, fit: GlyphFit, cell_left: f32, span_width: f32) -> f32 {
    match fit {
        GlyphFit::Pen => cell_left,
        GlyphFit::Centre => {
            let air = (span_width - slot.width as f32).max(0.0);
            cell_left + (air / 2.0).floor() - slot.bearing_x as f32
        }
    }
}

/// The baseline a glyph is drawn on, in a row starting at `row_top`.
///
/// The row's own baseline, except that a `Centre`d glyph whose ink would
/// poke out of the row -- a tall emoji above, a symbol below -- is moved
/// back inside it when it is short enough to fit. Anything still outside is
/// the clip's to cut.
pub fn fit_baseline(slot: GlyphSlot, fit: GlyphFit, row_top: f32, metrics: CellMetrics) -> f32 {
    let baseline = row_top + metrics.baseline;
    if fit != GlyphFit::Centre || slot.height as f32 > metrics.height {
        return baseline;
    }
    let top = baseline - slot.bearing_y as f32;
    let bottom = top + slot.height as f32;
    if top < row_top {
        baseline + (row_top - top)
    } else if bottom > row_top + metrics.height {
        baseline - (bottom - (row_top + metrics.height))
    } else {
        baseline
    }
}

/// A rectangle in pixels a glyph may not draw outside of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl ClipRect {
    /// The columns `[column, column + span)` of a row, in pixels.
    pub fn cells(left_origin: f32, top: f32, column: usize, span: usize, metrics: CellMetrics) -> Self {
        Self {
            left: left_origin + column as f32 * metrics.width,
            top,
            right: left_origin + (column + span.max(1)) as f32 * metrics.width,
            bottom: top + metrics.height,
        }
    }
}

/// Cut a glyph down to `clip`, moving its texture coordinates in step so
/// what is left samples the same pixels it did before. `None` when nothing
/// is left.
///
/// Every terminal glyph is drawn through this, so ink can never land on a
/// neighbouring cell -- a Nerd Font icon wider than its column, a fallback
/// glyph that came out a pixel too big, a tall glyph reaching into the row
/// above -- and then be painted over (or not) depending on draw order.
pub fn clip_glyph(glyph: GlyphQuad, clip: ClipRect) -> Option<GlyphQuad> {
    let quad = glyph.quad;
    let (left, top) = (quad.left.max(clip.left), quad.top.max(clip.top));
    let right = (quad.left + quad.width).min(clip.right);
    let bottom = (quad.top + quad.height).min(clip.bottom);
    if right <= left || bottom <= top {
        return None;
    }
    if (left, top, right, bottom) == (quad.left, quad.top, quad.left + quad.width, quad.top + quad.height) {
        return Some(glyph);
    }
    let u = |x: f32| glyph.tex_left + (x - quad.left) / quad.width * (glyph.tex_right - glyph.tex_left);
    let v = |y: f32| glyph.tex_top + (y - quad.top) / quad.height * (glyph.tex_bottom - glyph.tex_top);
    Some(GlyphQuad {
        quad: Quad {
            left,
            top,
            width: right - left,
            height: bottom - top,
            color: quad.color,
        },
        tex_left: u(left),
        tex_top: v(top),
        tex_right: u(right),
        tex_bottom: v(bottom),
    })
}

/// What a row's per-character glyphs are drawn with, beyond the cells.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowOptions {
    /// The column under a block cursor and the colour its glyph takes, so
    /// the character stays readable on the block. Decided here, for the
    /// glyph drawn from that cell, rather than afterwards by guessing which
    /// glyphs a pixel position belongs to.
    pub inverted: Option<(usize, [f32; 4])>,
    /// Whether to cut each glyph to its own cells. The terminal grid does;
    /// the chrome's proportional labels draw at the face's own advances and
    /// do not.
    pub clip: bool,
}

/// Build one row's quads.
///
/// `slot_for` is asked for a glyph's place in the atlas; returning None skips
/// the glyph and keeps the row's other cells, because a gap is easier to
/// diagnose than a blank line.
pub fn build_row(
    cells: &[StyledCell],
    left_origin: f32,
    top: f32,
    metrics: CellMetrics,
    colors: FrameColors,
    atlas: &GlyphAtlas,
    mut slot_for: impl FnMut(char) -> Option<GlyphSlot>,
    out: &mut FrameQuads,
    already_drawn: &std::collections::HashSet<usize>,
) {
    build_row_with(
        cells,
        left_origin,
        top,
        metrics,
        colors,
        atlas,
        |cell| slot_for(cell.ch).map(|slot| (slot, GlyphFit::Pen)),
        out,
        already_drawn,
        RowOptions {
            inverted: None,
            clip: true,
        },
    );
}

/// As `build_row`, with each glyph's fit chosen by the caller and the
/// cursor's cell inverted.
///
/// `slot_for` gets the whole cell, so the glyph can be fitted to the columns
/// the cell actually occupies rather than to a width worked out again from
/// the character.
#[allow(clippy::too_many_arguments)]
pub fn build_row_with(
    cells: &[StyledCell],
    left_origin: f32,
    top: f32,
    metrics: CellMetrics,
    colors: FrameColors,
    atlas: &GlyphAtlas,
    mut slot_for: impl FnMut(&StyledCell) -> Option<(GlyphSlot, GlyphFit)>,
    out: &mut FrameQuads,
    already_drawn: &std::collections::HashSet<usize>,
    options: RowOptions,
) {
    let mut column = 0usize;

    for cell in cells {
        // The spacer stored after a wide character: its columns belong to
        // the character before it. Counting it as one of its own is how a
        // row grew a phantom blank cell after every hanzi.
        if cell.width == 0 {
            continue;
        }
        // A wide character occupies two columns, and its background has to
        // cover both or the second shows through.
        let span = cell.width as f32;
        let left = left_origin + column as f32 * metrics.width;
        let (foreground, background) = resolve(&cell.style, colors);

        if background != colors.background {
            // Only where it differs: filling every cell with the frame colour
            // is the whole screen drawn twice, every frame.
            out.backgrounds.push(Quad {
                left,
                top,
                width: metrics.width * span,
                height: metrics.height,
                color: background,
            });
        }

        // Lines, blocks and separators we draw ourselves, over the cell's
        // background and instead of the font's glyph. A font's box-drawing
        // characters are laid out for its own metrics, so at our cell size
        // they leave hairline gaps where a table's lines should meet; drawn
        // to the cell, they join exactly.
        if let Some(drawn) = crate::box_glyphs::quads_for(cell.ch, left, top, metrics, foreground) {
            out.backgrounds.extend(drawn);
            column += cell.width;
            continue;
        }

        // A column the shaper already drew: drawing it again would put a
        // second glyph underneath the ligature that replaced it.
        if !already_drawn.contains(&column)
            && !cell.style.hidden
            && cell.ch != ' '
            && cell.ch != '\0'
        {
            if let Some((slot, fit)) = slot_for(cell) {
                if slot.width > 0 && slot.height > 0 {
                    // The cursor's own cell, known here by the cell the glyph
                    // was drawn from -- never by where its ink happens to land.
                    let color = match options.inverted {
                        Some((inverted, color)) if inverted == column => color,
                        _ => foreground,
                    };
                    let span_width = metrics.width * span;
                    let glyph = glyph_quad(
                        slot,
                        fit_pen(slot, fit, left, span_width),
                        fit_baseline(slot, fit, top, metrics),
                        color,
                        atlas,
                    );
                    let glyph = if options.clip {
                        clip_glyph(
                            glyph,
                            ClipRect::cells(left_origin, top, column, cell.width, metrics),
                        )
                    } else {
                        Some(glyph)
                    };
                    out.glyphs.extend(glyph);
                }
            }
        }

        // Underlines and the rest, over the background and under nothing --
        // a line the program asked for is information, and losing it silently
        // is the same as not parsing it.
        out.backgrounds.extend(crate::decorations::quads_for(
            &cell.style,
            left,
            top,
            metrics.width * span,
            metrics,
            foreground,
        ));

        column += cell.width;
    }
}

/// Place one glyph relative to a pen sitting on the baseline.
///
/// On whole pixels: the atlas is sampled linearly, so a bitmap placed at a
/// fractional position is resampled -- blurred by an amount that differs
/// column to column, which reads as uneven spacing.
pub fn glyph_quad(
    slot: GlyphSlot,
    pen_x: f32,
    baseline_y: f32,
    color: [f32; 4],
    atlas: &GlyphAtlas,
) -> GlyphQuad {
    let atlas_width = atlas.width().max(1) as f32;
    let atlas_height = atlas.height().max(1) as f32;

    GlyphQuad {
        quad: Quad {
            left: (pen_x + slot.bearing_x as f32).round(),
            // Bearing is measured up from the baseline; the screen counts down.
            top: (baseline_y - slot.bearing_y as f32).round(),
            width: slot.width as f32,
            height: slot.height as f32,
            color,
        },
        tex_left: slot.x as f32 / atlas_width,
        tex_top: slot.y as f32 / atlas_height,
        tex_right: (slot.x + slot.width) as f32 / atlas_width,
        tex_bottom: (slot.y + slot.height) as f32 / atlas_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::GlyphKey;
    use unterm_engine::next_core::font_raster::RasterizedGlyph;

    fn metrics() -> CellMetrics {
        CellMetrics {
            width: 10.0,
            height: 20.0,
            baseline: 16.0,
        }
    }

    fn colors() -> FrameColors {
        FrameColors {
            foreground: [1.0, 1.0, 1.0, 1.0],
            background: [0.0, 0.0, 0.0, 1.0],
            palette: &crate::quads::DEFAULT_PALETTE,
        }
    }

    fn cell(ch: char, style: CellStyle) -> StyledCell {
        StyledCell {
            ch,
            style,
            width: 1,
            combining: None,
        }
    }

    fn atlas_with_glyph() -> (GlyphAtlas, GlyphSlot) {
        let mut atlas = GlyphAtlas::new(64, 64);
        let slot = atlas.insert(
            GlyphKey {
                stack: 0,
                face: 0,
                glyph_index: 1,
                pixel_size: 16,
            },
            &RasterizedGlyph {
                coverage: vec![255; 6 * 8],
                width: 6,
                height: 8,
                bearing_x: 1,
                bearing_y: 7,
                advance_x: 10,
            },
        );
        (atlas, slot)
    }

    #[test]
    fn a_glyph_sits_on_the_baseline_by_its_bearings() {
        let (atlas, slot) = atlas_with_glyph();

        let quad = glyph_quad(slot, 100.0, 50.0, [1.0; 4], &atlas);

        assert_eq!(quad.quad.left, 101.0);
        // Seven pixels above the baseline, because that is what bearing means.
        assert_eq!(quad.quad.top, 43.0);
        assert_eq!((quad.quad.width, quad.quad.height), (6.0, 8.0));
    }

    #[test]
    fn texture_coordinates_follow_the_atlas_when_it_grows() {
        let (mut atlas, slot) = atlas_with_glyph();
        let before = glyph_quad(slot, 0.0, 0.0, [1.0; 4], &atlas);

        // Force a grow, which doubles the height and halves every normalized
        // vertical coordinate.
        while atlas.height() == 64 {
            atlas.insert(
                GlyphKey {
                    stack: 0,
                    face: 0,
                    glyph_index: atlas.len() as u32 + 100,
                    pixel_size: 16,
                },
                &RasterizedGlyph {
                    coverage: vec![255; 60 * 60],
                    width: 60,
                    height: 60,
                    bearing_x: 0,
                    bearing_y: 0,
                    advance_x: 60,
                },
            );
        }
        let after = glyph_quad(slot, 0.0, 0.0, [1.0; 4], &atlas);

        // Reusing the old coordinates would sample the wrong pixels, and the
        // glyph would come out as a slice of its neighbour.
        assert_ne!(before.tex_bottom, after.tex_bottom);
        assert!(after.tex_bottom < before.tex_bottom);
    }

    #[test]
    fn a_cell_with_the_frame_background_draws_no_background_quad() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();

        build_row(
            &[cell('a', CellStyle::default())],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some(slot),
            &mut out,
            &Default::default(),
        );

        // Filling every cell with the frame colour is the whole screen drawn
        // twice, every frame.
        assert!(out.backgrounds.is_empty());
        assert_eq!(out.glyphs.len(), 1);
    }

    #[test]
    fn a_coloured_cell_draws_its_background_over_exactly_its_cell() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();
        let style = CellStyle {
            bg: Some(StyledColor::Rgb(255, 0, 0)),
            ..CellStyle::default()
        };

        build_row(
            &[cell('a', style)],
            0.0,
            40.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some(slot),
            &mut out,
            &Default::default(),
        );

        let quad = out.backgrounds[0];
        assert_eq!((quad.left, quad.top), (0.0, 40.0));
        assert_eq!((quad.width, quad.height), (10.0, 20.0));
        assert_eq!(quad.color, [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_wide_cell_covers_both_its_columns() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();
        let style = CellStyle {
            bg: Some(StyledColor::Rgb(0, 0, 255)),
            ..CellStyle::default()
        };
        let wide = StyledCell {
            ch: '漢',
            style,
            width: 2,
            combining: None,
        };

        build_row(
            &[wide],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some(slot),
            &mut out,
            &Default::default(),
        );

        // Covering only one column lets the frame background show through the
        // right half of the character.
        assert_eq!(out.backgrounds[0].width, 20.0);
    }

    #[test]
    fn cells_advance_by_their_own_width() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();
        let style = CellStyle {
            bg: Some(StyledColor::Rgb(1, 2, 3)),
            ..CellStyle::default()
        };
        let wide = StyledCell {
            ch: '漢',
            style: style.clone(),
            width: 2,
            combining: None,
        };

        build_row(
            &[wide, cell('a', style)],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some(slot),
            &mut out,
            &Default::default(),
        );

        // The narrow cell starts after both columns of the wide one.
        assert_eq!(out.backgrounds[1].left, 20.0);
    }

    /// SGR 2 recedes: Claude Code's suggested prompt must not look like what
    /// the user typed beside it. SGR 8 hides the glyph but keeps the cell.
    #[test]
    fn faint_text_is_dimmer_and_hidden_text_is_invisible() {
        let (atlas, slot) = atlas_with_glyph();
        let draw = |style: CellStyle| {
            let mut out = FrameQuads::default();
            build_row(
                &[cell('a', style)],
                0.0,
                0.0,
                metrics(),
                colors(),
                &atlas,
                |_| Some(slot),
                &mut out,
                &Default::default(),
            );
            out.glyphs.first().map(|glyph| glyph.quad.color)
        };
        let plain = draw(CellStyle::default()).expect("plain glyph");
        let faint = draw(CellStyle { faint: true, ..CellStyle::default() }).expect("faint glyph");
        let (fg, bg) = (colors().foreground, colors().background);
        for channel in 0..3 {
            let expected = bg[channel] + (fg[channel] - bg[channel]) * 0.5;
            assert!((faint[channel] - expected).abs() < 1e-6, "{faint:?}");
        }
        assert_ne!(faint, plain);
        if let Some(hidden) = draw(CellStyle { hidden: true, ..CellStyle::default() }) {
            assert_eq!(&hidden[..3], &bg[..3]);
        }
    }

    #[test]
    fn inverse_swaps_the_two_colours() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();
        let style = CellStyle {
            inverse: true,
            ..CellStyle::default()
        };

        build_row(
            &[cell('a', style)],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some(slot),
            &mut out,
            &Default::default(),
        );

        // What a selection highlight relies on: the background becomes the
        // frame's foreground, not some third colour.
        assert_eq!(out.backgrounds[0].color, colors().foreground);
        assert_eq!(out.glyphs[0].quad.color, colors().background);
    }

    #[test]
    fn a_space_draws_no_glyph() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();

        build_row(
            &[cell(' ', CellStyle::default())],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some(slot),
            &mut out,
            &Default::default(),
        );

        assert!(out.glyphs.is_empty());
    }

    #[test]
    fn a_hidden_cell_keeps_its_background_and_loses_its_glyph() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();
        let style = CellStyle {
            hidden: true,
            bg: Some(StyledColor::Rgb(9, 9, 9)),
            ..CellStyle::default()
        };

        build_row(
            &[cell('a', style)],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some(slot),
            &mut out,
            &Default::default(),
        );

        // `hidden` conceals the character, not the cell -- a password prompt
        // still occupies its space.
        assert_eq!(out.backgrounds.len(), 1);
        assert!(out.glyphs.is_empty());
    }

    #[test]
    fn a_glyph_the_atlas_does_not_have_skips_only_that_cell() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();

        build_row(
            &[
                cell('a', CellStyle::default()),
                cell('b', CellStyle::default()),
            ],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |ch| if ch == 'a' { None } else { Some(slot) },
            &mut out,
            &Default::default(),
        );

        // A gap is easier to diagnose than a blank line.
        assert_eq!(out.glyphs.len(), 1);
    }

    /// A glyph on a fractional pen still lands on whole pixels.
    #[test]
    fn a_glyph_is_placed_on_whole_pixels() {
        let (atlas, slot) = atlas_with_glyph();
        let quad = glyph_quad(slot, 100.4, 50.6, [1.0; 4], &atlas);
        assert_eq!((quad.quad.left, quad.quad.top), (101.0, 44.0));
    }

    /// Clipping cuts the rectangle and moves the texture coordinates with
    /// it, so what is left samples the same pixels it did.
    #[test]
    fn a_clipped_glyph_keeps_sampling_the_same_pixels() {
        let (atlas, slot) = atlas_with_glyph();
        // 6x8 at (1, 8)..(7, 16).
        let glyph = glyph_quad(slot, 0.0, 15.0, [1.0; 4], &atlas);
        let clip = ClipRect {
            left: 0.0,
            top: 10.0,
            right: 4.0,
            bottom: 20.0,
        };
        let cut = clip_glyph(glyph, clip).expect("part of it is inside");
        assert_eq!((cut.quad.left, cut.quad.top), (1.0, 10.0));
        assert_eq!((cut.quad.width, cut.quad.height), (3.0, 6.0));
        let u_per_px = (glyph.tex_right - glyph.tex_left) / 6.0;
        let v_per_px = (glyph.tex_bottom - glyph.tex_top) / 8.0;
        assert!((cut.tex_left - glyph.tex_left).abs() < 1e-6);
        assert!((cut.tex_right - (glyph.tex_left + 3.0 * u_per_px)).abs() < 1e-6);
        assert!((cut.tex_top - (glyph.tex_top + 2.0 * v_per_px)).abs() < 1e-6);
        assert!((cut.tex_bottom - glyph.tex_bottom).abs() < 1e-6);

        // Wholly outside: nothing.
        let away = ClipRect {
            left: 50.0,
            top: 0.0,
            right: 60.0,
            bottom: 20.0,
        };
        assert!(clip_glyph(glyph, away).is_none());
        // Wholly inside: untouched.
        let all = ClipRect {
            left: 0.0,
            top: 0.0,
            right: 100.0,
            bottom: 100.0,
        };
        assert_eq!(clip_glyph(glyph, all), Some(glyph));
    }

    /// An icon wider than its one column is cut at the column's edge rather
    /// than drawn over the character beside it.
    #[test]
    fn a_glyph_wider_than_its_cell_is_cut_at_the_cell() {
        let atlas = GlyphAtlas::new(64, 64);
        let wide = GlyphSlot {
            x: 0,
            y: 0,
            width: 16,
            height: 12,
            bearing_x: 0,
            bearing_y: 12,
            advance_x: 16,
        };
        let mut out = FrameQuads::default();
        build_row(
            &[cell('a', CellStyle::default()), cell('x', CellStyle::default())],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some(wide),
            &mut out,
            &Default::default(),
        );
        assert_eq!(out.glyphs.len(), 2);
        for (column, glyph) in out.glyphs.iter().enumerate() {
            let left = column as f32 * metrics().width;
            assert!(glyph.quad.left >= left);
            assert!(glyph.quad.left + glyph.quad.width <= left + metrics().width, "{glyph:?}");
        }
    }

    /// A centred glyph splits its air either side, and a tall one is moved
    /// down into its row rather than poking into the one above.
    #[test]
    fn a_centred_glyph_sits_in_the_middle_of_its_span_and_inside_its_row() {
        let slot = GlyphSlot {
            x: 0,
            y: 0,
            width: 14,
            height: 18,
            bearing_x: 2,
            bearing_y: 19,
            advance_x: 18,
        };
        let pen = fit_pen(slot, GlyphFit::Centre, 100.0, 20.0);
        assert_eq!(pen + slot.bearing_x as f32, 103.0, "3px of air on each side");
        let baseline = fit_baseline(slot, GlyphFit::Centre, 40.0, metrics());
        assert_eq!(baseline - slot.bearing_y as f32, 40.0, "top of the ink at the top of the row");
        // The primary face's pen is the cell's, untouched.
        assert_eq!(fit_pen(slot, GlyphFit::Pen, 100.0, 20.0), 100.0);
        assert_eq!(fit_baseline(slot, GlyphFit::Pen, 40.0, metrics()), 56.0);
    }

    /// The cursor's colour goes to the glyph drawn from the cursor's cell,
    /// and to no other.
    #[test]
    fn the_inverted_column_takes_the_cursor_colour() {
        let (atlas, slot) = atlas_with_glyph();
        let mut out = FrameQuads::default();
        let marked = [0.25, 0.5, 0.75, 1.0];
        build_row_with(
            &[
                cell('a', CellStyle::default()),
                cell('b', CellStyle::default()),
                cell('c', CellStyle::default()),
            ],
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| Some((slot, GlyphFit::Pen)),
            &mut out,
            &Default::default(),
            RowOptions {
                inverted: Some((1, marked)),
                clip: true,
            },
        );
        let colours: Vec<[f32; 4]> = out.glyphs.iter().map(|glyph| glyph.quad.color).collect();
        assert_eq!(colours, vec![colors().foreground, marked, colors().foreground]);
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;

    fn colors() -> FrameColors {
        FrameColors {
            foreground: [0.9, 0.9, 0.9, 1.0],
            background: [0.1, 0.1, 0.1, 1.0],
            palette: &crate::quads::DEFAULT_PALETTE,
        }
    }

    fn style_with(fg: StyledColor) -> CellStyle {
        let mut style = CellStyle::default();
        style.fg = Some(fg);
        style
    }

    /// The colours programs actually send are the palette ones.
    ///
    /// These used to resolve to the frame's foreground, so `ls --color`, git
    /// diffs and every coloured prompt came out the same shade as ordinary
    /// text. Only truecolor worked, and truecolor is what programs use least.
    #[test]
    fn a_palette_colour_is_resolved_rather_than_dropped() {
        let (red, _) = resolve(&style_with(StyledColor::Palette(1)), colors());
        assert_ne!(red, colors().foreground, "palette red is not the default");
        assert!(red[0] > red[1] && red[0] > red[2], "and it is red: {red:?}");

        let (green, _) = resolve(&style_with(StyledColor::Palette(2)), colors());
        assert!(green[1] > green[0] && green[1] > green[2], "got {green:?}");

        let (blue, _) = resolve(&style_with(StyledColor::Palette(4)), colors());
        assert!(blue[2] > blue[0] && blue[2] > blue[1], "got {blue:?}");
    }

    #[test]
    fn the_bright_half_of_the_palette_differs_from_the_dim_half() {
        let (dim, _) = resolve(&style_with(StyledColor::Palette(1)), colors());
        let (bright, _) = resolve(&style_with(StyledColor::Palette(9)), colors());
        assert_ne!(dim, bright, "bright red and red are different colours");
    }

    #[test]
    fn the_256_colour_cube_resolves_too() {
        // 196 is the cube's pure red; a program using it means it.
        let (red, _) = resolve(&style_with(StyledColor::Palette(196)), colors());
        assert!((red[0] - 1.0).abs() < 0.01 && red[1] < 0.01 && red[2] < 0.01);
    }

    #[test]
    fn truecolor_still_arrives_exactly() {
        let (colour, _) = resolve(&style_with(StyledColor::Rgb(10, 20, 30)), colors());
        assert!((colour[0] - 10.0 / 255.0).abs() < 0.001);
        assert!((colour[1] - 20.0 / 255.0).abs() < 0.001);
        assert!((colour[2] - 30.0 / 255.0).abs() < 0.001);
    }

    #[test]
    fn a_cell_naming_no_colour_still_takes_the_frames() {
        let (fg, bg) = resolve(&CellStyle::default(), colors());
        assert_eq!(fg, colors().foreground);
        assert_eq!(bg, colors().background);
    }
}

#[cfg(test)]
mod drawn_glyph_tests {
    use super::*;

    fn metrics() -> CellMetrics {
        CellMetrics {
            width: 10.0,
            height: 20.0,
            baseline: 16.0,
        }
    }

    fn colors() -> FrameColors {
        FrameColors {
            foreground: [0.9, 0.9, 0.9, 1.0],
            background: [0.1, 0.1, 0.1, 1.0],
            palette: &crate::quads::DEFAULT_PALETTE,
        }
    }

    fn cell(ch: char) -> StyledCell {
        StyledCell {
            ch,
            width: 1,
            style: CellStyle::default(),
            combining: None,
        }
    }

    fn row(cells: &[StyledCell]) -> FrameQuads {
        let atlas = GlyphAtlas::new(64, 64);
        let mut out = FrameQuads::default();
        build_row(
            cells,
            0.0,
            0.0,
            metrics(),
            colors(),
            &atlas,
            |_| {
                Some(GlyphSlot {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 12,
                    bearing_x: 0,
                    bearing_y: 12,
                    advance_x: 10,
                })
            },
            &mut out,
            &Default::default(),
        );
        out
    }

    /// The font also has a `─`, and it is the wrong one: laid out for the
    /// font's own metrics, it stops short of the cell edge and a table comes
    /// back with hairline gaps at every join.
    #[test]
    fn a_box_character_is_drawn_rather_than_looked_up() {
        let quads = row(&[cell('\u{2500}')]);
        assert!(
            quads.glyphs.is_empty(),
            "the font's glyph was placed as well"
        );
        assert!(
            !quads.backgrounds.is_empty(),
            "and nothing was drawn instead"
        );
    }

    /// Drawn to the cell, so two side by side meet with nothing between them.
    #[test]
    fn two_horizontals_side_by_side_leave_no_gap() {
        let quads = row(&[cell('\u{2500}'), cell('\u{2500}')]);
        let mut spans: Vec<(f32, f32)> = quads
            .backgrounds
            .iter()
            .map(|quad| (quad.left, quad.left + quad.width))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(spans.len(), 2);
        assert!(
            spans[0].1 >= spans[1].0,
            "gap between {:?} and {:?}",
            spans[0],
            spans[1]
        );
    }

    #[test]
    fn an_ordinary_character_still_comes_from_the_font() {
        let quads = row(&[cell('a')]);
        assert_eq!(quads.glyphs.len(), 1);
    }

    /// The cell's own background still goes down; the drawing sits on top.
    #[test]
    fn a_coloured_cell_keeps_its_background_under_the_drawing() {
        let mut style = CellStyle::default();
        style.bg = Some(StyledColor::Rgb(0, 0, 255));
        let quads = row(&[StyledCell {
            ch: '\u{2500}',
            width: 1,
            style,
            combining: None,
        }]);
        let blue = quads
            .backgrounds
            .iter()
            .position(|quad| quad.color[2] > 0.9 && quad.color[0] < 0.1);
        let line = quads
            .backgrounds
            .iter()
            .position(|quad| quad.height < metrics().height);
        assert!(blue.is_some() && line.is_some(), "{:?}", quads.backgrounds);
        assert!(blue < line, "the background covered the drawing");
    }

    /// A drawn cell must not shift what follows it.
    #[test]
    fn the_column_after_a_drawn_cell_is_where_it_should_be() {
        let quads = row(&[cell('\u{2500}'), cell('a')]);
        assert_eq!(quads.glyphs.len(), 1);
        assert_eq!(quads.glyphs[0].quad.left, metrics().width);
    }
}

#[cfg(test)]
mod themed_palette_tests {
    use super::*;

    fn frame(palette: &'static Palette) -> FrameColors {
        FrameColors {
            foreground: [0.9, 0.9, 0.9, 1.0],
            background: [0.1, 0.1, 0.1, 1.0],
            palette,
        }
    }

    fn red(index: u8) -> CellStyle {
        let mut style = CellStyle::default();
        style.fg = Some(StyledColor::Palette(index));
        style
    }

    static ALL_GREEN: Palette = [[0.0, 1.0, 0.0, 1.0]; 16];

    /// A theme has to reach the colours programs actually ask for. Themed
    /// background and foreground while `ls --color` stays xterm's red is a
    /// theme that only half applied.
    #[test]
    fn a_theme_decides_what_the_first_sixteen_colours_are() {
        let (themed, _) = resolve(&red(1), frame(&ALL_GREEN));
        assert_eq!(themed, [0.0, 1.0, 0.0, 1.0]);

        let (standard, _) = resolve(&red(1), frame(&DEFAULT_PALETTE));
        assert_ne!(standard, themed, "the default is not the themed one");
        assert!(
            standard[0] > standard[1],
            "and it is still red: {standard:?}"
        );
    }

    /// Only the first sixteen. The rest of the 256 are the cube and the
    /// greys, which are defined by their index rather than chosen.
    #[test]
    fn the_colour_cube_is_not_themed() {
        let (cube, _) = resolve(&red(196), frame(&ALL_GREEN));
        assert!(
            (cube[0] - 1.0).abs() < 0.01 && cube[1] < 0.01,
            "196 is the cube's pure red whatever the theme: {cube:?}"
        );
    }

    /// And truecolor is never touched: a program naming an exact colour has
    /// said what it wants.
    #[test]
    fn truecolor_is_never_themed() {
        let mut style = CellStyle::default();
        style.fg = Some(StyledColor::Rgb(10, 20, 30));
        let (exact, _) = resolve(&style, frame(&ALL_GREEN));
        assert!((exact[0] - 10.0 / 255.0).abs() < 0.001, "{exact:?}");
    }

    /// The default palette is a real one, not an accident of initialisation.
    #[test]
    fn the_default_palette_is_the_one_every_terminal_draws() {
        assert_eq!(DEFAULT_PALETTE.len(), 16);
        let black = DEFAULT_PALETTE[0];
        let white = DEFAULT_PALETTE[15];
        assert!(black.iter().take(3).all(|c| *c < 0.2), "{black:?}");
        assert!(white.iter().take(3).all(|c| *c > 0.8), "{white:?}");
    }
}

#[cfg(test)]
mod layer_tests {
    use super::*;

    fn quad(top: f32) -> Quad {
        Quad {
            left: 0.0,
            top,
            width: 10.0,
            height: 10.0,
            color: [1.0; 4],
        }
    }

    /// A panel's background has to be drawn after the text it covers. In one
    /// layer it goes down with every other background -- before any glyph --
    /// and the terminal's characters show straight through it.
    #[test]
    fn what_is_raised_is_drawn_after_the_glyphs() {
        let mut frame = FrameQuads::default();
        frame.backgrounds.push(quad(0.0));
        let mark = frame.mark();
        frame.backgrounds.push(quad(1.0));

        frame.raise_since(mark);
        assert_eq!(frame.backgrounds.len(), 1, "the terminal's own stays put");
        assert_eq!(frame.overlay_backgrounds.len(), 1);
        assert_eq!(frame.overlay_backgrounds[0].top, 1.0);
    }

    /// Order within the overlay is kept: a panel's text goes on its own
    /// background, not under it.
    #[test]
    fn raising_keeps_the_order_things_were_drawn_in() {
        let mut frame = FrameQuads::default();
        let mark = frame.mark();
        for top in 0..3 {
            frame.backgrounds.push(quad(top as f32));
        }
        frame.raise_since(mark);
        let tops: Vec<f32> = frame.overlay_backgrounds.iter().map(|q| q.top).collect();
        assert_eq!(tops, vec![0.0, 1.0, 2.0]);
    }

    /// Raising nothing changes nothing.
    #[test]
    fn a_frame_with_no_overlay_is_left_alone() {
        let mut frame = FrameQuads::default();
        frame.backgrounds.push(quad(0.0));
        let mark = frame.mark();
        frame.raise_since(mark);
        assert_eq!(frame.backgrounds.len(), 1);
        assert!(frame.overlay_backgrounds.is_empty());
    }
}
