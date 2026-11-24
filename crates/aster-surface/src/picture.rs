//! Pictures from documents, decoded once and drawn scaled into a rectangle.

use std::collections::HashMap;
use std::sync::Arc;

use vello_cpu::kurbo::{Affine, Rect};
use vello_cpu::peniko::color::PremulRgba8;
use vello_cpu::peniko::{ImageQuality, ImageSampler};
use vello_cpu::{Image, ImageSource, Pixmap, RenderContext};

/// Longest side a picture is kept at. Enough for a full-window slide on a
/// 2× screen; larger photos are shrunk when decoded.
const MAX_SIDE: u32 = 2560;
/// Decoded pixels kept at most, bytes; past this the cache starts over.
const CACHE_BYTES: usize = 384 << 20;

/// A picture's bytes, kept so their address stays a valid key, and the
/// picture decoded (`None` when it cannot be).
type Entry = (Arc<[u8]>, Option<Arc<Pixmap>>);

/// Decoded pictures by the address of their bytes. The bytes are kept with
/// them so the address cannot be reused while it is a key.
#[derive(Default)]
pub(crate) struct ImageCache {
    images: HashMap<usize, Entry>,
    bytes: usize,
}

impl ImageCache {
    /// The picture, decoded on first use. `None` for one that cannot be
    /// shown (an unsupported format, damaged, too large).
    pub(crate) fn get(&mut self, bytes: &Arc<[u8]>) -> Option<Arc<Pixmap>> {
        let key = bytes.as_ptr() as usize;
        if let Some((_, decoded)) = self.images.get(&key) {
            return decoded.clone();
        }
        let decoded = aster_image::decode(bytes, MAX_SIDE)
            .ok()
            .and_then(to_pixmap)
            .map(Arc::new);
        let size = decoded
            .as_ref()
            .map_or(0, |p| usize::from(p.width()) * usize::from(p.height()) * 4);
        if self.bytes + size > CACHE_BYTES {
            self.images.clear();
            self.bytes = 0;
        }
        self.bytes += size;
        self.images.insert(key, (bytes.clone(), decoded.clone()));
        decoded
    }
}

fn to_pixmap(image: aster_image::Rgba) -> Option<Pixmap> {
    let width = u16::try_from(image.width).ok()?;
    let height = u16::try_from(image.height).ok()?;
    let transparent = image.has_transparency();
    let pixels = image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = u16::from(p[3]);
            let premultiply = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
            PremulRgba8 {
                r: premultiply(p[0]),
                g: premultiply(p[1]),
                b: premultiply(p[2]),
                a: p[3],
            }
        })
        .collect();
    Some(Pixmap::from_parts_with_opacity(
        pixels,
        width,
        height,
        transparent,
    ))
}

/// Draw `pixmap` stretched over `rect` (before `transform`). `crop` is the
/// share cut off each side of the picture, left, top, right, bottom, as
/// PowerPoint's "crop" stores it.
pub(crate) fn draw_image(
    ctx: &mut RenderContext,
    pixmap: &Arc<Pixmap>,
    rect: Rect,
    crop: Option<[f64; 4]>,
    transform: Affine,
) {
    let (w, h) = (f64::from(pixmap.width()), f64::from(pixmap.height()));
    let [l, t, r, b] = crop.unwrap_or([0.0; 4]);
    let source_w = (w * (1.0 - l - r)).max(1e-3);
    let source_h = (h * (1.0 - t - b)).max(1e-3);
    let paint = Affine::translate((rect.x0, rect.y0))
        * Affine::scale_non_uniform(rect.width() / source_w, rect.height() / source_h)
        * Affine::translate((-l * w, -t * h));
    ctx.set_transform(transform);
    ctx.set_paint(Image {
        image: ImageSource::Pixmap(pixmap.clone()),
        sampler: ImageSampler {
            quality: ImageQuality::Medium,
            ..ImageSampler::default()
        },
    });
    ctx.set_paint_transform(paint);
    ctx.fill_rect(&rect);
    ctx.set_paint_transform(Affine::IDENTITY);
    ctx.set_transform(Affine::IDENTITY);
}
