//! Nearest-neighbor sampling for magnified previews. GPUI's image sampler is linear.

use std::{collections::HashMap, ops::Range, sync::Arc};

use gpui_kit::{Bounds, Pixels, RenderImage, Window, fill, point, px, rgba, size};
use image::{Frame, RgbaImage};

const TILE: u32 = 256;

#[derive(Default)]
pub(super) struct Tiles {
    extent: (u32, u32),
    images: HashMap<(u32, u32), Arc<RenderImage>>,
}

impl Tiles {
    #[cfg(all(test, feature = "gui-tests"))]
    pub(super) fn tile(&self, key: (u32, u32)) -> Option<Arc<RenderImage>> {
        self.images.get(&key).cloned()
    }

    #[cfg(all(test, feature = "gui-tests"))]
    pub(super) fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub(super) fn clear(&mut self, release: impl FnMut(Arc<RenderImage>)) {
        self.images
            .drain()
            .map(|(_, image)| image)
            .for_each(release);
    }

    pub(super) fn paint(
        &mut self,
        bounds: Bounds<Pixels>,
        source: &RenderImage,
        grid: bool,
        window: &mut Window,
    ) -> anyhow::Result<()> {
        let scale = window.scale_factor();
        let scaled = |value: Pixels| (value / px(1.)) * scale;
        let logical = |value: i32| px(value as f32 / scale);
        let (origin, extent) = device_rect(bounds, scale);
        if self.extent != extent {
            self.clear(|image| {
                let _ = window.drop_image(image);
            });
            self.extent = extent;
        }
        let mask = window.content_mask().bounds;
        let clip = |start: Pixels, end: Pixels, origin: i32, extent: u32| {
            let clamp = |value: i32| ((value - origin).max(0) as u32).min(extent);
            clamp(scaled(start).floor() as i32)..clamp(scaled(end).ceil() as i32)
        };
        let x_clip = clip(mask.left(), mask.right(), origin.x, extent.0);
        let y_clip = clip(mask.top(), mask.bottom(), origin.y, extent.1);
        let columns = visible_tiles(&x_clip);
        let rows = visible_tiles(&y_clip);
        // Only live viewport tiles occupy the atlas, even for full-page captures at 25x.
        self.images.retain(|&(x, y), image| {
            let keep = columns.contains(&x) && rows.contains(&y);
            if !keep {
                let _ = window.drop_image(image.clone());
            }
            keep
        });
        let Some(bytes) = source.as_bytes(0) else {
            return Ok(());
        };
        let source_size = source.size(0);
        let source_size = (u32::from(source_size.width), u32::from(source_size.height));
        for y in rows {
            for x in columns.clone() {
                let left = x * TILE;
                let top = y * TILE;
                let tile = self.images.entry((x, y)).or_insert_with(|| {
                    let pixels = sample_tile(bytes, source_size, extent, (left, top));
                    // GPUI already converted the decoded frame into the atlas channel order.
                    Arc::new(RenderImage::new([Frame::new(pixels)]))
                });
                let bounds = Bounds::new(
                    point(
                        logical(origin.x + left as i32),
                        logical(origin.y + top as i32),
                    ),
                    size(
                        logical(tile.size(0).width.0),
                        logical(tile.size(0).height.0),
                    ),
                );
                // Pre-sampled tiles paint one-to-one in device pixels, without interpolation.
                window.paint_image(bounds, bounds, Default::default(), tile.clone(), 0, false)?;
            }
        }
        if grid && !x_clip.is_empty() && !y_clip.is_empty() {
            let color = rgba(0xc0c0c038);
            for x in grid_lines(&x_clip, source_size.0, extent.0) {
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            logical(origin.x + x as i32),
                            logical(origin.y + y_clip.start as i32),
                        ),
                        size(logical(1), logical(y_clip.len() as i32)),
                    ),
                    color,
                ));
            }
            for y in grid_lines(&y_clip, source_size.1, extent.1) {
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            logical(origin.x + x_clip.start as i32),
                            logical(origin.y + y as i32),
                        ),
                        size(logical(x_clip.len() as i32), logical(1)),
                    ),
                    color,
                ));
            }
        }
        Ok(())
    }
}

fn device_rect(bounds: Bounds<Pixels>, scale: f32) -> (gpui_kit::Point<i32>, (u32, u32)) {
    let device = |value: Pixels| ((value / px(1.)) * scale).round() as i32;
    let origin = bounds.origin.map(device);
    // Derive extent from size so panning across rounding boundaries never invalidates tiles.
    let extent = (
        device(bounds.size.width).max(0) as u32,
        device(bounds.size.height).max(0) as u32,
    );
    (origin, extent)
}

fn visible_tiles(clip: &Range<u32>) -> Range<u32> {
    if clip.is_empty() {
        return 0..0;
    }
    clip.start / TILE..clip.end.div_ceil(TILE)
}

fn grid_lines(clip: &Range<u32>, cells: u32, extent: u32) -> impl Iterator<Item = u32> {
    let clip = clip.clone();
    let cells = u64::from(cells);
    let extent = u64::from(extent);
    let first = u64::from(clip.start) * cells / extent;
    let end = (u64::from(clip.end) * cells).div_ceil(extent);
    (first..end).filter_map(move |cell| {
        // Use the same pixel-center rule as sampling; fractional zoom must not drift.
        let boundary = (2 * cell * extent)
            .saturating_sub(cells)
            .div_ceil(2 * cells) as u32;
        clip.contains(&boundary).then_some(boundary)
    })
}

fn sample_tile(
    source: &[u8],
    source_size: (u32, u32),
    extent: (u32, u32),
    origin: (u32, u32),
) -> RgbaImage {
    let width = TILE.min(extent.0 - origin.0);
    let height = TILE.min(extent.1 - origin.1);
    // Map device-pixel centers through the whole image so fractional zoom stays
    // continuous across tile boundaries and when the viewport moves.
    let nearest = |pixel, source, output| {
        (((2 * u64::from(pixel) + 1) * u64::from(source)) / (2 * u64::from(output))) as usize
    };
    let columns: Vec<_> = (origin.0..origin.0 + width)
        .map(|x| nearest(x, source_size.0, extent.0) * 4)
        .collect();
    let stride = width as usize * 4;
    let mut pixels = vec![0; stride * height as usize];
    let mut previous_row = None;
    for (y, source_y) in (origin.1..origin.1 + height)
        .map(|y| nearest(y, source_size.1, extent.1))
        .enumerate()
    {
        let offset = y * stride;
        if y > 0 && previous_row == Some(source_y) {
            pixels.copy_within(offset - stride..offset, offset);
        } else {
            let start = source_y * source_size.0 as usize * 4;
            for (pixel, &x) in pixels[offset..offset + stride]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(&columns)
            {
                pixel.copy_from_slice(&source[start + x..start + x + 4]);
            }
        }
        previous_row = Some(source_y);
    }
    RgbaImage::from_raw(width, height, pixels).expect("tile dimensions match its pixel buffer")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_original_colors_and_alpha_across_fractional_tile_edges() {
        let source = [2, 3, 4, 127, 5, 6, 7, 255, 8, 9, 10, 0];
        let left = sample_tile(&source, (3, 1), (512, 2), (0, 0));
        let right = sample_tile(&source, (3, 1), (512, 2), (256, 0));
        assert_eq!(left.get_pixel(255, 1).0, [5, 6, 7, 255]);
        assert_eq!(right.get_pixel(0, 1).0, [5, 6, 7, 255]);
        assert_eq!(right.get_pixel(255, 0).0, [8, 9, 10, 0]);
        let edge = sample_tile(&source, (3, 1), (513, 3), (512, 0));
        assert_eq!(edge.dimensions(), (1, 3));
        assert_eq!(edge.get_pixel(0, 2).0, [8, 9, 10, 0]);
    }

    #[test]
    fn clips_to_visible_tiles_even_far_into_a_full_page_capture() {
        assert_eq!(visible_tiles(&(0..0)), 0..0);
        assert_eq!(visible_tiles(&(0..1)), 0..1);
        assert_eq!(visible_tiles(&(256..512)), 1..2);
        assert_eq!(visible_tiles(&(1_999_999..2_000_480)), 7812..7815);
    }

    #[test]
    fn grid_aligns_with_sample_transitions_at_fractional_zoom_and_when_clipped() {
        let output = sample_tile(
            &[0, 0, 0, 255, 255, 255, 255, 255],
            (2, 1),
            (25, 13),
            (0, 0),
        );
        let edge = grid_lines(&(10..18), 2, 25).collect::<Vec<_>>();
        assert_eq!(edge, [12]);
        assert_eq!(output.get_pixel(edge[0] - 1, 0).0[0], 0);
        assert_eq!(output.get_pixel(edge[0], 0).0[0], 255);
        assert_eq!(grid_lines(&(248..258), 80, 1000).collect::<Vec<_>>(), [250]);
    }

    #[test]
    fn extent_stays_invariant_across_subpixel_and_dpi_pan_translations() {
        let size = size(px(2025.), px(1012.5));
        let baseline = device_rect(Bounds::new(point(px(0.), px(0.)), size), 1.5).1;
        for offset in [-12.0_f32, -0.5, 0.3, 0.5, 1.0, 241.0] {
            let (_, moved) = device_rect(Bounds::new(point(px(offset), px(offset)), size), 1.5);
            assert_eq!(
                moved, baseline,
                "Panning to {offset} must not change magnified tile extent"
            );
        }
    }
}
