//! A primary face and the faces behind it.
//!
//! One face cannot draw everything. A monospace programming font typically has
//! Latin, some symbols, and nothing else -- which is why CJK came out as boxes
//! here before this existed. Fallback is not a nicety: without it, whole
//! writing systems are unreadable.
//!
//! The trap this is built around: `FT_Load_Char` does not fail on a character
//! the face lacks. It loads glyph 0 and renders an empty box, so a renderer
//! that only checks for errors never learns it should have asked someone else.
//! Every lookup here asks `has_glyph` first.

use unterm_engine::next_core::font_discovery::{FontEntry, FontIndex};
use unterm_engine::next_core::font_raster::{FontFace, RasterizedGlyph};
use unterm_engine::next_core::font_shaper::{ShapedGlyph, Shaper};

/// Families to try when the primary face has nothing, in order.
///
/// Chosen for coverage rather than looks: the first three carry CJK on the
/// three platforms, and the last two carry symbols and emoji. A character with
/// no face at all is still better shown as the primary's box than as nothing.
/// Faces to try for characters the chosen font does not have.
///
/// Family names as the font files actually report them, which is not always
/// what the font is called in a menu: Windows ships "Microsoft YaHei", and
/// asking for "Microsoft YaHei UI" -- which is what this list used to say --
/// matches nothing, so Chinese fell through to the primary face and drew as
/// boxes on the one platform most likely to need it.
const FALLBACK_FAMILIES: &[&str] = &[
    // Chinese
    "Microsoft YaHei",
    "SimSun",
    "DengXian",
    "PingFang SC",
    // Chinese faces every macOS carries. PingFang is downloaded on demand and
    // a fresh system may not have it yet; without these the next CJK face was
    // Japanese Hiragino, which has no 页, 贴, 话, 标 or 签, and the chrome
    // printed "新建标签页" as "新建".
    "Hiragino Sans GB",
    "Heiti SC",
    "STHeiti",
    "Songti SC",
    "Noto Sans CJK SC",
    "Noto Sans Mono CJK SC",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
    // Japanese and Korean, which the Chinese faces do not fully cover
    "Yu Gothic",
    "Meiryo",
    "Hiragino Sans",
    "Malgun Gothic",
    "Noto Sans CJK JP",
    "Noto Sans CJK KR",
    // Symbols and emoji. One per platform: geometric shapes and marks like
    // the chrome's ▶ and ✓ live in a symbol face, and each system names its
    // own -- a machine without one draws chrome whose marks are simply gone.
    "Segoe UI Symbol",
    // Apple Symbols has the geometric shapes but no tick; Menlo carries the
    // tick and the dingbat-adjacent marks. Both ship with every macOS.
    "Apple Symbols",
    "Menlo",
    "Noto Sans Symbols 2",
    "Segoe UI Emoji",
    "Symbols Nerd Font Mono",
    "Noto Color Emoji",
];

/// The font files that ship with the product.
///
/// Two of them, and both matter for how the window looks rather than for what
/// the terminal can print. The symbols face carries the icon language the
/// chrome is drawn in -- the folder on a project row, the shell mark on a tab,
/// the robot on an agent's pane -- and the emoji face carries the rest.
///
/// Looked up by file rather than by family, because they are not installed:
/// asking the system index for "Symbols Nerd Font Mono" on a machine that has
/// only the bundled copy finds nothing, and every icon in the chrome comes out
/// as an empty box. Which is exactly what happened.
const BUNDLED: &[&str] = &["SymbolsNerdFontMono-Regular.ttf", "NotoColorEmoji.ttf"];

/// Where the bundled fonts are, wherever this build is running from.
///
/// Beside the executable in an installed copy; a few levels up in a build
/// tree, where the exe sits in `target/debug`. Tried in that order so an
/// installed copy never reads out of somebody's source checkout.
fn bundled_font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(beside) = exe.parent() {
            dirs.push(beside.join("assets").join("fonts"));
            if let Some(prefix) = beside.parent() {
                // Linux packages and AppImages keep architecture-independent
                // data under usr/share. macOS keeps it in the app bundle's
                // Resources directory beside MacOS.
                dirs.push(prefix.join("share").join("unterm").join("fonts"));
                dirs.push(prefix.join("Resources").join("assets").join("fonts"));
            }
            for up in [1usize, 2, 3] {
                let mut at = beside.to_path_buf();
                for _ in 0..up {
                    at = match at.parent() {
                        Some(parent) => parent.to_path_buf(),
                        None => break,
                    };
                }
                dirs.push(at.join("assets").join("fonts"));
            }
        }
    }
    if let Ok(here) = std::env::current_dir() {
        dirs.push(here.join("assets").join("fonts"));
    }
    // Cargo permits the target directory to live anywhere. In that case the
    // executable-relative walk above cannot reach the checkout, and tests (or
    // a developer build launched from another directory) would silently lose
    // the product's own symbol and emoji faces. The manifest location is fixed
    // at compile time, so it remains a reliable source-tree fallback without
    // affecting an installed copy, which finds its assets beside the exe.
    if let Some(workspace) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        dirs.push(workspace.join("assets").join("fonts"));
    }
    dirs
}

/// One bundled face by file name, if this build can find it.
pub fn bundled_face(name: &str, pixel_size: u32) -> Option<FontFace> {
    bundled_font_dirs()
        .iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .and_then(|path| FontFace::open_in_memory(&path, pixel_size).ok())
}

/// The bundled faces this build can actually find.
fn bundled_faces(pixel_size: u32) -> Vec<FontFace> {
    let dirs = bundled_font_dirs();
    let mut faces = Vec::new();
    for name in BUNDLED {
        let found = dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|path| path.is_file());
        match found {
            Some(path) => match FontFace::open_in_memory(&path, pixel_size) {
                Ok(face) => faces.push(face),
                Err(err) => log::warn!("bundled font {path:?} would not open: {err:#}"),
            },
            None => log::warn!("bundled font {name} is not beside this build"),
        }
    }
    faces
}

pub struct FontStack {
    faces: Vec<FontFace>,
    pixel_size: u32,
    /// One shaper per face, built on first use.
    ///
    /// Building one binds HarfBuzz to a FreeType face and reads its tables;
    /// doing that per row would cost more than the shaping. They live as long
    /// as the stack because the faces do.
    shapers: Vec<Option<Shaper>>,
}

impl FontStack {
    /// Build a stack around `primary`, adding whichever fallbacks this machine
    /// actually has.
    pub fn new(primary: FontFace, requested: &[String], pixel_size: u32) -> Self {
        let index = FontIndex::cached();
        let mut faces = vec![primary];

        // The config's own list first: someone who named a font meant it.
        let wanted = requested
            .iter()
            .map(String::as_str)
            .chain(FALLBACK_FAMILIES.iter().copied());

        // The bundled symbols face goes in front of the installed families, so
        // the chrome's icons come from the file that was chosen for them rather
        // than from whichever installed face happens to claim the code point.
        //
        // The emoji face does not: it is a colour-bitmap font that claims a lot
        // of ordinary text symbols -- arrows, geometric shapes -- and renders
        // none of them under a monochrome pass. In front, it swallows every one
        // of those and they come out blank. It belongs at the very end, where
        // it answers only for what nothing else has.
        let mut bundled = bundled_faces(pixel_size);
        let emoji = (bundled.len() > 1).then(|| bundled.remove(1));
        faces.extend(bundled);

        let mut seen: Vec<String> = Vec::new();
        for family in wanted {
            if seen.iter().any(|name| name == family) {
                continue;
            }
            seen.push(family.to_string());
            if let Some(entry) = index.best_in_family(family) {
                if let Ok(face) = open(entry, pixel_size) {
                    faces.push(face);
                }
            }
        }

        // And the emoji face last of all, for the code points nothing else has.
        faces.extend(emoji);

        let shapers = (0..faces.len()).map(|_| None).collect();
        Self {
            faces,
            pixel_size,
            shapers,
        }
    }

    /// The system's default monospace face, at a given size.
    ///
    /// For work that needs a font but has no window to take one from -- a
    /// scrollback capture, say. Returns None when the machine has no
    /// monospace font at all, which is a thing to report rather than panic on.
    pub fn system(pixel_size: u32) -> Option<Self> {
        let index = FontIndex::scan();
        let entry = index.default_monospace()?;
        let primary = FontFace::open(&entry.path, pixel_size).ok()?;
        Some(FontStack::new(primary, &[], pixel_size))
    }

    pub fn pixel_size(&self) -> u32 {
        self.pixel_size
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    /// Whether any face in the stack actually has this character.
    ///
    /// Distinct from `face_for`, which falls back to the primary so that
    /// something is always drawn: a caller asking whether a mark exists at all
    /// has to be able to tell a real answer from the fallback. Only tests ask
    /// today -- the glyph-coverage checks in `chrome_font`.
    #[cfg(test)]
    pub fn covers(&self, ch: char) -> bool {
        self.faces.iter().any(|face| face.has_glyph(ch))
    }

    /// Which face should draw `ch`, if any can.
    ///
    /// Falls back to the primary when nothing has it, so the character comes
    /// out as that face's box rather than as a hole in the line.
    pub fn face_for(&self, ch: char) -> usize {
        self.faces
            .iter()
            .position(|face| face.has_glyph(ch))
            .unwrap_or(0)
    }

    /// Shape a run of text with one face.
    ///
    /// `None` when the face cannot be shaped -- a bitmap font, a face
    /// HarfBuzz will not open. The caller falls back to placing each
    /// character on its own cell, which is what this did before shaping
    /// existed and is still right for a font with no ligatures.
    pub fn shape(&mut self, face: usize, text: &str) -> Option<Vec<ShapedGlyph>> {
        if face >= self.faces.len() {
            return None;
        }
        if self.shapers.len() <= face {
            self.shapers.resize_with(face + 1, || None);
        }
        if self.shapers[face].is_none() {
            self.shapers[face] = Shaper::new(&self.faces[face]).ok();
        }
        self.shapers[face]
            .as_mut()?
            .shape(text)
            .ok()
            .filter(|glyphs| !glyphs.is_empty())
    }

    /// The face's own index for a character.
    ///
    /// The real index, not the code point. Both drawing paths file glyphs in
    /// the atlas by index, and a character code standing in for one collides
    /// with whatever glyph actually has that number -- which shows up as
    /// characters vanishing from the middle of a word.
    pub fn glyph_index_for(&self, face: usize, ch: char) -> Option<u32> {
        self.faces.get(face)?.glyph_index_for(ch)
    }

    /// Rasterize one glyph of a face by its index, as the shaper reports it.
    pub fn rasterize_index(&mut self, face: usize, glyph_index: u32) -> Option<RasterizedGlyph> {
        self.faces
            .get_mut(face)?
            .rasterize_glyph_index(glyph_index)
            .ok()
    }

    /// As `rasterize_index`, at a pixel size other than the stack's own.
    ///
    /// For fitting a fallback glyph to the primary's cells: a fallback face
    /// was designed around its own em, and taking it at the primary's size
    /// leaves a double-width CJK glyph well short of its two cells. The face
    /// is put back to the stack's size before returning, whatever happened,
    /// because shaping and every other rasterization assume the base size.
    pub fn rasterize_index_at(
        &mut self,
        face: usize,
        glyph_index: u32,
        pixel_size: u32,
    ) -> Option<RasterizedGlyph> {
        if pixel_size == self.pixel_size {
            return self.rasterize_index(face, glyph_index);
        }
        let base = self.pixel_size;
        let face = self.faces.get_mut(face)?;
        face.set_pixel_size(pixel_size).ok()?;
        let rasterized = face.rasterize_glyph_index(glyph_index);
        if let Err(err) = face.set_pixel_size(base) {
            log::warn!("could not restore font size {base}: {err:#}");
        }
        rasterized.ok()
    }

    /// As `rasterize`, at a pixel size other than the stack's own.
    ///
    /// The same face-by-face walk, with each candidate resized for its one
    /// glyph and put back before the next thing touches it. The blank-bitmap
    /// skip is kept: a colour face that reports a glyph and renders nothing
    /// renders nothing at every size.
    pub fn rasterize_at(&mut self, ch: char, pixel_size: u32) -> Option<(usize, RasterizedGlyph)> {
        if pixel_size == self.pixel_size {
            return self.rasterize(ch);
        }
        let candidates: Vec<usize> = self
            .faces
            .iter()
            .enumerate()
            .filter(|(_, face)| face.has_glyph(ch))
            .map(|(index, _)| index)
            .collect();

        let base = self.pixel_size;
        let mut first: Option<(usize, RasterizedGlyph)> = None;
        for index in candidates {
            let face = &mut self.faces[index];
            if face.set_pixel_size(pixel_size).is_err() {
                continue;
            }
            let rasterized = face.rasterize(ch);
            if let Err(err) = face.set_pixel_size(base) {
                log::warn!("could not restore font size {base}: {err:#}");
            }
            let Ok(glyph) = rasterized else {
                continue;
            };
            if glyph.width > 0 && glyph.height > 0 {
                return Some((index, glyph));
            }
            // Keep the first answer as a fallback: a character with no ink
            // anywhere still needs its advance.
            first.get_or_insert((index, glyph));
        }
        first
    }

    /// Rasterize `ch` from whichever face can actually draw it.
    ///
    /// Having a glyph and drawing one are different questions, and the gap
    /// between them is invisible: a colour-bitmap face reports a glyph for a
    /// text symbol like `▶` and renders nothing at all under a monochrome
    /// pass. The pen then advances over a blank, which reads as a spacing bug
    /// rather than a font one -- which is exactly how the play mark vanished
    /// from the top bar while leaving its gap behind.
    ///
    /// So a face whose bitmap comes out empty is passed over, and the next one
    /// that has the character is asked. A character that is *meant* to be
    /// blank -- a space, a zero-width joiner -- has no bitmap from any face and
    /// falls out of the loop with nothing, which is the right answer for it.
    pub fn rasterize(&mut self, ch: char) -> Option<(usize, RasterizedGlyph)> {
        let candidates: Vec<usize> = self
            .faces
            .iter()
            .enumerate()
            .filter(|(_, face)| face.has_glyph(ch))
            .map(|(index, _)| index)
            .collect();

        let mut first: Option<(usize, RasterizedGlyph)> = None;
        for index in candidates {
            let Ok(glyph) = self.faces[index].rasterize(ch) else {
                continue;
            };
            if glyph.width > 0 && glyph.height > 0 {
                return Some((index, glyph));
            }
            // Keep the first answer as a fallback: a character with no ink
            // anywhere still needs its advance.
            first.get_or_insert((index, glyph));
        }
        first
    }
}

fn open(entry: &FontEntry, pixel_size: u32) -> anyhow::Result<FontFace> {
    Ok(FontFace::open_indexed(&entry.path, entry.face_index, pixel_size)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> Option<FontStack> {
        FontStack::system(16)
    }

    #[test]
    fn latin_comes_from_the_primary_face() {
        let Some(stack) = stack() else {
            return;
        };

        // A monospace face has Latin; falling back for it would mean losing the
        // font the user chose on the characters they see most.
        assert_eq!(stack.face_for('A'), 0);
    }

    #[test]
    fn a_character_the_primary_lacks_finds_another_face() {
        let Some(stack) = stack() else {
            return;
        };
        if stack.len() < 2 {
            // No fallback family installed; nothing to assert.
            return;
        }

        // If the primary genuinely has CJK, this is not a fallback case.
        let primary_has_cjk = stack.faces[0].has_glyph('漢');
        let chosen = stack.face_for('漢');

        if primary_has_cjk {
            assert_eq!(chosen, 0);
        } else {
            assert!(
                chosen > 0 || !stack.faces.iter().any(|face| face.has_glyph('漢')),
                "a face with the character should have been chosen"
            );
        }
    }

    #[test]
    fn a_character_nobody_has_still_draws_something() {
        let Some(stack) = stack() else {
            return;
        };

        // A private-use codepoint no font is likely to carry.
        let chosen = stack.face_for('\u{10FFFD}');

        // The primary's box is a better answer than a hole in the line.
        assert_eq!(chosen, 0);
    }

    #[test]
    fn rasterizing_reports_which_face_drew_it() {
        let Some(mut stack) = stack() else {
            return;
        };

        let Some((face, glyph)) = stack.rasterize('A') else {
            return;
        };

        // The atlas keys on the face, so a glyph from a fallback must not be
        // filed under the primary -- two faces' glyph for the same character
        // would collide.
        assert_eq!(face, 0);
        assert!(glyph.width > 0);
    }

    #[test]
    fn a_named_font_is_tried_before_the_built_in_list() {
        let index = FontIndex::scan();
        let Some(entry) = index.default_monospace() else {
            return;
        };
        let Ok(primary) = FontFace::open(&entry.path, 16) else {
            return;
        };

        // Someone who named a font in their config meant it.
        let stack = FontStack::new(primary, &["Consolas".to_string()], 16);

        if index.best_in_family("Consolas").is_some() {
            assert!(stack.len() >= 2);
        }
    }
}

#[cfg(test)]
mod shaping_tests {
    use super::*;

    /// The same thing the engine's own test does, in this binary.
    ///
    /// If this crashes and the engine's does not, the fault is in how this
    /// binary is linked rather than in either piece of code.
    #[test]
    fn a_bare_face_shapes_in_this_binary_too() {
        use unterm_engine::next_core::font_discovery::FontIndex;
        let index = FontIndex::scan();
        let Some(entry) = index.default_monospace() else {
            return;
        };
        let Ok(face) = FontFace::open(&entry.path, 16) else {
            return;
        };
        let Ok(mut shaper) = Shaper::new(&face) else {
            return;
        };
        let glyphs = shaper.shape("abc").expect("shape abc");
        assert_eq!(glyphs.len(), 3);
    }

    #[test]
    fn a_stack_can_shape_with_its_primary_face() {
        let Some(mut stack) = FontStack::system(16) else {
            return;
        };
        let Some(glyphs) = stack.shape(0, "abc") else {
            return;
        };
        assert_eq!(glyphs.len(), 3, "plain ASCII should not fuse or split");
        for glyph in &glyphs {
            assert!(
                stack.rasterize_index(0, glyph.glyph_index).is_some(),
                "the shaper's own glyph index must be rasterizable"
            );
        }
    }
}

#[cfg(test)]
mod cjk_tests {
    use super::*;

    /// A Chinese character has to reach a face that can draw it.
    ///
    /// The screen showed boxes where the engine held correct UTF-8, which is
    /// the symptom of a fallback that never happened: every face reports the
    /// character, or none does, and the stack draws .notdef either way.
    #[test]
    fn a_chinese_character_finds_a_face_that_has_it() {
        // A hosted CI runner has no desktop font install to probe -- what
        // this asserts is the machine, and the machines it speaks for are
        // the ones people run the product on.
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return;
        }
        let Some(mut stack) = FontStack::system(16) else {
            return;
        };
        let face = stack.face_for('中');
        assert!(
            stack.faces[face].has_glyph('中'),
            "face {face} of {} was chosen for 中 but does not have it",
            stack.faces.len()
        );

        let (drew, glyph) = stack.rasterize('中').expect("something must draw it");
        assert_eq!(drew, face, "the face that draws it is the one chosen");
        assert!(
            glyph.width > 0 && glyph.height > 0,
            "a blank bitmap is a box in disguise"
        );
    }

    #[test]
    fn the_stack_actually_has_a_face_with_chinese() {
        // A hosted CI runner has no desktop font install to probe -- what
        // this asserts is the machine, and the machines it speaks for are
        // the ones people run the product on.
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return;
        }
        let Some(stack) = FontStack::system(16) else {
            return;
        };
        let with_cjk: Vec<usize> = (0..stack.faces.len())
            .filter(|index| stack.faces[*index].has_glyph('中'))
            .collect();
        assert!(
            !with_cjk.is_empty(),
            "no face in the stack can draw Chinese; the fallback list is the bug"
        );
    }
}

#[cfg(test)]
mod bundled_tests {
    use super::*;

    /// The product ships the face its own chrome is drawn in. Without it every
    /// icon in the window -- the folder on a project row, the shell mark on a
    /// tab, the robot on an agent's pane -- is an empty box, which is what a
    /// missing glyph looks like and exactly what happened.
    #[test]
    fn the_bundled_faces_are_found_and_open() {
        let faces = bundled_faces(16);
        assert_eq!(
            faces.len(),
            BUNDLED.len(),
            "only {} of {} bundled faces were found; looked in {:?}",
            faces.len(),
            BUNDLED.len(),
            bundled_font_dirs()
        );
    }

    /// And they carry the code points the chrome asks for. A face that opens
    /// but has no folder glyph is a face that changes nothing.
    #[test]
    fn the_symbols_face_has_the_icons_the_chrome_uses() {
        let Some(face) = bundled_faces(16).into_iter().next() else {
            panic!("no bundled symbols face");
        };
        // Folder, the shell marks, the robot, the remote mark: every icon the
        // sidebar and the tab rows draw.
        for (name, ch) in [
            ("folder", '\u{f07b}'),
            ("powershell", '\u{ebc7}'),
            ("cmd", '\u{ebc4}'),
            ("bash", '\u{ebca}'),
            ("terminal", '\u{ea85}'),
            ("robot", '\u{f06a9}'),
            ("remote", '\u{eb3a}'),
        ] {
            assert!(
                face.has_glyph(ch),
                "the bundled symbols face has no {name} ({ch:?})"
            );
        }
    }

    /// A stack built from it can reach those code points, which is the thing
    /// the window actually depends on.
    #[test]
    fn a_stack_can_reach_the_chrome_icons() {
        let Some(stack) = FontStack::system(16) else {
            return;
        };
        for ch in ['\u{f07b}', '\u{ebc7}', '\u{ea85}', '\u{f06a9}'] {
            // Face 0 is the primary text face, which has none of these: an
            // icon that falls through to it is an icon drawn as an empty box.
            assert!(
                stack.face_for(ch) > 0,
                "{ch:?} fell through to the primary face"
            );
        }
    }
}
