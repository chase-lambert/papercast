//! Dirty-tile detection: which parts of the processed frame actually changed?
//!
//! Both output transports send the rects produced here, so granularity is
//! bandwidth: a blinking cursor should cost one tile, not a bounding box from
//! the cursor to the clock. Tiles are compared against the previous processed
//! frame (post-dither — safe because Bayer is position-anchored, so unchanged
//! input produces byte-identical output), then coalesced into row runs.

use crate::Rect;

pub struct TileDiff {
    tile: u32,
    /// More rects than this per frame collapse into one bounding box — the
    /// VNC merges beyond roughly 10 regions anyway, and both transports'
    /// per-rect overhead starts beating the pixel savings.
    max_rects: usize,
    prev: Vec<u8>,
    dims: (u32, u32),
}

impl TileDiff {
    pub fn new(tile_size: u32, max_rects: usize) -> Self {
        Self {
            tile: tile_size.max(8),
            max_rects: max_rects.max(1),
            prev: Vec::new(),
            dims: (0, 0),
        }
    }

    /// Compare `data` (gray, w×h) against the previous frame; remember it;
    /// return the changed regions. First frame (or a size change) returns
    /// one full-frame rect.
    pub fn diff(&mut self, data: &[u8], (w, h): (u32, u32)) -> Vec<Rect> {
        debug_assert_eq!(data.len(), w as usize * h as usize);
        if self.dims != (w, h) || self.prev.len() != data.len() {
            self.dims = (w, h);
            self.prev = data.to_vec();
            return vec![Rect::new(0, 0, w, h)];
        }

        let tiles_x = w.div_ceil(self.tile);
        let tiles_y = h.div_ceil(self.tile);
        let mut rects: Vec<Rect> = Vec::new();

        for ty in 0..tiles_y {
            let y0 = ty * self.tile;
            let th = (h - y0).min(self.tile);
            // Coalesce horizontally: extend the current run while tiles in
            // this row keep being dirty.
            let mut run: Option<Rect> = None;
            for tx in 0..tiles_x {
                let x0 = tx * self.tile;
                let tw = (w - x0).min(self.tile);
                if self.tile_changed(data, w, x0, y0, tw, th) {
                    run = Some(match run {
                        Some(r) => Rect::new(r.x, r.y, r.width + tw, r.height),
                        None => Rect::new(x0, y0, tw, th),
                    });
                } else if let Some(r) = run.take() {
                    push_or_merge_vertically(&mut rects, r);
                }
            }
            if let Some(r) = run.take() {
                push_or_merge_vertically(&mut rects, r);
            }
        }

        if rects.len() > self.max_rects {
            let bounds = rects
                .iter()
                .skip(1)
                .fold(rects[0], |acc, r| {
                    let x = acc.x.min(r.x);
                    let y = acc.y.min(r.y);
                    let right = (acc.x + acc.width).max(r.x + r.width);
                    let bottom = (acc.y + acc.height).max(r.y + r.height);
                    Rect::new(x, y, right - x, bottom - y)
                });
            rects = vec![bounds];
        }

        self.prev.copy_from_slice(data);
        rects
    }

    fn tile_changed(&self, data: &[u8], stride: u32, x0: u32, y0: u32, tw: u32, th: u32) -> bool {
        let (stride, x0, tw) = (stride as usize, x0 as usize, tw as usize);
        (y0 as usize..(y0 + th) as usize).any(|y| {
            let off = y * stride + x0;
            data[off..off + tw] != self.prev[off..off + tw]
        })
    }
}

/// If `r` sits directly below an existing rect with the same x/width, grow
/// that rect downward instead of adding a new one (vertical coalescing —
/// catches window edges, scrolling regions, etc.).
fn push_or_merge_vertically(rects: &mut Vec<Rect>, r: Rect) {
    if let Some(last) = rects
        .iter_mut()
        .rev()
        .find(|e| e.x == r.x && e.width == r.width && e.y + e.height == r.y)
    {
        last.height += r.height;
    } else {
        rects.push(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> Vec<u8> {
        (0..w * h).map(|i| f(i % w, i / w)).collect()
    }

    #[test]
    fn first_frame_is_fully_dirty() {
        let mut d = TileDiff::new(16, 8);
        let rects = d.diff(&frame(64, 64, |_, _| 0), (64, 64));
        assert_eq!(rects, vec![Rect::new(0, 0, 64, 64)]);
    }

    #[test]
    fn identical_frame_is_clean() {
        let mut d = TileDiff::new(16, 8);
        let data = frame(64, 64, |x, y| (x + y) as u8);
        d.diff(&data, (64, 64));
        assert!(d.diff(&data, (64, 64)).is_empty());
    }

    #[test]
    fn single_pixel_dirties_single_tile() {
        let mut d = TileDiff::new(16, 8);
        let mut data = frame(64, 64, |_, _| 0);
        d.diff(&data, (64, 64));
        data[20 * 64 + 40] = 255; // tile (2,1): x 32..48, y 16..32
        assert_eq!(d.diff(&data, (64, 64)), vec![Rect::new(32, 16, 16, 16)]);
    }

    #[test]
    fn distant_changes_stay_separate_rects() {
        let mut d = TileDiff::new(16, 8);
        let mut data = frame(64, 64, |_, _| 0);
        d.diff(&data, (64, 64));
        data[0] = 255; // tile (0,0)
        data[63 * 64 + 63] = 255; // tile (3,3)
        let rects = d.diff(&data, (64, 64));
        assert_eq!(rects, vec![Rect::new(0, 0, 16, 16), Rect::new(48, 48, 16, 16)]);
    }

    #[test]
    fn horizontal_run_coalesces() {
        let mut d = TileDiff::new(16, 8);
        let mut data = frame(64, 64, |_, _| 0);
        d.diff(&data, (64, 64));
        for x in 0..64 {
            data[8 * 64 + x] = 255; // full row through all 4 tiles of row 0
        }
        assert_eq!(d.diff(&data, (64, 64)), vec![Rect::new(0, 0, 64, 16)]);
    }

    #[test]
    fn vertical_runs_merge() {
        let mut d = TileDiff::new(16, 8);
        let mut data = frame(64, 64, |_, _| 0);
        d.diff(&data, (64, 64));
        for y in 0..64 {
            data[y * 64 + 8] = 255; // full column through all 4 tile rows
        }
        assert_eq!(d.diff(&data, (64, 64)), vec![Rect::new(0, 0, 16, 64)]);
    }

    #[test]
    fn rect_explosion_collapses_to_bounding_box() {
        let mut d = TileDiff::new(16, 2);
        let mut data = frame(64, 64, |_, _| 0);
        d.diff(&data, (64, 64));
        // Dirty a diagonal: 4 separate tiles > max_rects=2.
        for t in 0..4 {
            data[(t * 16) * 64 + t * 16] = 255;
        }
        assert_eq!(d.diff(&data, (64, 64)), vec![Rect::new(0, 0, 64, 64)]);
    }

    #[test]
    fn ragged_edge_tiles_are_covered() {
        let mut d = TileDiff::new(16, 8);
        let mut data = frame(70, 40, |_, _| 0); // not multiples of 16
        d.diff(&data, (70, 40));
        data[39 * 70 + 69] = 1; // bottom-right corner
        assert_eq!(d.diff(&data, (70, 40)), vec![Rect::new(64, 32, 6, 8)]);
    }
}
