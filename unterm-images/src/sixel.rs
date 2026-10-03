//! Sixel: DEC's bitmap format, as `DCS … q … ST`.
//!
//! The escape parser already turns the bytes into commands -- pick a colour,
//! paint a column of six pixels, repeat it, return, go down a band. Nothing
//! turned those into a picture; this does.

use crate::decode::{Decoded, MAX_PIXELS};
use wezterm_escape_parser::parser::Parser;
use wezterm_escape_parser::{Action, Sixel, SixelData};

/// How many colour registers a picture may use. xterm offers 1024.
const REGISTERS: usize = 1024;

/// The VT340's sixteen colours, which is what a picture that never defines
/// a register paints with. Percentages, as the terminal defined them.
const VT340: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (20, 20, 80),
    (80, 13, 13),
    (20, 80, 20),
    (80, 20, 80),
    (20, 80, 80),
    (80, 80, 20),
    (53, 53, 53),
    (26, 26, 26),
    (33, 33, 60),
    (60, 26, 26),
    (33, 60, 33),
    (60, 33, 60),
    (33, 60, 60),
    (60, 60, 33),
    (80, 80, 80),
];

fn percent(value: u8) -> u8 {
    ((u16::from(value.min(100)) * 255 + 50) / 100) as u8
}

/// DEC's HLS puts blue at 0 degrees, red at 120 and green at 240.
fn hls(hue_angle: u16, lightness: u8, saturation: u8) -> [u8; 3] {
    let hue = (f32::from(hue_angle % 360) + 240.0) % 360.0 / 360.0;
    let l = f32::from(lightness.min(100)) / 100.0;
    let s = f32::from(saturation.min(100)) / 100.0;
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    [
        channel(hue + 1.0 / 3.0),
        channel(hue),
        channel(hue - 1.0 / 3.0),
    ]
}

/// The extent the data actually paints, whatever the header claimed.
fn painted_size(data: &[SixelData]) -> (u64, u64) {
    let (mut x, mut band, mut max_x, mut max_y) = (0u64, 0u64, 0u64, 0u64);
    for item in data {
        match item {
            SixelData::Data(_) => {
                x += 1;
                max_x = max_x.max(x);
                max_y = max_y.max(band * 6 + 6);
            }
            SixelData::Repeat { repeat_count, .. } => {
                x += u64::from(*repeat_count);
                max_x = max_x.max(x);
                max_y = max_y.max(band * 6 + 6);
            }
            SixelData::CarriageReturn => x = 0,
            SixelData::NewLine => {
                x = 0;
                band += 1;
            }
            _ => {}
        }
    }
    (max_x, max_y)
}

/// Paint a parsed sixel image.
pub fn rasterize(sixel: &Sixel) -> Result<Decoded, String> {
    let (painted_w, painted_h) = painted_size(&sixel.data);
    // Raster attributes are a promise about the size; the data is the truth.
    // A header that undersells is common, so the larger of the two wins.
    let width = painted_w.max(u64::from(sixel.pixel_width.unwrap_or(0)));
    let height = painted_h.max(u64::from(sixel.pixel_height.unwrap_or(0)));
    if width == 0 || height == 0 {
        return Err("the sixel image paints nothing".to_string());
    }
    if width * height > MAX_PIXELS {
        return Err(format!("{width}x{height} sixel image is too large"));
    }
    let (width, height) = (width as usize, height as usize);

    let mut palette = vec![[0u8, 0, 0]; REGISTERS];
    for (index, (r, g, b)) in VT340.iter().enumerate() {
        palette[index] = [percent(*r), percent(*g), percent(*b)];
    }
    // Pixels nothing painted stay transparent, so the picture sits on the
    // pane's own background whatever the header said about it.
    let mut rgba = vec![0u8; width * height * 4];
    let (mut x, mut band, mut color) = (0usize, 0usize, 0usize);

    let paint = |x: usize, band: usize, bits: u8, color: [u8; 3], rgba: &mut [u8]| {
        if x >= width {
            return;
        }
        for bit in 0..6 {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let y = band * 6 + bit;
            if y >= height {
                break;
            }
            let at = (y * width + x) * 4;
            rgba[at..at + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
        }
    };

    for item in &sixel.data {
        match item {
            SixelData::Data(bits) => {
                paint(x, band, *bits, palette[color], &mut rgba);
                x += 1;
            }
            SixelData::Repeat { repeat_count, data } => {
                for _ in 0..*repeat_count {
                    if x >= width {
                        break;
                    }
                    paint(x, band, *data, palette[color], &mut rgba);
                    x += 1;
                }
            }
            SixelData::DefineColorMapRGB { color_number, rgb } => {
                let index = usize::from(*color_number) % REGISTERS;
                let (r, g, b) = rgb.to_tuple_rgb8();
                palette[index] = [r, g, b];
                color = index;
            }
            SixelData::DefineColorMapHSL {
                color_number,
                hue_angle,
                lightness,
                saturation,
            } => {
                let index = usize::from(*color_number) % REGISTERS;
                palette[index] = hls(*hue_angle, *lightness, *saturation);
                color = index;
            }
            SixelData::SelectColorMapEntry(index) => color = usize::from(*index) % REGISTERS,
            SixelData::CarriageReturn => x = 0,
            SixelData::NewLine => {
                x = 0;
                band += 1;
            }
        }
    }

    Ok(Decoded {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

/// Parse and paint a sixel image from the body of its DCS: everything
/// between `ESC P` and the string terminator.
pub fn from_dcs(body: &str) -> Result<Decoded, String> {
    let mut bytes = Vec::with_capacity(body.len() + 4);
    bytes.extend_from_slice(b"\x1bP");
    bytes.extend_from_slice(body.as_bytes());
    bytes.extend_from_slice(b"\x1b\\");
    let mut found = None;
    Parser::new().parse(&bytes, |action| {
        if let Action::Sixel(sixel) = action {
            found = Some(sixel);
        }
    });
    let sixel = found.ok_or_else(|| "not a sixel image".to_string())?;
    rasterize(&sixel)
}

/// Whether a DCS body is a sixel image: numeric parameters, then `q`.
pub fn is_sixel(body: &str) -> bool {
    body.char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == ';'))
        .is_some_and(|(_, c)| c == 'q')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(decoded: &Decoded, x: usize, y: usize) -> [u8; 4] {
        let at = (y * decoded.width as usize + x) * 4;
        decoded.rgba[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn a_sixel_body_is_recognised_and_other_dcs_is_not() {
        assert!(is_sixel("q#0;2;100;0;0~"));
        assert!(is_sixel("0;1;0q~"));
        assert!(!is_sixel("+q544e"));
        assert!(!is_sixel("tmux;\x1b"));
    }

    #[test]
    fn a_red_column_of_six_pixels() {
        // Register 1 defined as pure red in RGB percent, then one sixel with
        // all six bits set ('~' is 63 + 63).
        let decoded = from_dcs("q#1;2;100;0;0#1~").unwrap();

        assert_eq!((decoded.width, decoded.height), (1, 6));
        for y in 0..6 {
            assert_eq!(pixel(&decoded, 0, y), [255, 0, 0, 255]);
        }
    }

    #[test]
    fn repeats_newlines_and_unpainted_bits() {
        // Green, three columns of the top pixel only ('@' is bit 0), then a
        // new band with one full column.
        let decoded = from_dcs("q#2;2;0;100;0#2!3@-~").unwrap();

        assert_eq!((decoded.width, decoded.height), (3, 12));
        assert_eq!(pixel(&decoded, 2, 0), [0, 255, 0, 255]);
        // Bit 1 was never painted: transparent.
        assert_eq!(pixel(&decoded, 2, 1), [0, 0, 0, 0]);
        // The second band starts at row 6.
        assert_eq!(pixel(&decoded, 0, 6), [0, 255, 0, 255]);
        assert_eq!(pixel(&decoded, 1, 6), [0, 0, 0, 0]);
    }

    #[test]
    fn hls_puts_red_at_120_degrees() {
        assert_eq!(hls(120, 50, 100), [255, 0, 0]);
        assert_eq!(hls(0, 50, 100), [0, 0, 255]);
        assert_eq!(hls(240, 50, 100), [0, 255, 0]);
    }

    #[test]
    fn raster_attributes_set_the_size_even_where_nothing_is_painted() {
        let decoded = from_dcs("q\"1;1;4;12#0~").unwrap();
        assert_eq!((decoded.width, decoded.height), (4, 12));
    }
}
