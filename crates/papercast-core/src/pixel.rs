//! Pixel format conversions between pipeline formats and what the VNC
//! framebuffer stores (RGBA, 4 bytes/pixel).

use crate::{Frame, PixelFormat};

/// Expand 8-bit grayscale into RGBA (R=G=B=gray, A=255), reusing `out`'s
/// allocation across frames. Gray is symmetric in R/G/B, so this is correct
/// regardless of the client's negotiated channel order.
pub fn gray8_to_rgba(gray: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(gray.len() * 4);
    for &g in gray {
        out.extend_from_slice(&[g, g, g, 255]);
    }
}

/// B,G,R,X memory order (wl_shm xrgb8888 on little-endian) → RGBA.
pub fn bgrx_to_rgba(src: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(src.len());
    for px in src.chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
}

/// R,G,B,X memory order (wl_shm xbgr8888 on little-endian) → RGBA.
pub fn rgbx_to_rgba(src: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(src.len());
    for px in src.chunks_exact(4) {
        out.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
}

/// Convert any [`Frame`] to the RGBA layout the VNC framebuffer stores.
pub fn frame_to_rgba(frame: &Frame, out: &mut Vec<u8>) {
    match frame.format {
        PixelFormat::Gray8 => gray8_to_rgba(&frame.data, out),
        PixelFormat::Bgrx8888 => bgrx_to_rgba(&frame.data, out),
        PixelFormat::Rgbx8888 => rgbx_to_rgba(&frame.data, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_and_reuses_buffer() {
        let mut out = Vec::new();
        gray8_to_rgba(&[0, 128, 255], &mut out);
        assert_eq!(out, [0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255]);
        // Second call must fully replace, not append.
        gray8_to_rgba(&[7], &mut out);
        assert_eq!(out, [7, 7, 7, 255]);
    }

    #[test]
    fn swizzles_bgrx() {
        let mut out = Vec::new();
        bgrx_to_rgba(&[10, 20, 30, 0], &mut out); // B=10 G=20 R=30
        assert_eq!(out, [30, 20, 10, 255]);
    }

    #[test]
    fn passes_rgbx() {
        let mut out = Vec::new();
        rgbx_to_rgba(&[10, 20, 30, 0], &mut out);
        assert_eq!(out, [10, 20, 30, 255]);
    }
}
