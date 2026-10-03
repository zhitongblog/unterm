//! Inline images for Unterm: the kitty graphics protocol, iTerm2 inline
//! images (`OSC 1337 ; File=`) and sixel.
//!
//! The terminal kernel owns the text; this owns the pictures beside it. A
//! picture is stored once, by what its pixels are, and *placed* any number of
//! times: a placement is an anchor -- an absolute row in the pane's history
//! and a column -- and the block of cells it covers. Absolute rows are what
//! the kernel already uses for prompt marks, so a placement scrolls with the
//! text and leaves with it when the scrollback is trimmed, without the 48-byte
//! cell having to carry anything.
//!
//! The kernel tells this crate where the cursor is and what happened to the
//! rows (trimmed, scrolled inside a region, cleared, the alternate screen came
//! and went); this crate tells the kernel what to answer and where the cursor
//! goes. It never touches the screen itself.

pub mod decode;
pub mod sixel;

use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine as _;
use decode::Decoded;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use wezterm_escape_parser::apc::{
    KittyImage, KittyImageData, KittyImageDelete, KittyImageFormat, KittyImagePlacement,
    KittyImageTransmit,
};

/// How many bytes of decoded pictures one pane keeps before it starts
/// forgetting the ones nothing shows any more.
pub const STORE_BUDGET_BYTES: usize = 320 * 1024 * 1024;

/// The longest side of the pixels handed to a front end.
pub const MAX_TEXTURE_SIDE: u32 = 4096;

/// How many placements one screen keeps. A program that places a picture in
/// a loop gets the newest ones.
pub const MAX_PLACEMENTS: usize = 1024;

/// The most base64 a chunked transfer may accumulate before it is dropped.
const MAX_PENDING_BYTES: usize = decode::MAX_ENCODED_BYTES / 3 * 4 + 16;

/// One cell's size in pixels, as the front end draws it: width and height
/// packed into one word so a reader never sees half of an update. Zero until
/// a front end says.
static CELL_PIXELS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The front end's cell size. Process-wide, like the colours it reports: a
/// headless kernel has no pixels, and every pane of one window shares a font.
pub fn set_cell_pixels(width: u32, height: u32) {
    let packed = (u64::from(width) << 32) | u64::from(height);
    CELL_PIXELS.store(packed, std::sync::atomic::Ordering::Relaxed);
}

/// The cell size a front end reported, if one has.
pub fn cell_pixels() -> Option<(u32, u32)> {
    let packed = CELL_PIXELS.load(std::sync::atomic::Ordering::Relaxed);
    let (width, height) = ((packed >> 32) as u32, packed as u32);
    (width > 0 && height > 0).then_some((width, height))
}

/// Which protocol a picture arrived by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageProtocol {
    Kitty,
    Iterm,
    Sixel,
}

/// How a picture fills the cells it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFit {
    /// As large as fits without changing its shape, from the top left.
    Contain,
    /// Stretched to the whole block, because the program named both sides.
    Fill,
}

/// One picture on the screen, as a front end draws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePlacementSnapshot {
    /// The picture's name; fetch its pixels with it, once, and cache them.
    pub image: String,
    /// Absolute history row of the top-left cell, in the same numbering as
    /// the rows of the lines it is drawn beside. May be above the viewport.
    pub row: i64,
    pub col: usize,
    /// The block of cells the picture covers.
    pub cols: usize,
    pub rows: usize,
    /// Stacking order among pictures; higher is in front.
    pub z: i32,
    /// The full picture's size in pixels.
    pub width: u32,
    pub height: u32,
    /// The part of the picture to show, `[x, y, width, height]` in pixels,
    /// when it is not all of it.
    #[serde(default)]
    pub source: Option<[u32; 4]>,
    pub fit: ImageFit,
    pub protocol: ImageProtocol,
    /// The file name the program gave, if any.
    #[serde(default)]
    pub name: Option<String>,
}

/// A picture's pixels, for a front end to upload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    pub image: String,
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA, rows top to bottom. Base64 on the wire.
    #[serde(with = "rgba_base64")]
    pub rgba: Vec<u8>,
}

mod rgba_base64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(serde::de::Error::custom)
    }
}

/// The pane, as far as placing a picture is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub cols: usize,
    pub rows: usize,
    /// One cell, in pixels. Pictures are measured in cells from this.
    pub cell_width: u32,
    pub cell_height: u32,
    /// Absolute row of the top line of the live screen.
    pub top_row: i64,
}

/// Where the cursor is: an absolute row and a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At {
    pub row: i64,
    pub col: usize,
}

/// Where the cursor goes after a picture: down this many lines, scrolling as
/// a line feed would, then to this column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorMove {
    pub down: usize,
    pub col: usize,
}

/// What a command asks of the kernel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Bytes to write back to the program.
    pub reply: Option<Vec<u8>>,
    pub cursor: Option<CursorMove>,
    /// Whether anything on screen changed, so the kernel can redraw.
    pub changed: bool,
}

#[derive(Clone, Debug)]
struct Placement {
    key: String,
    row: i64,
    col: usize,
    cols: usize,
    rows: usize,
    z: i32,
    source: Option<[u32; 4]>,
    fit: ImageFit,
    protocol: ImageProtocol,
    name: Option<String>,
    /// Kitty image id and placement id (0 for none).
    kitty: Option<(u32, u32)>,
    seq: u64,
}

impl Placement {
    fn covers(&self, row: i64, col: usize) -> bool {
        self.covers_row(row) && col >= self.col && col < self.col + self.cols
    }

    fn covers_row(&self, row: i64) -> bool {
        row >= self.row && row < self.row + self.rows as i64
    }

    fn overlaps(&self, from: i64, to: i64) -> bool {
        self.row <= to && self.row + self.rows as i64 > from
    }
}

struct Stored {
    data: Arc<Decoded>,
    used: u64,
}

struct Pending {
    keys: String,
    payload: String,
}

struct Multipart {
    args: String,
    data: String,
}

/// One pane's pictures.
#[derive(Default)]
pub struct Images {
    store: HashMap<String, Stored>,
    bytes: usize,
    clock: u64,
    kitty_ids: HashMap<u32, String>,
    kitty_numbers: HashMap<u32, u32>,
    next_kitty_id: u32,
    pending: Option<Pending>,
    multipart: Option<Multipart>,
    main: Vec<Placement>,
    /// The alternate screen's placements, while it is up.
    alternate: Option<Vec<Placement>>,
    /// A picture transmitted without an id, for the `a=T` that displays it
    /// in the same command.
    anonymous: Option<String>,
}

fn lenient_base64() -> GeneralPurpose {
    GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
}

fn base64_bytes(text: &str) -> Result<Vec<u8>, String> {
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    lenient_base64()
        .decode(compact)
        .map_err(|err| format!("the data is not base64: {err}"))
}

fn ceil_div(value: f64, by: u32) -> usize {
    ((value / f64::from(by.max(1))).ceil() as usize).max(1)
}

impl Images {
    pub fn new() -> Self {
        Self::default()
    }

    /// Nothing stored and nothing placed: the kernel can skip all of this.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
            && self.main.is_empty()
            && self.alternate.as_ref().is_none_or(Vec::is_empty)
    }

    fn list(&mut self) -> &mut Vec<Placement> {
        match self.alternate.as_mut() {
            Some(alternate) => alternate,
            None => &mut self.main,
        }
    }

    fn list_ref(&self) -> &Vec<Placement> {
        self.alternate.as_ref().unwrap_or(&self.main)
    }

    /// The pixels of a stored picture.
    pub fn data(&self, key: &str) -> Option<Arc<Decoded>> {
        self.store.get(key).map(|stored| stored.data.clone())
    }

    /// The pixels of a stored picture, as a front end fetches them: at most
    /// `MAX_TEXTURE_SIDE` on a side.
    ///
    /// A pane is never more than a few thousand pixels across, so the extra
    /// would only be shipped and uploaded to be thrown away -- and past 8192
    /// a texture is beyond what many GPUs accept at all. A front end places
    /// pictures by their full size and samples by proportion, so a smaller
    /// texture lands in exactly the same place.
    pub fn image_data(&self, key: &str) -> Option<ImageData> {
        let data = self.data(key)?;
        let (width, height, rgba) = decode::fit_within(&data, MAX_TEXTURE_SIDE);
        Some(ImageData {
            image: key.to_string(),
            width,
            height,
            rgba,
        })
    }

    fn keep(&mut self, decoded: Decoded) -> String {
        let key = decode::content_key(&decoded);
        self.clock += 1;
        if let Some(stored) = self.store.get_mut(&key) {
            stored.used = self.clock;
            return key;
        }
        self.bytes += decoded.bytes();
        self.store.insert(
            key.clone(),
            Stored {
                data: Arc::new(decoded),
                used: self.clock,
            },
        );
        self.collect(Some(&key));
        key
    }

    /// Forget the least recently used pictures nothing shows, until the
    /// store is back under budget.
    fn collect(&mut self, keep: Option<&str>) {
        if self.bytes <= STORE_BUDGET_BYTES {
            return;
        }
        let shown = self.referenced();
        let mut candidates: Vec<(u64, String)> = self
            .store
            .iter()
            .filter(|(key, _)| !shown.contains(key.as_str()) && Some(key.as_str()) != keep)
            .map(|(key, stored)| (stored.used, key.clone()))
            .collect();
        candidates.sort();
        for (_, key) in candidates {
            if self.bytes <= STORE_BUDGET_BYTES {
                break;
            }
            self.forget(&key);
        }
    }

    fn forget(&mut self, key: &str) {
        if let Some(stored) = self.store.remove(key) {
            self.bytes = self.bytes.saturating_sub(stored.data.bytes());
        }
        self.kitty_ids.retain(|_, stored| stored != key);
    }

    fn referenced(&self) -> HashSet<String> {
        self.main
            .iter()
            .chain(self.alternate.iter().flatten())
            .map(|placement| placement.key.clone())
            .collect()
    }

    fn place(&mut self, mut placement: Placement) {
        self.clock += 1;
        placement.seq = self.clock;
        let list = self.list();
        if let Some((id, pid)) = placement.kitty {
            if id != 0 && pid != 0 {
                list.retain(|other| other.kitty != Some((id, pid)));
            }
        }
        list.push(placement);
        if list.len() > MAX_PLACEMENTS {
            let excess = list.len() - MAX_PLACEMENTS;
            list.drain(..excess);
        }
    }

    // ------------------------------------------------------------------ rows

    /// Rows `0..removed` of the main screen's history are gone, and every
    /// later row moved up by that much.
    pub fn history_trimmed(&mut self, removed: usize) {
        if removed == 0 || self.main.is_empty() {
            return;
        }
        let removed = removed as i64;
        self.main.retain_mut(|placement| {
            placement.row -= removed;
            placement.row + placement.rows as i64 > 0
        });
    }

    /// Rows `top..=bottom` moved by `delta` inside a scroll region (or by an
    /// inserted or deleted line) without anything reaching the history.
    /// Pictures anchored in the region move with them and leave if they move
    /// out of it.
    pub fn region_scrolled(&mut self, top: i64, bottom: i64, delta: i64) {
        if delta == 0 {
            return;
        }
        self.list().retain_mut(|placement| {
            if placement.row < top || placement.row > bottom {
                return true;
            }
            placement.row += delta;
            placement.row >= top && placement.row <= bottom
        });
    }

    /// Rows `from..=to` were erased: the pictures on them go too.
    pub fn rows_cleared(&mut self, from: i64, to: i64) {
        self.list().retain(|placement| !placement.overlaps(from, to));
    }

    /// Every picture on the current screen goes.
    pub fn screen_cleared(&mut self) {
        self.list().clear();
    }

    /// The alternate screen came up: it starts with no pictures, and the
    /// main screen's wait for it.
    pub fn enter_alternate(&mut self) {
        self.alternate = Some(Vec::new());
    }

    /// The alternate screen went away, and its pictures with it.
    pub fn leave_alternate(&mut self) {
        self.alternate = None;
    }

    /// Everything: placements on both screens, partial transfers and the
    /// pictures themselves. A terminal reset.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    // -------------------------------------------------------------- snapshot

    fn snapshot(&self, placement: &Placement) -> Option<ImagePlacementSnapshot> {
        let data = self.store.get(&placement.key)?;
        Some(ImagePlacementSnapshot {
            image: placement.key.clone(),
            row: placement.row,
            col: placement.col,
            cols: placement.cols,
            rows: placement.rows,
            z: placement.z,
            width: data.data.width,
            height: data.data.height,
            source: placement.source,
            fit: placement.fit,
            protocol: placement.protocol,
            name: placement.name.clone(),
        })
    }

    /// The pictures that show on rows `first..first + rows`, back to front.
    pub fn visible(&self, first: i64, rows: usize) -> Vec<ImagePlacementSnapshot> {
        let last = first + rows as i64 - 1;
        let mut shown: Vec<&Placement> = self
            .list_ref()
            .iter()
            .filter(|placement| placement.overlaps(first, last))
            .collect();
        shown.sort_by_key(|placement| (placement.z, placement.seq));
        shown
            .into_iter()
            .filter_map(|placement| self.snapshot(placement))
            .collect()
    }

    /// Every picture on the current screen, scrollback included.
    pub fn all(&self) -> Vec<ImagePlacementSnapshot> {
        let mut all: Vec<&Placement> = self.list_ref().iter().collect();
        all.sort_by_key(|placement| (placement.row, placement.seq));
        all.into_iter()
            .filter_map(|placement| self.snapshot(placement))
            .collect()
    }

    // ---------------------------------------------------------------- placing

    fn place_decoded(
        &mut self,
        decoded: Decoded,
        protocol: ImageProtocol,
        name: Option<String>,
        size: (usize, usize),
        fit: ImageFit,
        at: At,
        geo: Geometry,
    ) -> CursorMove {
        let key = self.keep(decoded);
        let (cols, rows) = size;
        self.place(Placement {
            key,
            row: at.row,
            col: at.col,
            cols,
            rows,
            z: 0,
            source: None,
            fit,
            protocol,
            name,
            kitty: None,
            seq: 0,
        });
        CursorMove {
            down: rows.saturating_sub(1),
            col: (at.col + cols).min(geo.cols.saturating_sub(1)),
        }
    }

    /// The natural size of `width` by `height` pixels in cells, never wider
    /// than the screen.
    fn natural_cells(width: f64, height: f64, geo: Geometry) -> (usize, usize) {
        let screen = (geo.cols as f64) * f64::from(geo.cell_width);
        let scale = if width > screen && screen > 0.0 {
            screen / width
        } else {
            1.0
        };
        (
            ceil_div(width * scale, geo.cell_width).min(geo.cols.max(1)),
            ceil_div(height * scale, geo.cell_height),
        )
    }

    // ----------------------------------------------------------------- sixel

    /// `DCS … q … ST`: paint it at the cursor.
    pub fn sixel(&mut self, body: &str, at: At, geo: Geometry) -> Outcome {
        let decoded = match sixel::from_dcs(body) {
            Ok(decoded) => decoded,
            Err(_) => return Outcome::default(),
        };
        let size =
            Self::natural_cells(f64::from(decoded.width), f64::from(decoded.height), geo);
        let mut cursor =
            self.place_decoded(decoded, ImageProtocol::Sixel, None, size, ImageFit::Contain, at, geo);
        // Sixel moves the cursor down, not across.
        cursor.col = at.col;
        Outcome {
            reply: None,
            cursor: Some(cursor),
            changed: true,
        }
    }

    // ---------------------------------------------------------------- iTerm2

    /// Whether an OSC 1337 body is one of the image commands handled here.
    pub fn is_iterm_image(body: &str) -> bool {
        ["File=", "MultipartFile=", "FilePart=", "FileEnd"]
            .iter()
            .any(|prefix| body.starts_with(prefix))
    }

    /// `OSC 1337 ; File=…:data`, and its multipart form.
    pub fn iterm(&mut self, body: &str, at: At, geo: Geometry) -> Outcome {
        if let Some(rest) = body.strip_prefix("File=") {
            let (args, data) = rest.split_once(':').unwrap_or((rest, ""));
            return self.iterm_file(args, data, at, geo);
        }
        if let Some(args) = body.strip_prefix("MultipartFile=") {
            self.multipart = Some(Multipart {
                args: args.to_string(),
                data: String::new(),
            });
            return Outcome::default();
        }
        if let Some(part) = body.strip_prefix("FilePart=") {
            let overflow = match self.multipart.as_mut() {
                Some(multipart) => {
                    multipart.data.push_str(part);
                    multipart.data.len() > MAX_PENDING_BYTES
                }
                None => false,
            };
            if overflow {
                self.multipart = None;
            }
            return Outcome::default();
        }
        if body.starts_with("FileEnd") {
            if let Some(multipart) = self.multipart.take() {
                return self.iterm_file(&multipart.args, &multipart.data, at, geo);
            }
        }
        Outcome::default()
    }

    fn iterm_file(&mut self, args: &str, data: &str, at: At, geo: Geometry) -> Outcome {
        let args: HashMap<&str, &str> = args
            .split(';')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        // Without inline=1 iTerm2 downloads the file; a terminal has nowhere
        // to put it, so it is not shown either.
        if args.get("inline").copied() != Some("1") {
            return Outcome::default();
        }
        let Ok(bytes) = base64_bytes(data) else {
            return Outcome::default();
        };
        let Ok(decoded) = decode::encoded(&bytes) else {
            return Outcome::default();
        };
        let name = args
            .get("name")
            .and_then(|name| base64_bytes(name).ok())
            .and_then(|name| String::from_utf8(name).ok());
        let preserve = args.get("preserveAspectRatio").copied() != Some("0");
        let width = Self::iterm_dimension(args.get("width").copied(), geo.cols, geo.cell_width);
        let height = Self::iterm_dimension(args.get("height").copied(), geo.rows, geo.cell_height);
        let (iw, ih) = (f64::from(decoded.width), f64::from(decoded.height));
        let (w, h) = match (width, height) {
            (None, None) => (iw, ih),
            (Some(w), None) => (w, if preserve { w * ih / iw } else { ih }),
            (None, Some(h)) => (if preserve { h * iw / ih } else { iw }, h),
            (Some(w), Some(h)) => (w, h),
        };
        let size = Self::natural_cells(w, h, geo);
        let fit = if preserve {
            ImageFit::Contain
        } else {
            ImageFit::Fill
        };
        let do_not_move = args.get("doNotMoveCursor").copied() == Some("1");
        let cursor = self.place_decoded(decoded, ImageProtocol::Iterm, name, size, fit, at, geo);
        Outcome {
            reply: None,
            cursor: (!do_not_move).then_some(cursor),
            changed: true,
        }
    }

    /// An iTerm2 width or height in pixels: `N` cells, `Npx`, `N%` of the
    /// screen, or `auto` (None).
    fn iterm_dimension(spec: Option<&str>, cells: usize, cell_px: u32) -> Option<f64> {
        let spec = spec?.trim();
        if spec.is_empty() || spec.eq_ignore_ascii_case("auto") {
            return None;
        }
        let screen = cells as f64 * f64::from(cell_px);
        if let Some(px) = spec.strip_suffix("px") {
            px.parse::<f64>().ok().filter(|v| *v > 0.0)
        } else if let Some(percent) = spec.strip_suffix('%') {
            percent
                .parse::<f64>()
                .ok()
                .filter(|v| *v > 0.0)
                .map(|v| screen * v / 100.0)
        } else {
            spec.parse::<f64>()
                .ok()
                .filter(|v| *v > 0.0)
                .map(|v| v * f64::from(cell_px))
        }
    }

    // ----------------------------------------------------------------- kitty

    /// `APC G … ST`: the kitty graphics protocol. `apc` is the whole body,
    /// starting with the `G`.
    pub fn kitty(&mut self, apc: &str, at: At, geo: Geometry) -> Outcome {
        let body = apc.strip_prefix('G').unwrap_or(apc);
        let (keys, payload) = body.split_once(';').unwrap_or((body, ""));
        let map: Vec<(&str, &str)> = keys
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        let more = map.iter().any(|(k, v)| *k == "m" && *v == "1");
        let only_chunk_keys = map.iter().all(|(k, _)| *k == "m" || *k == "q");

        // A continuation of a chunked transfer carries only `m` (and `q`).
        if self.pending.is_some() && only_chunk_keys {
            let done = {
                let pending = self.pending.as_mut().expect("checked above");
                pending.payload.push_str(payload);
                if pending.payload.len() > MAX_PENDING_BYTES {
                    self.pending = None;
                    return Outcome::default();
                }
                !more
            };
            if !done {
                return Outcome::default();
            }
            let pending = self.pending.take().expect("checked above");
            return self.kitty_command(&pending.keys, &pending.payload, at, geo);
        }
        // Anything else abandons a transfer that never finished.
        self.pending = None;

        let keys_without_m: String = map
            .iter()
            .filter(|(k, _)| *k != "m")
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        if more {
            self.pending = Some(Pending {
                keys: keys_without_m,
                payload: payload.to_string(),
            });
            return Outcome::default();
        }
        self.kitty_command(&keys_without_m, payload, at, geo)
    }

    fn kitty_command(&mut self, keys: &str, payload: &str, at: At, geo: Geometry) -> Outcome {
        let map: HashMap<&str, &str> = keys
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        let reply_to = KittyReply::from_keys(&map);

        if map.get("U").copied() == Some("1") {
            return reply_to.error("EINVAL:Unicode placeholders are not supported");
        }
        if ["P", "Q", "H", "V"].iter().any(|key| map.contains_key(key)) {
            return reply_to.error("EINVAL:relative placements are not supported");
        }

        let raw = format!("G{keys};{payload}");
        let Some(command) = KittyImage::parse_apc(raw.as_bytes()) else {
            return reply_to.error("EINVAL:could not parse the command");
        };

        match command {
            KittyImage::Query { transmit } => match load(&transmit) {
                Ok(_) => reply_to.ok(None),
                Err(err) => reply_to.error(&err),
            },
            KittyImage::TransmitData { transmit, .. } => match load(&transmit) {
                Ok(decoded) => {
                    let id = self.register(&transmit, decoded);
                    reply_to.ok(id)
                }
                Err(err) => reply_to.error(&err),
            },
            KittyImage::TransmitDataAndDisplay {
                transmit,
                placement,
                ..
            } => match load(&transmit) {
                Ok(decoded) => {
                    let id = self.register(&transmit, decoded);
                    let key = match id {
                        Some(id) => self.kitty_ids.get(&id).cloned(),
                        None => self.anonymous.take(),
                    };
                    let Some(key) = key else {
                        return reply_to.error("ENOENT:the image was not stored");
                    };
                    let mut outcome = reply_to.ok(id);
                    outcome.cursor = self.kitty_display(&key, id.unwrap_or(0), &placement, at, geo);
                    outcome.changed = true;
                    outcome
                }
                Err(err) => reply_to.error(&err),
            },
            KittyImage::Display {
                image_id,
                image_number,
                placement,
                ..
            } => {
                let id = image_id.or_else(|| {
                    image_number.and_then(|number| self.kitty_numbers.get(&number).copied())
                });
                let Some((id, key)) =
                    id.and_then(|id| self.kitty_ids.get(&id).cloned().map(|key| (id, key)))
                else {
                    return reply_to.error("ENOENT:no image with that id");
                };
                let mut outcome = reply_to.ok(Some(id));
                outcome.cursor = self.kitty_display(&key, id, &placement, at, geo);
                outcome.changed = true;
                outcome
            }
            KittyImage::Delete { what, .. } => {
                let changed = self.kitty_delete(what, at, geo);
                Outcome {
                    reply: None,
                    cursor: None,
                    changed,
                }
            }
            KittyImage::TransmitFrame { .. } | KittyImage::ComposeFrame { .. } => {
                reply_to.error("EINVAL:animation is not supported")
            }
        }
    }

    /// Store a transmitted picture under its id. Returns the id the program
    /// can refer to it by, if it asked for one.
    fn register(&mut self, transmit: &KittyImageTransmit, decoded: Decoded) -> Option<u32> {
        let key = self.keep(decoded);
        let id = match (transmit.image_id, transmit.image_number) {
            (Some(id), _) if id != 0 => Some(id),
            (_, Some(number)) => {
                let id = self.fresh_kitty_id();
                self.kitty_numbers.insert(number, id);
                Some(id)
            }
            _ => None,
        };
        match id {
            Some(id) => {
                self.kitty_ids.insert(id, key);
            }
            None => self.anonymous = Some(key),
        }
        id
    }

    fn fresh_kitty_id(&mut self) -> u32 {
        loop {
            // Counting down from the top keeps out of the way of the small
            // ids programs choose for themselves.
            self.next_kitty_id = self.next_kitty_id.wrapping_sub(1);
            if self.next_kitty_id != 0 && !self.kitty_ids.contains_key(&self.next_kitty_id) {
                return self.next_kitty_id;
            }
        }
    }

    fn kitty_display(
        &mut self,
        key: &str,
        id: u32,
        placement: &KittyImagePlacement,
        at: At,
        geo: Geometry,
    ) -> Option<CursorMove> {
        let data = self.data(key)?;
        let (iw, ih) = (data.width, data.height);
        let sx = placement.x.unwrap_or(0).min(iw.saturating_sub(1));
        let sy = placement.y.unwrap_or(0).min(ih.saturating_sub(1));
        let sw = placement.w.filter(|w| *w > 0).unwrap_or(iw - sx).min(iw - sx);
        let sh = placement.h.filter(|h| *h > 0).unwrap_or(ih - sy).min(ih - sy);
        let source = ((sx, sy, sw, sh) != (0, 0, iw, ih)).then_some([sx, sy, sw, sh]);
        let (cw, ch) = (f64::from(geo.cell_width.max(1)), f64::from(geo.cell_height.max(1)));
        let (sw_f, sh_f) = (f64::from(sw.max(1)), f64::from(sh.max(1)));
        let (cols, rows, fit) = match (placement.columns, placement.rows) {
            (Some(c), Some(r)) if c > 0 && r > 0 => (c as usize, r as usize, ImageFit::Fill),
            (Some(c), _) if c > 0 => (
                c as usize,
                ((f64::from(c) * cw * sh_f / sw_f / ch).ceil() as usize).max(1),
                ImageFit::Contain,
            ),
            (_, Some(r)) if r > 0 => (
                ((f64::from(r) * ch * sw_f / sh_f / cw).ceil() as usize).max(1),
                r as usize,
                ImageFit::Contain,
            ),
            _ => {
                let (cols, rows) = Self::natural_cells(sw_f, sh_f, geo);
                (cols, rows, ImageFit::Contain)
            }
        };
        self.place(Placement {
            key: key.to_string(),
            row: at.row,
            col: at.col,
            cols,
            rows,
            z: placement.z_index.unwrap_or(0),
            source,
            fit,
            protocol: ImageProtocol::Kitty,
            name: None,
            kitty: Some((id, placement.placement_id.unwrap_or(0))),
            seq: 0,
        });
        (!placement.do_not_move_cursor).then(|| CursorMove {
            down: rows.saturating_sub(1),
            col: (at.col + cols).min(geo.cols.saturating_sub(1)),
        })
    }

    fn kitty_delete(&mut self, what: KittyImageDelete, at: At, geo: Geometry) -> bool {
        let screen = (geo.top_row, geo.top_row + geo.rows as i64 - 1);
        let cell = |x: u32, y: u32| (geo.top_row + i64::from(y) - 1, (x as usize).saturating_sub(1));
        let (matches, free): (Box<dyn Fn(&Placement) -> bool>, bool) = match what {
            KittyImageDelete::All { delete } => {
                (Box::new(move |p: &Placement| p.overlaps(screen.0, screen.1)), delete)
            }
            KittyImageDelete::ByImageId {
                image_id,
                placement_id,
                delete,
            } => (
                Box::new(move |p: &Placement| {
                    p.kitty.is_some_and(|(id, pid)| {
                        id == image_id && placement_id.is_none_or(|want| want == pid)
                    })
                }),
                delete,
            ),
            KittyImageDelete::ByImageNumber {
                image_number,
                placement_id,
                delete,
            } => {
                let image_id = self.kitty_numbers.get(&image_number).copied().unwrap_or(0);
                (
                    Box::new(move |p: &Placement| {
                        image_id != 0
                            && p.kitty.is_some_and(|(id, pid)| {
                                id == image_id && placement_id.is_none_or(|want| want == pid)
                            })
                    }),
                    delete,
                )
            }
            KittyImageDelete::AtCursorPosition { delete } => {
                (Box::new(move |p: &Placement| p.covers(at.row, at.col)), delete)
            }
            KittyImageDelete::DeleteAt { x, y, delete } => {
                let (row, col) = cell(x, y);
                (Box::new(move |p: &Placement| p.covers(row, col)), delete)
            }
            KittyImageDelete::DeleteAtZ { x, y, z, delete } => {
                let (row, col) = cell(x, y);
                (
                    Box::new(move |p: &Placement| p.z == z && p.covers(row, col)),
                    delete,
                )
            }
            KittyImageDelete::DeleteColumn { x, delete } => {
                let col = (x as usize).saturating_sub(1);
                (
                    Box::new(move |p: &Placement| col >= p.col && col < p.col + p.cols),
                    delete,
                )
            }
            KittyImageDelete::DeleteRow { y, delete } => {
                let row = geo.top_row + i64::from(y) - 1;
                (Box::new(move |p: &Placement| p.covers_row(row)), delete)
            }
            KittyImageDelete::DeleteZ { z, delete } => {
                (Box::new(move |p: &Placement| p.z == z), delete)
            }
            KittyImageDelete::AnimationFrames { .. } => return false,
        };

        let mut removed_keys = Vec::new();
        let list = self.list();
        let before = list.len();
        list.retain(|placement| {
            let gone = placement.protocol == ImageProtocol::Kitty && matches(placement);
            if gone {
                removed_keys.push((placement.key.clone(), placement.kitty));
            }
            !gone
        });
        let changed = list.len() != before;

        if free {
            let shown = self.referenced();
            for (key, kitty) in removed_keys {
                if let Some((id, _)) = kitty {
                    self.kitty_ids.remove(&id);
                }
                if !shown.contains(&key) {
                    self.forget(&key);
                }
            }
        }
        changed
    }
}

/// Who a kitty reply goes to, and how much the program wants to hear.
struct KittyReply {
    image_id: Option<u32>,
    image_number: Option<u32>,
    placement_id: Option<u32>,
    /// `q=1` keeps OK quiet; `q=2` keeps everything quiet.
    quiet: u8,
}

impl KittyReply {
    fn from_keys(map: &HashMap<&str, &str>) -> Self {
        let number = |key: &str| map.get(key).and_then(|v| v.parse::<u32>().ok());
        Self {
            image_id: number("i").filter(|id| *id != 0),
            image_number: number("I"),
            placement_id: number("p"),
            quiet: map.get("q").and_then(|v| v.parse().ok()).unwrap_or(0),
        }
    }

    /// No id and no number: the protocol says nothing is sent back.
    fn addressed(&self) -> bool {
        self.image_id.is_some() || self.image_number.is_some()
    }

    fn message(&self, id: Option<u32>, text: &str) -> Vec<u8> {
        let mut keys = Vec::new();
        if let Some(id) = id.or(self.image_id) {
            keys.push(format!("i={id}"));
        }
        if let Some(number) = self.image_number {
            keys.push(format!("I={number}"));
        }
        if let Some(pid) = self.placement_id {
            keys.push(format!("p={pid}"));
        }
        format!("\x1b_G{};{text}\x1b\\", keys.join(",")).into_bytes()
    }

    fn ok(&self, id: Option<u32>) -> Outcome {
        let reply = (self.addressed() && self.quiet == 0).then(|| self.message(id, "OK"));
        Outcome {
            reply,
            cursor: None,
            changed: false,
        }
    }

    fn error(&self, text: &str) -> Outcome {
        let reply = (self.addressed() && self.quiet < 2).then(|| self.message(None, text));
        Outcome {
            reply,
            cursor: None,
            changed: false,
        }
    }
}

/// The bytes of a kitty transmission, decoded into pixels.
fn load(transmit: &KittyImageTransmit) -> Result<Decoded, String> {
    let bytes = match &transmit.data {
        KittyImageData::Direct(text) => {
            base64_bytes(text).map_err(|err| format!("EINVAL:{err}"))?
        }
        KittyImageData::DirectBin(bytes) => bytes.clone(),
        KittyImageData::File {
            path,
            data_offset,
            data_size,
        } => read_file(path, *data_offset, *data_size)?,
        KittyImageData::TemporaryFile {
            path,
            data_offset,
            data_size,
        } => {
            let bytes = read_file(path, *data_offset, *data_size)?;
            if looks_like_a_temporary_file(path) {
                let _ = std::fs::remove_file(path);
            }
            bytes
        }
        KittyImageData::SharedMem { .. } => {
            return Err("EINVAL:shared memory is not supported".to_string())
        }
    };
    let bytes = match transmit.compression {
        wezterm_escape_parser::apc::KittyImageCompression::None => bytes,
        wezterm_escape_parser::apc::KittyImageCompression::Deflate => {
            decode::inflate(&bytes).map_err(|err| format!("EINVAL:{err}"))?
        }
    };
    let decoded = match transmit.format {
        Some(KittyImageFormat::Png) => decode::encoded(&bytes),
        Some(KittyImageFormat::Rgb) => decode::raw(
            &bytes,
            transmit.width.unwrap_or(0),
            transmit.height.unwrap_or(0),
            3,
        ),
        Some(KittyImageFormat::Rgba) | None => decode::raw(
            &bytes,
            transmit.width.unwrap_or(0),
            transmit.height.unwrap_or(0),
            4,
        ),
    };
    decoded.map_err(|err| format!("EBADF:{err}"))
}

fn read_file(path: &str, offset: Option<u32>, size: Option<u32>) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let metadata =
        std::fs::metadata(path).map_err(|err| format!("ENOENT:could not open {path}: {err}"))?;
    // A device or a pipe is not a picture, and reading /dev/zero to the end
    // never finishes.
    if !metadata.is_file() {
        return Err(format!("EINVAL:{path} is not a regular file"));
    }
    let available = metadata.len().saturating_sub(u64::from(offset.unwrap_or(0)));
    let wanted = size.map(u64::from).unwrap_or(available).min(available);
    if wanted > decode::MAX_ENCODED_BYTES as u64 {
        return Err(format!("EFBIG:{path} is too large"));
    }
    let mut file =
        std::fs::File::open(path).map_err(|err| format!("ENOENT:could not open {path}: {err}"))?;
    if let Some(offset) = offset {
        file.seek(SeekFrom::Start(u64::from(offset)))
            .map_err(|err| format!("EINVAL:{err}"))?;
    }
    let mut bytes = Vec::with_capacity(wanted as usize);
    file.take(wanted)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("EIO:{err}"))?;
    Ok(bytes)
}

/// Only files the protocol says may be deleted are deleted: in a temporary
/// directory, with the protocol's marker in the name.
fn looks_like_a_temporary_file(path: &str) -> bool {
    if !path.contains("tty-graphics-protocol") {
        return false;
    }
    let path = std::path::Path::new(path);
    let temp = std::env::temp_dir();
    ["/tmp", "/var/tmp", "/dev/shm"]
        .iter()
        .map(std::path::Path::new)
        .chain(std::iter::once(temp.as_path()))
        .any(|dir| path.starts_with(dir))
}

#[cfg(test)]
mod tests;
