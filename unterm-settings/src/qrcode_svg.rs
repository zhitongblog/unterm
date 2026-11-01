//! Minimal, zero-dependency ISO/IEC 18004 QR Code SVG generator (Byte mode, ECC Level L, Versions 1..=6).
//! Supports payloads up to 134 bytes, ideal for LAN/Tailscale pairing URLs and URIs.

struct VersionSpec {
    size: usize,
    data_codewords: usize,
    ec_codewords_per_block: usize,
    num_blocks: usize,
    align_centers: &'static [usize],
}

const VERSIONS: &[VersionSpec] = &[
    VersionSpec {
        size: 21,
        data_codewords: 19,
        ec_codewords_per_block: 7,
        num_blocks: 1,
        align_centers: &[],
    },
    VersionSpec {
        size: 25,
        data_codewords: 34,
        ec_codewords_per_block: 10,
        num_blocks: 1,
        align_centers: &[6, 18],
    },
    VersionSpec {
        size: 29,
        data_codewords: 55,
        ec_codewords_per_block: 15,
        num_blocks: 1,
        align_centers: &[6, 22],
    },
    VersionSpec {
        size: 33,
        data_codewords: 80,
        ec_codewords_per_block: 20,
        num_blocks: 1,
        align_centers: &[6, 26],
    },
    VersionSpec {
        size: 37,
        data_codewords: 108,
        ec_codewords_per_block: 26,
        num_blocks: 1,
        align_centers: &[6, 30],
    },
    VersionSpec {
        size: 41,
        data_codewords: 136,
        ec_codewords_per_block: 18,
        num_blocks: 2,
        align_centers: &[6, 34],
    },
];

/// Render `text` as a crisp, standalone `<svg>` QR code string.
pub fn render_svg(text: &str) -> String {
    let bytes = text.as_bytes();
    let spec = VERSIONS
        .iter()
        .find(|v| bytes.len() + 2 <= v.data_codewords)
        .unwrap_or_else(|| VERSIONS.last().unwrap());
    let payload = if bytes.len() + 2 > spec.data_codewords {
        &bytes[..spec.data_codewords - 2]
    } else {
        bytes
    };

    let codewords = encode_codewords(payload, spec);
    let matrix = build_matrix(spec, &codewords);
    let n = spec.size;
    let quiet = 3usize;
    let total = n + quiet * 2;

    let mut path = String::with_capacity(n * n * 12);
    for r in 0..n {
        for c in 0..n {
            if matrix[r][c] {
                let x = c + quiet;
                let y = r + quiet;
                path.push_str(&format!("M{x},{y}h1v1h-1z"));
            }
        }
    }

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {total} {total}" shape-rendering="crispEdges" width="100%" height="100%"><rect width="{total}" height="{total}" rx="2" fill="#ffffff"/><path d="{path}" fill="#111111"/></svg>"##
    )
}

fn encode_codewords(payload: &[u8], spec: &VersionSpec) -> Vec<u8> {
    let mut bits: Vec<bool> = Vec::with_capacity(spec.data_codewords * 8);
    let push_bits = |bits: &mut Vec<bool>, val: usize, len: usize| {
        for i in (0..len).rev() {
            bits.push(((val >> i) & 1) != 0);
        }
    };
    // Byte mode indicator: 0100
    push_bits(&mut bits, 0b0100, 4);
    // Character count indicator (8 bits for versions 1..=9 in Byte mode)
    push_bits(&mut bits, payload.len(), 8);
    for &b in payload {
        push_bits(&mut bits, b as usize, 8);
    }
    // Terminator (up to 4 zero bits)
    let max_bits = spec.data_codewords * 8;
    let term = (max_bits - bits.len()).min(4);
    push_bits(&mut bits, 0, term);
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut data = Vec::with_capacity(spec.data_codewords);
    for chunk in bits.chunks(8) {
        let mut byte = 0u8;
        for &bit in chunk {
            byte = (byte << 1) | u8::from(bit);
        }
        data.push(byte);
    }
    let pad_bytes = [0xECu8, 0x11u8];
    let mut pad_idx = 0;
    while data.len() < spec.data_codewords {
        data.push(pad_bytes[pad_idx % 2]);
        pad_idx += 1;
    }

    if spec.num_blocks == 1 {
        let ec = rs_encode(&data, spec.ec_codewords_per_block);
        data.extend_from_slice(&ec);
        data
    } else {
        let block_len = spec.data_codewords / spec.num_blocks;
        let mut data_blocks = Vec::with_capacity(spec.num_blocks);
        let mut ec_blocks = Vec::with_capacity(spec.num_blocks);
        for b in 0..spec.num_blocks {
            let slice = &data[b * block_len..(b + 1) * block_len];
            data_blocks.push(slice.to_vec());
            ec_blocks.push(rs_encode(slice, spec.ec_codewords_per_block));
        }
        let mut interleaved =
            Vec::with_capacity(spec.data_codewords + spec.num_blocks * spec.ec_codewords_per_block);
        for i in 0..block_len {
            for b in 0..spec.num_blocks {
                interleaved.push(data_blocks[b][i]);
            }
        }
        for i in 0..spec.ec_codewords_per_block {
            for b in 0..spec.num_blocks {
                interleaved.push(ec_blocks[b][i]);
            }
        }
        interleaved
    }
}

fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if (b & 1) != 0 {
            p ^= a;
        }
        let hi = (a & 0x80) != 0;
        a <<= 1;
        if hi {
            a ^= 0x1D;
        }
        b >>= 1;
    }
    p
}

fn gf_pow(x: u8, power: usize) -> u8 {
    let mut res = 1u8;
    for _ in 0..power {
        res = gf_mul(res, x);
    }
    res
}

fn rs_generator_poly(degree: usize) -> Vec<u8> {
    let mut poly = vec![1u8];
    for i in 0..degree {
        let root = gf_pow(2, i);
        let mut next = vec![0u8; poly.len() + 1];
        for (j, &c) in poly.iter().enumerate() {
            next[j] ^= c;
            next[j + 1] ^= gf_mul(c, root);
        }
        poly = next;
    }
    poly
}

fn rs_encode(data: &[u8], ec_len: usize) -> Vec<u8> {
    let gen = rs_generator_poly(ec_len);
    let mut rem = vec![0u8; ec_len];
    for &b in data {
        let factor = b ^ rem[0];
        rem.remove(0);
        rem.push(0);
        for i in 0..ec_len {
            rem[i] ^= gf_mul(gen[i + 1], factor);
        }
    }
    rem
}

fn build_matrix(spec: &VersionSpec, codewords: &[u8]) -> Vec<Vec<bool>> {
    let n = spec.size;
    let mut modules = vec![vec![false; n]; n];
    let mut is_function = vec![vec![false; n]; n];

    let mut set_func = |r: usize, c: usize, val: bool| {
        modules[r][c] = val;
        is_function[r][c] = true;
    };

    // Finder patterns + separators
    let draw_finder = |top: usize, left: usize, set_func: &mut dyn FnMut(usize, usize, bool)| {
        for dr in 0..7 {
            for dc in 0..7 {
                let dark = dr == 0
                    || dr == 6
                    || dc == 0
                    || dc == 6
                    || ((2..=4).contains(&dr) && (2..=4).contains(&dc));
                set_func(top + dr, left + dc, dark);
            }
        }
    };
    draw_finder(0, 0, &mut set_func);
    draw_finder(0, n - 7, &mut set_func);
    draw_finder(n - 7, 0, &mut set_func);

    // White separators around finders
    for i in 0..8 {
        if i < n {
            set_func(7, i, false);
            set_func(i, 7, false);
            set_func(7, n - 1 - i, false);
            set_func(i, n - 8, false);
            set_func(n - 8, i, false);
            set_func(n - 1 - i, 7, false);
        }
    }

    // Timing patterns
    for i in 8..(n - 8) {
        let dark = i % 2 == 0;
        set_func(6, i, dark);
        set_func(i, 6, dark);
    }

    // Alignment patterns
    for &ar in spec.align_centers {
        for &ac in spec.align_centers {
            if (ar <= 8 && ac <= 8)
                || (ar <= 8 && ac >= n - 8)
                || (ar >= n - 8 && ac <= 8)
            {
                continue;
            }
            for dr in 0..5 {
                for dc in 0..5 {
                    let dark = dr == 0 || dr == 4 || dc == 0 || dc == 4 || (dr == 2 && dc == 2);
                    set_func(ar - 2 + dr, ac - 2 + dc, dark);
                }
            }
        }
    }

    // Dark module + reserve format info areas
    modules[n - 8][8] = true;
    is_function[n - 8][8] = true;
    for i in 0..9 {
        if i != 6 {
            if !is_function[8][i] {
                modules[8][i] = false;
                is_function[8][i] = true;
            }
            if !is_function[i][8] {
                modules[i][8] = false;
                is_function[i][8] = true;
            }
        }
    }
    for i in 0..8 {
        if !is_function[8][n - 1 - i] {
            modules[8][n - 1 - i] = false;
            is_function[8][n - 1 - i] = true;
        }
        if !is_function[n - 1 - i][8] {
            modules[n - 1 - i][8] = false;
            is_function[n - 1 - i][8] = true;
        }
    }

    // Place data bits in upward/downward 2-column zig-zag
    let mut bit_idx = 0usize;
    let total_bits = codewords.len() * 8;
    let mut col = (n - 1) as isize;
    let mut upward = true;
    while col > 0 {
        if col == 6 {
            col -= 1;
        }
        for step in 0..n {
            let r = if upward { n - 1 - step } else { step };
            for dc in 0..2 {
                let c = (col - dc) as usize;
                if !is_function[r][c] {
                    let bit = if bit_idx < total_bits {
                        let byte = codewords[bit_idx / 8];
                        ((byte >> (7 - (bit_idx % 8))) & 1) != 0
                    } else {
                        false
                    };
                    modules[r][c] = bit;
                    bit_idx += 1;
                }
            }
        }
        upward = !upward;
        col -= 2;
    }

    // Apply Mask 0 ((r + c) % 2 == 0) and write format bits for ECC Level L (01) + Mask 0 (000)
    for r in 0..n {
        for c in 0..n {
            if !is_function[r][c] && (r + c) % 2 == 0 {
                modules[r][c] = !modules[r][c];
            }
        }
    }

    // Format info for ECC Level L (01) + Mask 0 (000): 0x77C4 (15 bits: 111011111000100)
    let format_bits: u16 = 0b111011111000100;
    let get_fbit = |i: usize| -> bool { ((format_bits >> i) & 1) != 0 };

    // Around top-left finder
    for i in 0..6 {
        modules[8][i] = get_fbit(i);
    }
    modules[8][7] = get_fbit(6);
    modules[8][8] = get_fbit(7);
    modules[7][8] = get_fbit(8);
    for i in 9..15 {
        modules[14 - i][8] = get_fbit(i);
    }

    // Top-right and bottom-left format info
    for i in 0..8 {
        modules[8][n - 1 - i] = get_fbit(i);
    }
    for i in 8..15 {
        modules[n - 15 + i][8] = get_fbit(i);
    }
    modules[n - 8][8] = true;

    modules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_qr_svg_for_pairing_url() {
        let svg = render_svg("http://192.168.1.100:19880/?pair=123456");
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("viewBox=\"0 0 35 35\""));
        assert!(svg.contains("<path d=\"M"));
    }
}
