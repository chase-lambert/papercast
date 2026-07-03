//! Tiny drawing helpers for diagnostic overlays on grayscale buffers:
//! seven-segment digits for the test pattern and the latency probe.

pub fn fill_rect(px: &mut [u8], stride: usize, x: usize, y: usize, w: usize, h: usize, value: u8) {
    let rows = px.len() / stride;
    for yy in y..(y + h).min(rows) {
        let row = &mut px[yy * stride..yy * stride + stride];
        for p in &mut row[x.min(stride)..(x + w).min(stride)] {
            *p = value;
        }
    }
}

/// Segment layout: bit 6..0 = a,b,c,d,e,f,g (a=top, g=middle, clockwise).
const SEGMENTS: [u8; 10] = [
    0b1111110, // 0
    0b0110000, // 1
    0b1101101, // 2
    0b1111001, // 3
    0b0110011, // 4
    0b1011011, // 5
    0b1011111, // 6
    0b1110000, // 7
    0b1111111, // 8
    0b1111011, // 9
];

/// Draw `ms` as zero-padded digits on a white plate at the top-left.
/// Black-on-white regardless of theme/invert so a camera can always read it.
pub fn draw_ms_counter(px: &mut [u8], stride: usize, ms: u64, scale: usize) {
    let digits: Vec<u8> = format!("{ms:08}").bytes().map(|b| b - b'0').collect();
    let cell_w = 6 * scale;
    let plate_w = digits.len() * cell_w + 2 * scale;
    fill_rect(px, stride, 0, 0, plate_w, 12 * scale, 255);
    for (i, &d) in digits.iter().enumerate() {
        draw_digit(px, stride, scale + i * cell_w, scale, scale, d);
    }
}

fn draw_digit(px: &mut [u8], stride: usize, x: usize, y: usize, s: usize, digit: u8) {
    let seg = SEGMENTS[digit as usize];
    let (len, th) = (4 * s, s); // segment length and thickness
    let mut horiz = |on: bool, dy: usize| {
        if on {
            fill_rect(px, stride, x + th, y + dy, len - 2 * th, th, 0);
        }
    };
    horiz(seg & 0b1000000 != 0, 0); // a: top
    horiz(seg & 0b0000001 != 0, len); // g: middle
    horiz(seg & 0b0001000 != 0, 2 * len); // d: bottom
    let mut vert = |on: bool, dx: usize, dy: usize| {
        if on {
            fill_rect(px, stride, x + dx, y + dy, th, len, 0);
        }
    };
    vert(seg & 0b0100000 != 0, len - th, 0); // b: top-right
    vert(seg & 0b0000100 != 0, len - th, len); // c: bottom-right
    vert(seg & 0b0000010 != 0, 0, 0); // f: top-left
    vert(seg & 0b0010000 != 0, 0, len); // e: bottom-left
}
