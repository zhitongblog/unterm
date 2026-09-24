//! Glyph rasterization for next-core, straight onto FreeType.
//!
//! The GPU path needs one thing from a font: given a character and a pixel
//! size, an 8-bit coverage bitmap plus the metrics to place it. `wezterm-font`
//! can do that, but it carries ten thousand lines of terminal-specific font
//! policy — fallback chains, config plumbing, shaping caches — none of which
//! next-core wants to inherit. FreeType is the library underneath it, and
//! talking to it directly is what the plan calls for: own the architecture,
//! reuse the mature library.
//!
//! Deliberately *not* here: font discovery and shaping. This turns a font file
//! into glyphs; choosing which file, and mapping text to glyph ids, are
//! separate problems.

use anyhow::{anyhow, Context, Result};
use std::ffi::CString;
use std::path::Path;
use std::sync::Arc;

use freetype::{
    FT_Done_Face, FT_Done_FreeType, FT_Face, FT_Get_Char_Index, FT_Init_FreeType, FT_Library,
    FT_Load_Char, FT_Pixel_Mode, FT_Select_Size, FT_Set_Pixel_Sizes, FT_LOAD_COLOR,
    FT_LOAD_RENDER,
};

/// A rasterized glyph: 8-bit coverage plus where to put it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterizedGlyph {
    /// One byte of coverage per pixel, row-major, `width * height` long.
    pub coverage: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Offset from the pen position to the bitmap's left edge.
    pub bearing_x: i32,
    /// Offset from the baseline up to the bitmap's top edge.
    pub bearing_y: i32,
    /// How far the pen advances after drawing, in pixels.
    pub advance_x: i32,
}

impl RasterizedGlyph {
    /// Expand coverage into RGBA, with the coverage in the alpha channel.
    ///
    /// The renderer tints with the cell's foreground colour and multiplies by
    /// alpha, so the colour channels only need to be non-zero; writing white
    /// keeps the texture readable in a debugger.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(self.coverage.len() * 4);
        for value in &self.coverage {
            rgba.extend_from_slice(&[0xff, 0xff, 0xff, *value]);
        }
        rgba
    }
}

/// One thread inside FreeType at a time, process-wide. This build has no
/// threading support, so separate libraries still share an allocator with no
/// lock of its own and two threads inside is an access violation -- which
/// guarding setup and teardown alone did not fix. Not a hot path: glyphs are
/// rasterized once and then live in an atlas. Not reentrant: never hold it
/// across another entry point.
pub(crate) static FREETYPE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Owns the FreeType library handle.
///
/// FreeType's library and face handles are not `Sync`: a face borrows from its
/// library and neither may be touched from two threads at once. The handle is
/// kept private and every use goes through `&mut self`, so the borrow checker
/// enforces that for us.
struct Library {
    raw: FT_Library,
}

/// Keeps a face's FreeType library alive for as long as something needs it.
///
/// Handed to the shaper, which references the *face* through HarfBuzz --
/// which is not enough on its own, because destroying a library destroys its
/// faces whatever their reference count says.
///
/// Deliberately not `Sync`: FreeType's library is not thread-safe, and saying
/// otherwise to make a type parameter fit is how an intermittent access
/// violation gets written. One was.
pub struct LibraryHandle(#[allow(dead_code)] std::sync::Arc<Library>);

// SAFETY: the raw handle is never handed out, and every method that touches it
// takes `&mut`, so no two threads can be inside FreeType at the same time.
unsafe impl Send for Library {}

impl Drop for Library {
    fn drop(&mut self) {
        let _inside = FREETYPE.lock();
        // SAFETY: `raw` came from a successful FT_Init_FreeType and is dropped
        // exactly once. Faces hold an Arc to this, so none are alive here.
        unsafe {
            FT_Done_FreeType(self.raw);
        }
    }
}

/// A font face loaded from a file, sized in pixels.
pub struct FontFace {
    // Kept alive for as long as the face: FreeType faces borrow from their
    // library, so dropping the library first would dangle.
    _library: Arc<Library>,
    face: FT_Face,
    pixel_size: u32,
    /// What a rasterized bitmap must be multiplied by to land at
    /// `pixel_size`: 1.0 for a scalable face; a bitmap-only face renders at
    /// its nearest strike, and this carries the correction.
    bitmap_scale: f32,
}

// SAFETY: same argument as `Library` -- the handle is private and every use
// goes through `&mut self`.
unsafe impl Send for FontFace {}

impl Drop for FontFace {
    fn drop(&mut self) {
        // Released before `_library` drops and takes the same lock.
        let _inside = FREETYPE.lock();
        // SAFETY: `face` came from a successful FT_New_Face and is dropped
        // exactly once, before the library it borrows from.
        unsafe {
            FT_Done_Face(self.face);
        }
    }
}

/// The coverage curve that stands in for the platform's font smoothing.
///
/// FreeType reports linear coverage, and compositing it as sRGB alpha reads
/// light-on-dark text thinner than the platform draws the same face --
/// CoreText both blends gamma-aware and fattens antialiased edges a touch.
/// Lifting mid coverage with a fixed power curve reproduces that weight;
/// full and empty pixels pass through untouched, so hinted stems stay crisp.
///
/// Windows needs it as much as macOS. Passing coverage through unchanged
/// there was meant to follow "the platform's thinner convention", but
/// DirectWrite is not thin: rendered from the same Cascadia Mono file at the
/// same size and colours, Windows Terminal put out 12% more ink than we did,
/// all of it in the antialiased edges -- the dim, washed-out text that made
/// the terminal read as a port. An exponent of 0.65 matched its ink to within
/// a percent; Linux gets the same, since its text goes through the same
/// uncorrected blend.
fn smoothed(value: u8) -> u8 {
    if value == 0 || value == 255 {
        return value;
    }
    static CURVE: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
    CURVE.get_or_init(|| {
        let exponent = smoothing_exponent();
        let mut table = [0u8; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            *slot = ((index as f32 / 255.0).powf(exponent) * 255.0).round() as u8;
        }
        table
    })[value as usize]
}

/// How hard [`smoothed`] lifts mid coverage on this platform.
fn smoothing_exponent() -> f32 {
    if cfg!(target_os = "macos") {
        // Matched against CoreText.
        0.62
    } else {
        // Matched against DirectWrite; see `smoothed`.
        0.65
    }
}

impl FontFace {
    /// Load `path` and size it to `pixel_size` pixels per em.
    pub fn open(path: &Path, pixel_size: u32) -> Result<Self> {
        Self::open_indexed(path, 0, pixel_size)
    }

    /// Load one face of a file that may carry several.
    ///
    /// A .ttc collection is a family in one file -- PingFang ships Ultralight
    /// through Semibold in a single path, and index 0 is whatever happens to
    /// be first. A caller that always opens 0 gets that arbitrary weight for
    /// the whole family, which is how every CJK glyph came out hairline-thin.
    pub fn open_indexed(path: &Path, face_index: i64, pixel_size: u32) -> Result<Self> {
        let library = Arc::new(Library::init()?);
        let path_c = CString::new(path.as_os_str().to_string_lossy().as_bytes())
            .with_context(|| format!("font path is not representable as C string: {path:?}"))?;

        let mut face: FT_Face = std::ptr::null_mut();
        // SAFETY: `library.raw` is live, `path_c` outlives the call, and
        // `face` is a valid out-pointer.
        let err = {
            let _inside = FREETYPE.lock();
            unsafe { FT_New_Face(library.raw, path_c.as_ptr(), face_index as _, &mut face) }
        };
        if err != 0 {
            return Err(anyhow!(
                "FT_New_Face({path:?}[{face_index}]) failed with error {err}"
            ));
        }

        let mut face = Self {
            _library: library,
            face,
            pixel_size: 0,
            bitmap_scale: 1.0,
        };
        face.set_pixel_size(pixel_size)?;
        Ok(face)
    }

    /// How many faces the file this face came from carries. 1 for a plain
    /// .ttf; the collection size for a .ttc.
    pub fn num_faces(&self) -> i64 {
        // SAFETY: `self.face` is live for as long as `self`.
        unsafe { (*self.face).num_faces as i64 }
    }

    /// A share in the library this face was loaded from.
    ///
    /// The shaper holds one. HarfBuzz takes its own reference on the *face*,
    /// which keeps the face alive -- but destroying the library destroys every
    /// face in it regardless, so without this a shaper outliving its face
    /// would call into freed memory when it was itself dropped.
    pub(crate) fn library(&self) -> LibraryHandle {
        LibraryHandle(self._library.clone())
    }

    /// The underlying FreeType face.
    ///
    /// For the shaper, which builds a HarfBuzz font from it. Crate-internal:
    /// the handle is not `Sync` and callers outside next-core have no way to
    /// uphold that.
    pub(crate) fn raw(&self) -> FT_Face {
        self.face
    }

    pub fn pixel_size(&self) -> u32 {
        self.pixel_size
    }

    /// Resize the face. Cheap enough to call per frame, but the caller is
    /// expected to cache rasterized glyphs, not re-rasterize them.
    pub fn set_pixel_size(&mut self, pixel_size: u32) -> Result<()> {
        let pixel_size = pixel_size.max(1);
        // SAFETY: `self.face` is live for the lifetime of `self`.
        let _inside = FREETYPE.lock();
        let err = unsafe { FT_Set_Pixel_Sizes(self.face, 0, pixel_size) };
        if err == 0 {
            self.pixel_size = pixel_size;
            self.bitmap_scale = 1.0;
            return Ok(());
        }
        // A bitmap-only face (a colour emoji font is strikes at fixed
        // sizes, no outlines) cannot take an arbitrary size, and FreeType
        // refuses rather than picking -- which is how the bundled emoji face
        // silently fell out of every stack. Pick the nearest strike
        // ourselves; the raster path corrects the difference.
        // SAFETY: `self.face` is live; `available_sizes` holds
        // `num_fixed_sizes` entries for as long as the face.
        let (count, sizes) = unsafe { ((*self.face).num_fixed_sizes, (*self.face).available_sizes) };
        if count > 0 && !sizes.is_null() {
            let strikes = unsafe { std::slice::from_raw_parts(sizes, count as usize) };
            // 26.6 fixed point, like every ppem FreeType reports.
            let ppem = |strike: &freetype::FT_Bitmap_Size| strike.y_ppem.font_units() as f32 / 64.0;
            let gap = |strike: &_| (ppem(strike) - pixel_size as f32).abs();
            let best = (0..strikes.len())
                .min_by(|a, b| gap(&strikes[*a]).total_cmp(&gap(&strikes[*b])))
                .unwrap_or(0);
            let best_ppem = ppem(&strikes[best]);
            // SAFETY: `best` indexes into the face's own strike table.
            if unsafe { FT_Select_Size(self.face, best as i32) } == 0 && best_ppem > 0.0 {
                self.pixel_size = pixel_size;
                self.bitmap_scale = pixel_size as f32 / best_ppem;
                return Ok(());
            }
        }
        Err(anyhow!(
            "FT_Set_Pixel_Sizes({pixel_size}) failed with error {err}"
        ))
    }

    /// The face's own line metrics at the current pixel size: ascender,
    /// descender (negative), and the line height the designer chose — all in
    /// pixels. A terminal that invents its own line height from a capital's
    /// bearing draws every font tighter than the font asked to be drawn.
    pub fn line_metrics(&self) -> Option<(f32, f32, f32)> {
        // SAFETY: `self.face` is live; `size` is set by FT_Set_Pixel_Sizes,
        // which `open` always calls before this can be reached.
        unsafe {
            let size = (*self.face).size;
            if size.is_null() {
                return None;
            }
            let metrics = (*size).metrics;
            // The size metrics are 26.6 fixed point; `font_units` hands back
            // the raw storage, and the shift is the same one the glyph
            // advance above uses.
            let height = (metrics.height.font_units() >> 6) as f32;
            if height <= 0.0 {
                return None;
            }
            Some((
                (metrics.ascender.font_units() >> 6) as f32,
                (metrics.descender.font_units() >> 6) as f32,
                height,
            ))
        }
    }

    /// What this face claims: family, style, and whether it is monospace.
    ///
    /// Returns `None` when the face reports no family name, which means the
    /// file is not something we can meaningfully offer to a user.
    pub fn describe(&self) -> Option<(String, String, bool)> {
        // SAFETY: `self.face` is live; FreeType keeps these strings alive for
        // the lifetime of the face, and they are NUL-terminated C strings.
        unsafe {
            let face = &*self.face;
            if face.family_name.is_null() {
                return None;
            }
            let family = std::ffi::CStr::from_ptr(face.family_name)
                .to_string_lossy()
                .into_owned();
            let style = if face.style_name.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(face.style_name)
                    .to_string_lossy()
                    .into_owned()
            };
            let monospace =
                face.face_flags & (freetype::FT_FACE_FLAG_FIXED_WIDTH as freetype::FT_Long) != 0;
            Some((family, style, monospace))
        }
    }

    /// Rasterize `ch` at the current pixel size.
    ///
    /// A glyph with no outline — a space, or a character this face does not
    /// cover — rasterizes to an empty bitmap with a real advance, which is
    /// what the layout wants: the pen still moves.
    /// The face's glyph for `ch`, or None when it has none.
    ///
    /// This is the question fallback turns on, and it has to be asked *before*
    /// rasterizing: `FT_Load_Char` does not fail on a missing character, it
    /// quietly loads glyph 0 and renders the empty box. A renderer that skips
    /// this check draws boxes instead of falling through to a face that has
    /// the character -- which is exactly what CJK looked like here.
    pub fn glyph_index_for(&self, ch: char) -> Option<u32> {
        // SAFETY: `self.face` is live for the lifetime of self.
        let _inside = FREETYPE.lock();
        let index = unsafe { FT_Get_Char_Index(self.face, ch as freetype::FT_ULong) };
        (index != 0).then_some(index as u32)
    }

    /// Whether this face can draw `ch` itself.
    pub fn has_glyph(&self, ch: char) -> bool {
        self.glyph_index_for(ch).is_some()
    }

    pub fn rasterize(&mut self, ch: char) -> Result<RasterizedGlyph> {
        // SAFETY: `self.face` is live for the lifetime of self.
        self.rasterize_with(|face, flags| unsafe {
            FT_Load_Char(face, ch as freetype::FT_ULong, flags)
        })
        .map_err(|err| anyhow!("FT_Load_Char({ch:?}) failed with error {err}"))
    }

    /// Rasterize by glyph index rather than character.
    ///
    /// This is the entry point the shaper feeds: shaping maps text to glyph
    /// ids, and a ligature or a positional form has no character to look up.
    pub fn rasterize_glyph_index(&mut self, glyph_index: u32) -> Result<RasterizedGlyph> {
        // SAFETY: `self.face` is live; an out-of-range index is a FreeType
        // error, not an out-of-bounds read.
        self.rasterize_with(|face, flags| unsafe {
            freetype::FT_Load_Glyph(face, glyph_index as freetype::FT_UInt, flags)
        })
        .map_err(|err| anyhow!("FT_Load_Glyph({glyph_index}) failed with error {err}"))
    }

    /// Whether this face keeps its ink in colour strikes rather than outlines.
    fn has_colour(&self) -> bool {
        // SAFETY: `self.face` is live for the lifetime of `self`.
        unsafe { (*self.face).face_flags & (freetype::FT_FACE_FLAG_COLOR as freetype::FT_Long) != 0 }
    }

    /// One raster walk for both entry points: a colour face is asked for
    /// colour, everything else is asked for monochrome and only retried in
    /// colour when it drew nothing. A plain failure returns the load error
    /// to name.
    ///
    /// The order matters, and getting it backwards is what made every
    /// emoji-only character draw inside out. Loading a colour strike
    /// *without* `FT_LOAD_COLOR` neither fails nor comes back empty:
    /// FreeType flattens the strike to 8-bit grey, and the grey of a black
    /// disc on a transparent ground is that disc's negative -- the cell
    /// fills solid and the glyph punches a hole out of it. Because the
    /// result was non-empty, the colour retry below was never reached, and
    /// characters no text face carries -- `⏺`, which is only in Noto Color
    /// Emoji and Apple Color Emoji, and which Claude Code prints on every
    /// message -- came out as their own photographic negative.
    fn rasterize_with(
        &mut self,
        mut load: impl FnMut(FT_Face, i32) -> freetype::FT_Error,
    ) -> std::result::Result<RasterizedGlyph, freetype::FT_Error> {
        let _inside = FREETYPE.lock();
        if self.has_colour() && load(self.face, (FT_LOAD_RENDER | FT_LOAD_COLOR) as i32) == 0 {
            if let Ok(glyph) = self.read_rendered_slot() {
                if !glyph.coverage.is_empty() {
                    return Ok(self.corrected(glyph));
                }
            }
        }
        let mono = load(self.face, FT_LOAD_RENDER as i32);
        let drawn = match mono {
            0 => Some(self.read_rendered_slot().map_err(|_| mono)?),
            _ => None,
        };
        if drawn.as_ref().is_some_and(|glyph| !glyph.coverage.is_empty()) {
            return Ok(self.corrected(drawn.expect("checked above")));
        }
        if load(self.face, (FT_LOAD_RENDER | FT_LOAD_COLOR) as i32) == 0 {
            if let Ok(glyph) = self.read_rendered_slot() {
                if !glyph.coverage.is_empty() || drawn.is_none() {
                    return Ok(self.corrected(glyph));
                }
            }
        }
        match drawn {
            Some(glyph) => Ok(self.corrected(glyph)),
            None => Err(mono),
        }
    }

    /// Bring a rasterized glyph to the size the caller asked this face for:
    /// a no-op for a scalable face; for a bitmap-strike face the slot holds
    /// the nearest strike -- a 17px request answered at 128px -- and drawing
    /// that as-is would put a fist-sized hand in a sidebar row.
    fn corrected(&self, glyph: RasterizedGlyph) -> RasterizedGlyph {
        let scale = self.bitmap_scale;
        if (scale - 1.0).abs() < f32::EPSILON || glyph.width == 0 || glyph.height == 0 {
            return glyph;
        }
        let width = ((glyph.width as f32 * scale).round() as usize).max(1);
        let height = ((glyph.height as f32 * scale).round() as usize).max(1);
        let mut coverage = Vec::with_capacity(width * height);
        // Area average, so thin strokes dim rather than vanish.
        for row in 0..height {
            let src_row0 = ((row as f32 / scale) as usize).min(glyph.height - 1);
            let src_row1 = (((row + 1) as f32 / scale).ceil() as usize)
                .clamp(src_row0 + 1, glyph.height);
            for column in 0..width {
                let src_col0 = ((column as f32 / scale) as usize).min(glyph.width - 1);
                let src_col1 = (((column + 1) as f32 / scale).ceil() as usize)
                    .clamp(src_col0 + 1, glyph.width);
                let mut sum = 0u32;
                let mut count = 0u32;
                for src_row in src_row0..src_row1 {
                    for src_col in src_col0..src_col1 {
                        sum += u32::from(glyph.coverage[src_row * glyph.width + src_col]);
                        count += 1;
                    }
                }
                coverage.push((sum / count.max(1)) as u8);
            }
        }
        RasterizedGlyph {
            coverage,
            width,
            height,
            bearing_x: (glyph.bearing_x as f32 * scale).round() as i32,
            bearing_y: (glyph.bearing_y as f32 * scale).round() as i32,
            advance_x: (glyph.advance_x as f32 * scale).round() as i32,
        }
    }

    /// Read whatever the last load rendered into the face's glyph slot.
    fn read_rendered_slot(&mut self) -> Result<RasterizedGlyph> {
        // SAFETY: after a successful FT_Load_Char the slot and its bitmap are
        // populated and remain valid until the next load on this face.
        let (bitmap, bearing_x, bearing_y, advance_x, buffer) = unsafe {
            let slot = &*(*self.face).glyph;
            let bitmap = slot.bitmap;
            (
                bitmap,
                slot.bitmap_left,
                slot.bitmap_top,
                // 26.6 fixed point: whole pixels live above the low 6 bits.
                (slot.advance.x.font_units() >> 6) as i32,
                bitmap.buffer,
            )
        };

        let width = bitmap.width as usize;
        let height = bitmap.rows as usize;
        let pitch = bitmap.pitch;
        let bgra = bitmap.pixel_mode == FT_Pixel_Mode::FT_PIXEL_MODE_BGRA as u8;

        let coverage = if width == 0 || height == 0 || buffer.is_null() {
            Vec::new()
        } else {
            // For BGRA the silhouette is the alpha, every fourth byte; the
            // smoothing curve stays a text-only affair.
            let (step, first) = if bgra { (4, 3) } else { (1, 0) };
            let mut coverage = Vec::with_capacity(width * height);
            for row in 0..height {
                // A negative pitch means the bitmap is stored bottom-up.
                let offset = if pitch >= 0 {
                    (row as isize) * (pitch as isize)
                } else {
                    ((height - 1 - row) as isize) * (-pitch as isize)
                };
                // SAFETY: FreeType guarantees `rows * |pitch|` readable bytes
                // from `buffer`, and `offset + width * step` stays inside.
                let row_bytes =
                    unsafe { std::slice::from_raw_parts(buffer.offset(offset), width * step) };
                let values = row_bytes.iter().skip(first).step_by(step).copied();
                if bgra {
                    coverage.extend(values);
                } else {
                    coverage.extend(values.map(smoothed));
                }
            }
            coverage
        };

        Ok(RasterizedGlyph {
            coverage,
            width,
            height,
            bearing_x,
            bearing_y,
            advance_x,
        })
    }
}

impl Library {
    fn init() -> Result<Self> {
        let _inside = FREETYPE.lock();
        let mut raw: FT_Library = std::ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer; on success FreeType fills it.
        let err = unsafe { FT_Init_FreeType(&mut raw) };
        if err != 0 {
            return Err(anyhow!("FT_Init_FreeType failed with error {err}"));
        }
        Ok(Self { raw })
    }
}

use freetype::FT_New_Face;

#[cfg(test)]
mod tests {
    use super::*;

    /// A font file that exists on the test machine.
    ///
    /// Discovery is out of scope for this module, so the tests take the
    /// platform's best-known path and skip when it is absent rather than
    /// failing on a machine that simply keeps its fonts elsewhere.
    fn test_font() -> Option<std::path::PathBuf> {
        let candidates: &[&str] = if cfg!(windows) {
            &[
                r"C:\Windows\Fonts\consola.ttf",
                r"C:\Windows\Fonts\cour.ttf",
                r"C:\Windows\Fonts\arial.ttf",
            ]
        } else if cfg!(target_os = "macos") {
            &[
                "/System/Library/Fonts/Menlo.ttc",
                "/Library/Fonts/Arial.ttf",
            ]
        } else {
            &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
                "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
            ]
        };
        candidates
            .iter()
            .map(std::path::PathBuf::from)
            .find(|path| path.exists())
    }

    #[test]
    fn rasterizes_a_glyph_with_partial_coverage() {
        let Some(path) = test_font() else {
            eprintln!("no system font found; skipping");
            return;
        };
        let mut face = FontFace::open(&path, 32).expect("open font");
        assert_eq!(face.pixel_size(), 32);

        let glyph = face.rasterize('M').expect("rasterize M");

        assert!(
            glyph.width > 0 && glyph.height > 0,
            "M should have an outline"
        );
        assert_eq!(glyph.coverage.len(), glyph.width * glyph.height);
        assert!(glyph.advance_x > 0, "the pen must advance past an M");

        // The point of rasterizing: a shape, not a filled box. Both fully
        // covered and fully empty pixels must be present, or the glyph would
        // render as a solid rectangle.
        assert!(
            glyph.coverage.iter().any(|c| *c > 0),
            "glyph has no ink at all"
        );
        assert!(
            glyph.coverage.iter().any(|c| *c == 0),
            "glyph is solid; coverage is not a mask"
        );
    }

    #[test]
    fn a_space_has_no_ink_but_still_advances() {
        let Some(path) = test_font() else {
            eprintln!("no system font found; skipping");
            return;
        };
        let mut face = FontFace::open(&path, 24).expect("open font");

        let glyph = face.rasterize(' ').expect("rasterize space");

        assert!(glyph.coverage.iter().all(|c| *c == 0));
        assert!(
            glyph.advance_x > 0,
            "a space must still move the pen, or text would pile up"
        );
    }

    #[test]
    fn resizing_changes_the_rasterized_size() {
        let Some(path) = test_font() else {
            eprintln!("no system font found; skipping");
            return;
        };
        let mut face = FontFace::open(&path, 16).expect("open font");
        let small = face.rasterize('W').expect("rasterize small W");

        face.set_pixel_size(48).expect("resize");
        let large = face.rasterize('W').expect("rasterize large W");

        assert!(
            large.height > small.height && large.width > small.width,
            "48px W ({}x{}) should be bigger than 16px ({}x{})",
            large.width,
            large.height,
            small.width,
            small.height
        );
    }

    #[test]
    fn rgba_puts_coverage_in_the_alpha_channel() {
        let glyph = RasterizedGlyph {
            coverage: vec![0, 128, 255],
            width: 3,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: 3,
        };

        let rgba = glyph.to_rgba();

        assert_eq!(rgba.len(), 12);
        // The renderer multiplies its own colour by this alpha, so the
        // coverage has to land there and nowhere else.
        assert_eq!(&rgba[0..4], &[0xff, 0xff, 0xff, 0]);
        assert_eq!(&rgba[4..8], &[0xff, 0xff, 0xff, 128]);
        assert_eq!(&rgba[8..12], &[0xff, 0xff, 0xff, 255]);
    }

    #[test]
    fn a_missing_font_file_is_an_error_not_a_panic() {
        let err = match FontFace::open(std::path::Path::new("does-not-exist.ttf"), 16) {
            Ok(_) => panic!("opening a missing font must fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("FT_New_Face"));
    }

    /// The bundled colour face, which is the only place several of the
    /// characters an agent prints actually live.
    fn colour_font() -> Option<std::path::PathBuf> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../assets/fonts/NotoColorEmoji.ttf");
        path.exists().then_some(path)
    }

    /// A glyph that only a colour face carries must come out as itself, not
    /// as its negative.
    ///
    /// `⏺` is a filled disc on a transparent ground: ink in the middle,
    /// nothing in the corners. Loaded without `FT_LOAD_COLOR` the strike
    /// flattens to grey and those two swap -- solid corners, hollow middle --
    /// which is what the terminal drew for every one of these characters.
    /// The corners and the centre are therefore the whole assertion: they
    /// are what tells the glyph from its own inverse.
    #[test]
    fn a_colour_only_glyph_is_not_rasterized_inside_out() {
        let Some(path) = colour_font() else {
            eprintln!("bundled colour font missing; skipping");
            return;
        };
        let mut face = FontFace::open(&path, 32).expect("open the colour face");
        let glyph = face.rasterize('\u{23FA}').expect("rasterize U+23FA");
        assert!(
            glyph.width >= 4 && glyph.height >= 4,
            "expected a real bitmap, got {}x{}",
            glyph.width,
            glyph.height
        );

        let at = |x: usize, y: usize| u32::from(glyph.coverage[y * glyph.width + x]);
        let (w, h) = (glyph.width, glyph.height);
        let corners = at(0, 0) + at(w - 1, 0) + at(0, h - 1) + at(w - 1, h - 1);
        let centre = at(w / 2, h / 2);

        assert!(
            centre > 128,
            "the disc's centre should be inked, not hollow (centre={})",
            centre
        );
        assert!(
            corners < 128,
            "the corners should be empty ground, not filled (sum={}); \
             solid corners around a hollow centre is the glyph inverted",
            corners
        );
    }

}

