//! Text shaping for next-core, straight onto HarfBuzz.
//!
//! `font_discovery` picks a file and `font_raster` draws a glyph, but neither
//! knows which glyphs a string becomes. That is shaping: text in, positioned
//! glyph ids out. It is not a per-character mapping — ligatures fuse several
//! characters into one glyph, combining marks attach to the base without
//! advancing, and scripts like Arabic pick a different glyph per position.
//!
//! HarfBuzz is the library every terminal uses for this, including the one we
//! are replacing. Using it directly is the plan: own the architecture, reuse
//! the mature library.

use crate::next_core::font_raster::FontFace;
use anyhow::{anyhow, Result};

/// One glyph the shaper produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapedGlyph {
    /// Glyph index in the face — not a character. Feed this to the rasterizer.
    pub glyph_index: u32,
    /// Byte offset in the input string this glyph came from.
    ///
    /// Several glyphs can share a cluster (a mark over a base), and one glyph
    /// can cover several characters (a ligature), so this is the only honest
    /// way back from a glyph to the text it renders.
    pub cluster: u32,
    /// How far the pen moves after this glyph, in pixels.
    pub x_advance: i32,
    pub y_advance: i32,
    /// Where to draw relative to the pen, in pixels.
    pub x_offset: i32,
    pub y_offset: i32,
}

/// A HarfBuzz font bound to a FreeType face.
///
/// Holds its own reference to the face, so the shaper and the rasterizer can
/// use the same font without one outliving the other.
pub struct Shaper {
    font: *mut harfbuzz::hb_font_t,
    /// Keeps the FreeType library alive for as long as this shaper.
    ///
    /// HarfBuzz references the *face*, which is not enough: destroying the
    /// library destroys its faces whatever their reference count, and a
    /// shaper dropped after its library would then free a face that is
    /// already gone. Declared after `font` so it outlives it.
    _library: crate::next_core::font_raster::LibraryHandle,
}

// SAFETY: the raw handle is private and every method takes `&mut self`, so
// HarfBuzz is never entered from two threads at once.
unsafe impl Send for Shaper {}

impl Drop for Shaper {
    fn drop(&mut self) {
        let _inside = crate::next_core::font_raster::FREETYPE.lock();
        // SAFETY: `font` came from hb_ft_font_create_referenced and is
        // destroyed exactly once; it released its own face reference.
        unsafe {
            harfbuzz::hb_font_destroy(self.font);
        }
    }
}

impl Shaper {
    /// Build a shaper for `face`.
    ///
    /// The face's current pixel size is baked in: HarfBuzz reads metrics from
    /// FreeType at creation, so resizing the face afterwards needs a new
    /// shaper or the advances will be for the old size.
    pub fn new(face: &FontFace) -> Result<Self> {
        // The two bindings each generated their own Rust type for FT_Face
        // from the same C header, so the pointer is cast across. They describe
        // the same struct; nothing is reinterpreted.
        //
        // SAFETY: `face.raw()` is live for the duration of the call, and the
        // "referenced" variant takes its own reference rather than borrowing,
        // so the HarfBuzz font stays valid even if the caller drops the face.
        let _inside = crate::next_core::font_raster::FREETYPE.lock();
        let font = unsafe { harfbuzz::hb_ft_font_create_referenced(face.raw() as _) };
        if font.is_null() {
            return Err(anyhow!("hb_ft_font_create_referenced returned null"));
        }
        Ok(Self {
            font,
            _library: face.library(),
        })
    }

    /// Shape `text` into positioned glyphs.
    ///
    /// Script, language, and direction are guessed from the text itself, which
    /// is what a terminal wants: it has no higher-level markup to consult.
    pub fn shape(&mut self, text: &str) -> Result<Vec<ShapedGlyph>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let _inside = crate::next_core::font_raster::FREETYPE.lock();

        // SAFETY: the buffer is created, filled, shaped, read, and destroyed
        // within this call; no pointer escapes it.
        unsafe {
            let buffer = harfbuzz::hb_buffer_create();
            if buffer.is_null() {
                return Err(anyhow!("hb_buffer_create returned null"));
            }
            // From here on every early return must destroy the buffer, so the
            // work is wrapped and the result handled after.
            let result = (|| -> Result<Vec<ShapedGlyph>> {
                harfbuzz::hb_buffer_add_utf8(
                    buffer,
                    text.as_ptr() as *const std::os::raw::c_char,
                    text.len() as i32,
                    0,
                    text.len() as i32,
                );
                harfbuzz::hb_buffer_guess_segment_properties(buffer);
                harfbuzz::hb_shape(self.font, buffer, std::ptr::null(), 0);

                let mut count: u32 = 0;
                let infos = harfbuzz::hb_buffer_get_glyph_infos(buffer, &mut count);
                let positions = harfbuzz::hb_buffer_get_glyph_positions(buffer, &mut count);
                if infos.is_null() || positions.is_null() {
                    return Err(anyhow!("HarfBuzz returned no glyph array after shaping"));
                }

                let count = count as usize;
                let infos = std::slice::from_raw_parts(infos, count);
                let positions = std::slice::from_raw_parts(positions, count);

                Ok(infos
                    .iter()
                    .zip(positions)
                    .map(|(info, pos)| ShapedGlyph {
                        glyph_index: info.codepoint,
                        cluster: info.cluster,
                        // 26.6 fixed point, same as FreeType's advances.
                        x_advance: from_26_6(pos.x_advance),
                        y_advance: from_26_6(pos.y_advance),
                        x_offset: from_26_6(pos.x_offset),
                        y_offset: from_26_6(pos.y_offset),
                    })
                    .collect())
            })();
            harfbuzz::hb_buffer_destroy(buffer);
            result
        }
    }
}

/// A 26.6 fixed-point value in whole pixels, rounded to the nearest.
///
/// A bare `>> 6` floors toward negative infinity, so a small negative offset
/// (a mark nudged left by -10/64 px) became a whole pixel left, and every
/// kerned or mark-positioned glyph carried a systematic 1px left/down bias.
pub fn from_26_6(value: i32) -> i32 {
    (value + 32) >> 6
}

#[cfg(test)]
mod rounding_tests {
    use super::from_26_6;

    #[test]
    fn fixed_point_rounds_to_the_nearest_pixel() {
        assert_eq!(from_26_6(0), 0);
        assert_eq!(from_26_6(64), 1);
        assert_eq!(from_26_6(-64), -1);
        // Small negatives are zero, not -1: the old floor's bias.
        assert_eq!(from_26_6(-10), 0);
        assert_eq!(from_26_6(-31), 0);
        assert_eq!(from_26_6(-33), -1);
        assert_eq!(from_26_6(31), 0);
        assert_eq!(from_26_6(32), 1);
        assert_eq!(from_26_6(10 * 64 + 40), 11);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::next_core::font_discovery::FontIndex;

    fn monospace_face(pixel_size: u32) -> Option<FontFace> {
        let index = FontIndex::scan();
        let entry = index.default_monospace()?;
        FontFace::open_indexed(&entry.path, entry.face_index, pixel_size).ok()
    }

    #[test]
    fn shapes_ascii_into_one_glyph_per_character() {
        let Some(face) = monospace_face(24) else {
            eprintln!("no monospace font found; skipping");
            return;
        };
        let mut shaper = Shaper::new(&face).expect("create shaper");

        let glyphs = shaper.shape("abc").expect("shape abc");

        assert_eq!(glyphs.len(), 3, "plain ASCII should not fuse or split");
        // Clusters are byte offsets into the input, so plain ASCII walks 0,1,2.
        assert_eq!(
            glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(
            glyphs.iter().all(|g| g.glyph_index != 0),
            "glyph 0 is .notdef; the face should cover ASCII"
        );
        assert!(
            glyphs.iter().all(|g| g.x_advance > 0),
            "every glyph must advance the pen"
        );
    }

    #[test]
    fn a_monospace_face_advances_every_glyph_equally() {
        let Some(face) = monospace_face(24) else {
            eprintln!("no monospace font found; skipping");
            return;
        };
        let mut shaper = Shaper::new(&face).expect("create shaper");

        let glyphs = shaper.shape("iWm.").expect("shape mixed widths");

        // That is what monospace means, and the terminal's whole grid depends
        // on it: an 'i' and a 'W' occupy the same cell.
        let advances: Vec<i32> = glyphs.iter().map(|g| g.x_advance).collect();
        assert!(!advances.is_empty());
        assert!(
            advances.windows(2).all(|w| w[0] == w[1]),
            "monospace face gave uneven advances: {:?}",
            advances
        );
    }

    #[test]
    fn shaping_follows_the_faces_pixel_size() {
        let Some(small) = monospace_face(16) else {
            eprintln!("no monospace font found; skipping");
            return;
        };
        let Some(large) = monospace_face(48) else {
            return;
        };
        let mut small_shaper = Shaper::new(&small).expect("create small shaper");
        let mut large_shaper = Shaper::new(&large).expect("create large shaper");

        let small_advance = small_shaper.shape("M").expect("shape small")[0].x_advance;
        let large_advance = large_shaper.shape("M").expect("shape large")[0].x_advance;

        assert!(
            large_advance > small_advance,
            "48px advance ({}) should exceed 16px ({})",
            large_advance,
            small_advance
        );
    }

    /// The whole font pipeline, end to end and without wezterm-font: find a
    /// face on this machine, shape text with it, and draw the glyphs the
    /// shaper named. Each piece is tested alone; this is the one that would
    /// catch them not fitting together.
    #[test]
    fn discovered_font_shapes_and_rasterizes_its_own_glyphs() {
        let Some(mut face) = monospace_face(28) else {
            eprintln!("no monospace font found; skipping");
            return;
        };
        let mut shaper = Shaper::new(&face).expect("create shaper");

        let glyphs = shaper.shape("Hi").expect("shape text");
        assert_eq!(glyphs.len(), 2);

        for glyph in &glyphs {
            // Rasterize by the index the shaper produced, not by character:
            // that is the only handle a ligature or positional form has.
            let raster = face
                .rasterize_glyph_index(glyph.glyph_index)
                .expect("rasterize the shaped glyph");
            assert!(
                raster.width > 0 && raster.height > 0,
                "shaped glyph {} rasterized to nothing",
                glyph.glyph_index
            );
            assert!(
                raster.coverage.iter().any(|c| *c > 0),
                "shaped glyph {} has no ink",
                glyph.glyph_index
            );
            assert!(
                raster.coverage.iter().any(|c| *c == 0),
                "shaped glyph {} is a solid block, not a mask",
                glyph.glyph_index
            );
        }
    }

    #[test]
    fn empty_text_shapes_to_nothing() {
        let Some(face) = monospace_face(24) else {
            eprintln!("no monospace font found; skipping");
            return;
        };
        let mut shaper = Shaper::new(&face).expect("create shaper");

        assert!(shaper.shape("").expect("shape empty").is_empty());
    }

    #[test]
    fn clusters_map_glyphs_back_to_their_bytes() {
        let Some(face) = monospace_face(24) else {
            eprintln!("no monospace font found; skipping");
            return;
        };
        let mut shaper = Shaper::new(&face).expect("create shaper");

        // A 3-byte character: the cluster is a byte offset, not a char index,
        // so the glyph after it starts at 3 rather than 1.
        let text = "你a";
        let glyphs = shaper.shape(text).expect("shape mixed-width text");

        assert!(!glyphs.is_empty());
        assert_eq!(glyphs[0].cluster, 0);
        if let Some(last) = glyphs.last() {
            assert!(
                (last.cluster as usize) < text.len(),
                "cluster {} is outside the input",
                last.cluster
            );
        }
        // Every cluster must land on a character boundary, or slicing the
        // input by cluster would panic.
        for glyph in &glyphs {
            assert!(
                text.is_char_boundary(glyph.cluster as usize),
                "cluster {} is not a char boundary in {text:?}",
                glyph.cluster
            );
        }
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use crate::next_core::font_discovery::FontIndex;

    /// A shaper outliving the face it was built from must not crash.
    ///
    /// Each face owns a share in a FreeType library, and destroying a library
    /// destroys its faces whatever their reference count -- so a shaper that
    /// only held HarfBuzz's reference to the *face* would free memory that had
    /// already gone. It cost an access violation to find; this is what keeps
    /// it found.
    #[test]
    fn a_shaper_survives_the_face_it_was_built_from() {
        let index = FontIndex::scan();
        let Some(entry) = index.default_monospace() else {
            eprintln!("no monospace font found; skipping");
            return;
        };
        let Ok(face) = FontFace::open_indexed(&entry.path, entry.face_index, 16) else {
            return;
        };
        let mut shaper = Shaper::new(&face).expect("create shaper");

        drop(face);

        // Still usable, and still droppable.
        let glyphs = shaper.shape("abc").expect("shape after the face is gone");
        assert_eq!(glyphs.len(), 3);
    }
}
