//! Turns a document and a scroll position into a frame Slint can display.
//!
//! Two strategies are implemented so they can be compared on the same content:
//!
//! - `FullRepaint` rasterises the whole viewport every frame, straight into the
//!   pixel buffer that is handed to Slint.
//! - `Tiled` rasterises fixed-height horizontal strips once, keeps them, and
//!   builds each frame by copying the visible rows out of the cached strips.

use std::collections::HashMap;
use std::time::Instant;

use slint::{Rgba8Pixel, SharedPixelBuffer};
use vello_cpu::{Pixmap, PixmapMut, RenderContext, RenderSettings, Resources};

use crate::doc::Document;

/// Height of one cached strip in physical pixels.
pub const TILE_HEIGHT: u16 = 256;
/// Strips further than this many tiles from the viewport are dropped.
const KEEP_TILES: i64 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    FullRepaint,
    Tiled,
}

impl Strategy {
    pub fn label(self) -> &'static str {
        match self {
            Strategy::FullRepaint => "整屏重绘",
            Strategy::Tiled => "瓦片缓存",
        }
    }
}

/// Where the time of one frame went, in milliseconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    /// Walking the visible paragraphs and recording draw commands.
    pub scene_ms: f64,
    /// Rasterising those commands into pixels.
    pub raster_ms: f64,
    /// Allocating the output buffer and, for tiles, copying rows into it.
    pub compose_ms: f64,
    pub tiles_rendered: u32,
}

impl Timing {
    pub fn total_ms(&self) -> f64 {
        self.scene_ms + self.raster_ms + self.compose_ms
    }
}

fn ms(from: Instant, to: Instant) -> f64 {
    to.duration_since(from).as_secs_f64() * 1e3
}

pub struct Framer {
    strategy: Strategy,
    width: u16,
    height: u16,
    resources: Resources,
    full_ctx: RenderContext,
    tile_ctx: RenderContext,
    tiles: HashMap<i64, Pixmap>,
}

impl Framer {
    /// `threads` is the number of rasteriser worker threads; 0 keeps everything
    /// on the calling thread.
    pub fn new(strategy: Strategy, width: u16, height: u16, threads: u16) -> Self {
        let settings = RenderSettings {
            num_threads: threads,
            ..RenderSettings::default()
        };
        Self {
            strategy,
            width,
            height,
            resources: Resources::new(),
            full_ctx: RenderContext::new_with(width, height, settings),
            tile_ctx: RenderContext::new_with(width, TILE_HEIGHT, settings),
            tiles: HashMap::new(),
        }
    }

    /// Drop every cached strip that shows any part of the band `top..bottom`.
    pub fn invalidate(&mut self, top: f64, bottom: f64) {
        let tile = f64::from(TILE_HEIGHT);
        let first = (top / tile).floor() as i64;
        let last = ((bottom - 1.0).max(top) / tile).floor() as i64;
        self.tiles
            .retain(|index, _| *index < first || *index > last);
    }

    /// Drop every cached strip at or below `top`. Needed when an edit changes a
    /// paragraph's height and everything after it moves.
    pub fn invalidate_from(&mut self, top: f64) {
        let first = (top / f64::from(TILE_HEIGHT)).floor() as i64;
        self.tiles.retain(|index, _| *index < first);
    }

    /// Produce the frame whose top edge is `top` physical pixels into the document.
    pub fn frame(&mut self, doc: &Document, top: i64) -> (SharedPixelBuffer<Rgba8Pixel>, Timing) {
        match self.strategy {
            Strategy::FullRepaint => self.full_frame(doc, top),
            Strategy::Tiled => self.tiled_frame(doc, top),
        }
    }

    fn full_frame(&mut self, doc: &Document, top: i64) -> (SharedPixelBuffer<Rgba8Pixel>, Timing) {
        let (width, height) = (self.width, self.height);
        let t0 = Instant::now();
        self.full_ctx.reset();
        doc.paint(
            &mut self.full_ctx,
            &mut self.resources,
            top as f64,
            f64::from(height),
        );
        let t1 = Instant::now();
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(u32::from(width), u32::from(height));
        let t2 = Instant::now();
        self.full_ctx.flush();
        let target = PixmapMut::new(width, height, buffer.make_mut_bytes())
            .expect("buffer matches the render context size");
        self.full_ctx.render(target, &mut self.resources);
        let t3 = Instant::now();
        let timing = Timing {
            scene_ms: ms(t0, t1),
            raster_ms: ms(t2, t3),
            compose_ms: ms(t1, t2),
            tiles_rendered: 0,
        };
        (buffer, timing)
    }

    fn tiled_frame(&mut self, doc: &Document, top: i64) -> (SharedPixelBuffer<Rgba8Pixel>, Timing) {
        let (width, height) = (self.width, self.height);
        let tile = i64::from(TILE_HEIGHT);
        let bottom = top + i64::from(height);
        let first = top.div_euclid(tile);
        let last = (bottom - 1).div_euclid(tile);
        let mut timing = Timing::default();

        for index in first..=last {
            if self.tiles.contains_key(&index) {
                continue;
            }
            let t0 = Instant::now();
            self.tile_ctx.reset();
            doc.paint(
                &mut self.tile_ctx,
                &mut self.resources,
                (index * tile) as f64,
                f64::from(TILE_HEIGHT),
            );
            let t1 = Instant::now();
            let mut pixmap = Pixmap::new(width, TILE_HEIGHT);
            self.tile_ctx.flush();
            self.tile_ctx.render(&mut pixmap, &mut self.resources);
            let t2 = Instant::now();
            timing.scene_ms += ms(t0, t1);
            timing.raster_ms += ms(t1, t2);
            timing.tiles_rendered += 1;
            self.tiles.insert(index, pixmap);
        }

        let t0 = Instant::now();
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(u32::from(width), u32::from(height));
        let row_bytes = usize::from(width) * 4;
        let out = buffer.make_mut_bytes();
        for index in first..=last {
            let tile_top = index * tile;
            let from = top.max(tile_top);
            let to = bottom.min(tile_top + tile);
            let rows = (to - from) as usize;
            let src_start = (from - tile_top) as usize * row_bytes;
            let dst_start = (from - top) as usize * row_bytes;
            let src = self.tiles[&index].data_as_u8_slice();
            out[dst_start..dst_start + rows * row_bytes]
                .copy_from_slice(&src[src_start..src_start + rows * row_bytes]);
        }
        timing.compose_ms = ms(t0, Instant::now());

        self.tiles
            .retain(|index, _| *index >= first - KEEP_TILES && *index <= last + KEEP_TILES);
        (buffer, timing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How far apart two frames are: `(pixels that differ, largest channel delta)`.
    fn difference(
        a: &SharedPixelBuffer<Rgba8Pixel>,
        b: &SharedPixelBuffer<Rgba8Pixel>,
    ) -> (usize, u8) {
        let (a, b) = (
            a.as_bytes().as_chunks::<4>().0,
            b.as_bytes().as_chunks::<4>().0,
        );
        assert_eq!(a.len(), b.len());
        let mut pixels = 0;
        let mut delta = 0;
        for (x, y) in a.iter().zip(b) {
            if x != y {
                pixels += 1;
                for (c, d) in x.iter().zip(y) {
                    delta = delta.max(c.abs_diff(*d));
                }
            }
        }
        (pixels, delta)
    }

    /// Strips are rasterised with a different origin than a full repaint, so glyph
    /// positions round differently in f32 and a handful of anti-aliased pixels land
    /// one level apart. That is invisible, but it is not bit-identical.
    fn assert_visually_identical(
        a: &SharedPixelBuffer<Rgba8Pixel>,
        b: &SharedPixelBuffer<Rgba8Pixel>,
        what: &str,
    ) {
        let (pixels, delta) = difference(a, b);
        let total = a.as_bytes().len() / 4;
        assert!(delta <= 1, "{what}: channel delta {delta}");
        assert!(
            pixels * 10_000 <= total,
            "{what}: {pixels} of {total} pixels differ"
        );
    }

    fn document(width: u16) -> Document {
        let mut doc = Document::generate(80);
        doc.layout_all(width, 2.0);
        doc
    }

    #[test]
    fn tiled_frames_match_full_repaints_to_within_one_level() {
        let (width, height) = (720, 600);
        let doc = document(width);
        let mut full = Framer::new(Strategy::FullRepaint, width, height, 0);
        let mut tiled = Framer::new(Strategy::Tiled, width, height, 0);
        for top in [0_i64, 1, 255, 256, 257, 700, 1333, 2048] {
            let (a, _) = full.frame(&doc, top);
            let (b, _) = tiled.frame(&doc, top);
            assert_visually_identical(&a, &b, &format!("top={top}"));
        }
    }

    #[test]
    fn a_frame_is_not_blank() {
        let (width, height) = (720, 600);
        let doc = document(width);
        let (frame, _) = Framer::new(Strategy::FullRepaint, width, height, 0).frame(&doc, 0);
        let dark = frame
            .as_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] < 0x80)
            .count();
        assert!(dark > 2000, "only {dark} dark pixels");
    }

    #[test]
    fn scrolling_reuses_cached_strips() {
        let (width, height) = (720, 600);
        let doc = document(width);
        let mut tiled = Framer::new(Strategy::Tiled, width, height, 0);
        let (_, first) = tiled.frame(&doc, 0);
        assert_eq!(first.tiles_rendered, 3);
        let (_, same) = tiled.frame(&doc, 40);
        assert_eq!(same.tiles_rendered, 0);
        let (_, next) = tiled.frame(&doc, 200);
        assert_eq!(next.tiles_rendered, 1);
    }

    #[test]
    fn an_edit_repaints_only_the_strips_it_touches() {
        let (width, height) = (720, 600);
        let mut doc = document(width);
        let mut tiled = Framer::new(Strategy::Tiled, width, height, 0);
        let mut full = Framer::new(Strategy::FullRepaint, width, height, 0);
        tiled.frame(&doc, 0);

        let index = doc.paragraph_at(300.0);
        let (top, bottom) = doc.paragraph_band(index);
        let moved = doc.append(index, "x");
        assert!(!moved, "one character should not add a line here");
        tiled.invalidate(top, bottom);

        let (after, timing) = tiled.frame(&doc, 0);
        assert!(timing.tiles_rendered >= 1 && timing.tiles_rendered <= 2);
        let (expected, _) = full.frame(&doc, 0);
        assert_visually_identical(&after, &expected, "after the edit");
    }

    #[test]
    fn worker_threads_do_not_change_the_pixels() {
        let (width, height) = (720, 600);
        let doc = document(width);
        let (single, _) = Framer::new(Strategy::FullRepaint, width, height, 0).frame(&doc, 500);
        let (multi, _) = Framer::new(Strategy::FullRepaint, width, height, 4).frame(&doc, 500);
        assert!(single.as_bytes() == multi.as_bytes());
    }
}
