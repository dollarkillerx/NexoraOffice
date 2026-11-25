//! The icons of conditional formats, drawn as simple shapes: arrows,
//! traffic lights, signs, symbols, flags, stars, triangles, quarters,
//! boxes and rating bars. Each is made to fit a square of a few pixels
//! on a side.

use aster_sheets::cond::{Icon, IconFamily};
use vello_cpu::RenderContext;
use vello_cpu::kurbo::{Arc, BezPath, Circle, Point, Rect, Shape, Stroke};
use vello_cpu::peniko::Color;

type Rgba8 = [u8; 4];

const GREEN: Rgba8 = [0x2f, 0xa8, 0x4f, 0xff];
const LIME: Rgba8 = [0x9a, 0xc9, 0x3c, 0xff];
const YELLOW: Rgba8 = [0xf2, 0xb2, 0x0a, 0xff];
const ORANGE: Rgba8 = [0xf0, 0x80, 0x1e, 0xff];
const RED: Rgba8 = [0xe0, 0x3a, 0x3e, 0xff];
const PINK: Rgba8 = [0xe8, 0x8a, 0x8c, 0xff];
const GRAY: Rgba8 = [0x8a, 0x8a, 0x8a, 0xff];
const LIGHT: Rgba8 = [0xd4, 0xd4, 0xd4, 0xff];
const BLACK: Rgba8 = [0x30, 0x30, 0x30, 0xff];
const WHITE: Rgba8 = [0xff, 0xff, 0xff, 0xff];

fn paint(color: Rgba8) -> Color {
    Color::from_rgba8(color[0], color[1], color[2], color[3])
}

/// Red for the lowest, green for the highest, in between as many as the
/// set has.
fn level(index: u8, count: u8) -> Rgba8 {
    let palette: &[Rgba8] = match count {
        3 => &[RED, YELLOW, GREEN],
        4 => &[RED, ORANGE, YELLOW, GREEN],
        _ => &[RED, ORANGE, YELLOW, LIME, GREEN],
    };
    palette[usize::from(index).min(palette.len() - 1)]
}

fn polygon(points: &[(f64, f64)]) -> BezPath {
    let mut path = BezPath::new();
    for (i, point) in points.iter().enumerate() {
        if i == 0 {
            path.move_to(*point);
        } else {
            path.line_to(*point);
        }
    }
    path.close_path();
    path
}

/// A pie of the part `share` (0 to 1) of a circle, from the top, clockwise.
fn pie(center: Point, radius: f64, share: f64) -> BezPath {
    let mut path = BezPath::new();
    if share >= 0.999 {
        return Circle::new(center, radius).to_path(0.1);
    }
    path.move_to(center);
    path.line_to((center.x, center.y - radius));
    let arc = Arc::new(
        center,
        (radius, radius),
        -std::f64::consts::FRAC_PI_2,
        share * std::f64::consts::TAU,
        0.0,
    );
    path.extend(arc.append_iter(0.1));
    path.close_path();
    path
}

fn star(center: Point, radius: f64) -> BezPath {
    let points: Vec<(f64, f64)> = (0..10)
        .map(|i| {
            let angle = -std::f64::consts::FRAC_PI_2 + f64::from(i) * std::f64::consts::PI / 5.0;
            let r = if i % 2 == 0 { radius } else { radius * 0.42 };
            (center.x + r * angle.cos(), center.y + r * angle.sin())
        })
        .collect();
    polygon(&points)
}

/// An arrow pointing along `angle` (radians, clockwise from the right).
fn arrow(center: Point, size: f64, angle: f64) -> BezPath {
    let unit = [
        (-0.85, -0.2),
        (0.1, -0.2),
        (0.1, -0.7),
        (0.95, 0.0),
        (0.1, 0.7),
        (0.1, 0.2),
        (-0.85, 0.2),
    ];
    let (sin, cos) = angle.sin_cos();
    let points: Vec<(f64, f64)> = unit
        .iter()
        .map(|(x, y)| {
            let (x, y) = (x * size * 0.5, y * size * 0.5);
            (center.x + x * cos - y * sin, center.y + x * sin + y * cos)
        })
        .collect();
    polygon(&points)
}

fn stroked(ctx: &mut RenderContext, path: &BezPath, color: Rgba8, width: f64) {
    ctx.set_paint(paint(color));
    ctx.set_stroke(Stroke::new(width.max(1.0)));
    ctx.stroke_path(path);
}

fn filled(ctx: &mut RenderContext, path: &BezPath, color: Rgba8) {
    ctx.set_paint(paint(color));
    ctx.fill_path(path);
}

/// The mark in a symbol icon: a cross, an exclamation mark or a tick.
fn mark(ctx: &mut RenderContext, which: u8, c: Point, r: f64, color: Rgba8, width: f64) {
    let mut path = BezPath::new();
    match which {
        0 => {
            path.move_to((c.x - r * 0.5, c.y - r * 0.5));
            path.line_to((c.x + r * 0.5, c.y + r * 0.5));
            path.move_to((c.x + r * 0.5, c.y - r * 0.5));
            path.line_to((c.x - r * 0.5, c.y + r * 0.5));
        }
        1 => {
            path.move_to((c.x, c.y - r * 0.6));
            path.line_to((c.x, c.y + r * 0.15));
            path.move_to((c.x, c.y + r * 0.5));
            path.line_to((c.x, c.y + r * 0.52));
        }
        _ => {
            path.move_to((c.x - r * 0.55, c.y));
            path.line_to((c.x - r * 0.1, c.y + r * 0.45));
            path.line_to((c.x + r * 0.6, c.y - r * 0.45));
        }
    }
    stroked(ctx, &path, color, width);
}

/// Draw `icon` in `rect` (physical pixels), a square or nearly.
pub fn draw_icon(ctx: &mut RenderContext, icon: Icon, rect: Rect) {
    let size = rect.width().min(rect.height());
    if size < 3.0 {
        return;
    }
    let c = Point::new(rect.center().x, rect.center().y);
    let r = size / 2.0;
    let count = icon.set.count;
    let index = icon.index.min(count - 1);
    let color = level(index, count);
    let line = (size / 12.0).max(1.0);
    match icon.set.family {
        IconFamily::Arrows | IconFamily::ArrowsGray => {
            let angles_deg: &[f64] = match count {
                3 => &[90.0, 0.0, -90.0],
                4 => &[90.0, 45.0, -45.0, -90.0],
                _ => &[90.0, 45.0, 0.0, -45.0, -90.0],
            };
            let angle = angles_deg[usize::from(index)].to_radians();
            let shade = if icon.set.family == IconFamily::ArrowsGray {
                GRAY
            } else {
                color
            };
            filled(ctx, &arrow(c, size * 0.95, angle), shade);
        }
        IconFamily::TrafficLights | IconFamily::TrafficLightsRimmed | IconFamily::RedToBlack => {
            let shade = if icon.set.family == IconFamily::RedToBlack {
                [BLACK, GRAY, PINK, RED][usize::from(index).min(3)]
            } else {
                color
            };
            let dot = Circle::new(c, r * 0.82).to_path(0.1);
            filled(ctx, &dot, shade);
            if icon.set.family == IconFamily::TrafficLightsRimmed {
                stroked(ctx, &dot, BLACK, line);
            }
        }
        IconFamily::Signs => match index {
            0 => filled(
                ctx,
                &polygon(&[
                    (c.x, c.y - r),
                    (c.x + r, c.y),
                    (c.x, c.y + r),
                    (c.x - r, c.y),
                ]),
                RED,
            ),
            1 => filled(
                ctx,
                &polygon(&[
                    (c.x, c.y - r),
                    (c.x + r, c.y + r * 0.8),
                    (c.x - r, c.y + r * 0.8),
                ]),
                YELLOW,
            ),
            _ => filled(ctx, &Circle::new(c, r * 0.85).to_path(0.1), GREEN),
        },
        IconFamily::Symbols => {
            filled(ctx, &Circle::new(c, r * 0.9).to_path(0.1), color);
            mark(ctx, index, c, r * 0.9, WHITE, line * 1.4);
        }
        IconFamily::SymbolsPlain => mark(ctx, index, c, r, color, line * 2.0),
        IconFamily::Flags => {
            let mut pole = BezPath::new();
            pole.move_to((c.x - r * 0.6, c.y - r));
            pole.line_to((c.x - r * 0.6, c.y + r));
            stroked(ctx, &pole, GRAY, line);
            filled(
                ctx,
                &polygon(&[
                    (c.x - r * 0.6, c.y - r * 0.9),
                    (c.x + r * 0.9, c.y - r * 0.4),
                    (c.x - r * 0.6, c.y + r * 0.2),
                ]),
                color,
            );
        }
        IconFamily::Stars => {
            let outline = star(c, r);
            filled(ctx, &outline, LIGHT);
            match index {
                0 => {}
                1 => {
                    ctx.push_clip_layer(&Rect::new(rect.x0, rect.y0, c.x, rect.y1).to_path(0.1));
                    filled(ctx, &outline, YELLOW);
                    ctx.pop_layer();
                }
                _ => filled(ctx, &outline, YELLOW),
            }
            stroked(ctx, &outline, GRAY, line * 0.7);
        }
        IconFamily::Triangles => match index {
            0 => filled(
                ctx,
                &polygon(&[
                    (c.x - r, c.y - r * 0.7),
                    (c.x + r, c.y - r * 0.7),
                    (c.x, c.y + r * 0.8),
                ]),
                RED,
            ),
            1 => filled(
                ctx,
                &Rect::new(c.x - r, c.y - r * 0.2, c.x + r, c.y + r * 0.2).to_path(0.1),
                YELLOW,
            ),
            _ => filled(
                ctx,
                &polygon(&[
                    (c.x - r, c.y + r * 0.7),
                    (c.x + r, c.y + r * 0.7),
                    (c.x, c.y - r * 0.8),
                ]),
                GREEN,
            ),
        },
        IconFamily::Quarters => {
            let share = f64::from(index) / f64::from(count - 1);
            let disc = Circle::new(c, r * 0.85).to_path(0.1);
            filled(ctx, &disc, WHITE);
            filled(ctx, &pie(c, r * 0.85, share), BLACK);
            stroked(ctx, &disc, BLACK, line);
        }
        IconFamily::Boxes => {
            let share = f64::from(index) / f64::from(count - 1);
            let outer = Rect::new(c.x - r * 0.8, c.y - r * 0.8, c.x + r * 0.8, c.y + r * 0.8);
            filled(ctx, &outer.to_path(0.1), WHITE);
            let top = outer.y1 - outer.height() * share;
            filled(
                ctx,
                &Rect::new(outer.x0, top, outer.x1, outer.y1).to_path(0.1),
                BLACK,
            );
            stroked(ctx, &outer.to_path(0.1), BLACK, line);
        }
        IconFamily::Rating => {
            let on = (f64::from(index) * 4.0 / f64::from(count - 1)).round() as u8;
            let width = size / 5.0;
            for bar in 0..4u8 {
                let x0 = rect.center().x - size / 2.0 + f64::from(bar) * size / 4.0 + width * 0.1;
                let height = size * (0.35 + 0.65 * f64::from(bar + 1) / 4.0);
                filled(
                    ctx,
                    &Rect::new(x0, c.y + r - height, x0 + width * 0.8, c.y + r).to_path(0.1),
                    if bar < on { BLACK } else { LIGHT },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_sheets::cond::IconSet;
    use vello_cpu::{Pixmap, Resources};

    fn painted(name: &str, index: u8) -> usize {
        let set = IconSet::parse(name).unwrap();
        let mut ctx = RenderContext::new(32, 32);
        let mut resources = Resources::new();
        let mut pixmap = Pixmap::new(32, 32);
        draw_icon(
            &mut ctx,
            Icon { set, index },
            Rect::new(4.0, 4.0, 28.0, 28.0),
        );
        ctx.flush();
        ctx.render(&mut pixmap, &mut resources);
        pixmap
            .data_as_u8_slice()
            .chunks(4)
            .filter(|p| p[3] != 0)
            .count()
    }

    #[test]
    fn every_icon_of_every_set_draws_something_inside_its_square() {
        for name in [
            "3Arrows",
            "3ArrowsGray",
            "4Arrows",
            "5Arrows",
            "3TrafficLights1",
            "3TrafficLights2",
            "4TrafficLights",
            "4RedToBlack",
            "3Signs",
            "3Symbols",
            "3Symbols2",
            "3Flags",
            "3Stars",
            "3Triangles",
            "5Quarters",
            "5Boxes",
            "4Rating",
            "5Rating",
        ] {
            let set = IconSet::parse(name).unwrap();
            for index in 0..set.count {
                let count = painted(name, index);
                // Inside the 24 by 24 square, and not nothing: the empty
                // star and the empty quarter are outlines.
                assert!(count > 10, "{name} {index} drew {count} pixels");
                assert!(count <= 24 * 24 + 80, "{name} {index} drew {count} pixels");
            }
        }
    }

    #[test]
    fn a_tiny_box_draws_nothing() {
        let set = IconSet::parse("3Arrows").unwrap();
        let mut ctx = RenderContext::new(8, 8);
        draw_icon(
            &mut ctx,
            Icon { set, index: 0 },
            Rect::new(0.0, 0.0, 2.0, 2.0),
        );
    }
}
