//! Pixel format conversions between pipeline formats and what the VNC
//! framebuffer stores (RGBA, 4 bytes/pixel).

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
}
