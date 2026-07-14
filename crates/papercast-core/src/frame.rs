//! Frame and geometry types shared between capture, pipeline, and server.

/// A rectangle in output-framebuffer pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
}

/// Pixel layout of a captured frame's byte buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 4 bytes per pixel, memory order B,G,R,X (wl_shm xrgb8888 on
    /// little-endian, the most common compositor format).
    Bgrx8888,
    /// 4 bytes per pixel, memory order R,G,B,X.
    Rgbx8888,
    /// 1 byte per pixel grayscale (pipeline output, test sources).
    Gray8,
}

impl PixelFormat {
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            PixelFormat::Bgrx8888 | PixelFormat::Rgbx8888 => 4,
            PixelFormat::Gray8 => 1,
        }
    }
}

/// One captured (or generated) frame handed from a source to the pipeline.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Row-major pixel data; rows are tightly packed (stride == width * bpp).
    /// Capture backends with padded strides must repack before handing off.
    pub data: Vec<u8>,
}
