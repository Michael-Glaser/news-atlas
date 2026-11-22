//! The mark: a world with a small graph inside it.
//!
//! ATLAS is a world map and a graph of what happened where, so the mark is that one
//! sentence and nothing else: the world's outline, one hub, two linked nodes, and the
//! two edges that join them. It is deliberately *not* a globe with a grid on it — a
//! crossed sphere is the icon every map app already has, and it says nothing about the
//! half of this product that matters, which is that entities are connected.
//!
//! Three decisions worth keeping:
//!
//! * **No text in the mark.** A wordmark set in the product's own light face sits next
//!   to this in the rail; baking letters into a 32 px icon is what makes an icon read as
//!   mush.
//! * **The hub is the only solid shape.** Everything else is a line or an outline, so
//!   the eye lands on "the thing that happened" first at every size.
//! * **Every channel is equal.** The window is monochrome by rule, and the dock icon is
//!   drawn from the same greys, so [`icon`] writes `r == g == b` for every pixel. A test
//!   asserts it, because one tinted pixel is a violation nobody notices by eye.
//!
//! The same geometry is expressed twice on purpose: [`paint`] draws it with the egui
//! painter (vector, crisp at any rail size), and [`icon`] rasterises it with 4×4
//! supersampling for the window manager, which wants pixels rather than shapes. Both
//! read [`Geometry`], so the two cannot drift apart by a coordinate.

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

/// The plate behind the mark, for the dock/taskbar icon only. Equal channels: grey.
const PLATE: u8 = 14;
/// The drawing itself. Not pure white: the plate is the blackest thing, so the strokes
/// read as ink on it rather than as light.
const INK: u8 = 238;

/// The smallest size that can show a leaf as an outline rather than a dot, in pixels.
///
/// Below this the ring around a leaf is under a pixel thick and the mark turns to
/// porridge, so the leaves become solid. The hub is always solid: it is the subject.
const OUTLINE_MIN: f32 = 22.0;

/// Where every piece of the mark sits, as fractions of the mark's own box.
///
/// One struct, in fractions, is the whole reason the vector mark and the raster icon are
/// the same mark: both multiply these numbers by their own size.
struct Geometry {
    /// Radius of the world, as a fraction of the box.
    radius: f32,
    /// Stroke width of the world's outline.
    outline: f32,
    /// The hub: the one solid shape, and the middle of the graph.
    hub: Vec2,
    hub_radius: f32,
    /// The two leaves the hub is connected to, and how far out they sit.
    leaves: [Vec2; 3],
    /// Three different radii, largest nearest the hub: a graph has a hierarchy, and equal
    /// nodes read as a molecule.
    leaf_radius: [f32; 3],
    /// Stroke width of an edge and of a leaf's outline.
    edge: f32,
    leaf_outline: f32,
}

impl Geometry {
    fn new() -> Geometry {
        Geometry {
            radius: 0.320,
            outline: 0.038,
            // Left of centre and slightly low, so the graph has room to lean right —
            // a centred hub with symmetric leaves reads as a molecule, not a map.
            hub: Vec2::new(-0.040, 0.020),
            hub_radius: 0.070,
            // Three leaves at three distances: two would read as the share glyph, and
            // equal distances read as a molecule. Three uneven ones read as a graph.
            leaves: [
                Vec2::new(0.195, -0.185),
                Vec2::new(0.240, 0.075),
                Vec2::new(0.065, 0.235),
            ],
            leaf_radius: [0.050, 0.042, 0.036],
            edge: 0.026,
            leaf_outline: 0.026,
        }
    }

    /// Should a leaf be drawn as an outline at this size?
    fn outlined(&self, size: f32) -> bool {
        size >= OUTLINE_MIN
    }

    /// The point on a leaf's rim nearest the hub: an edge should stop at the node, not
    /// run under it, so a leaf drawn as an outline does not have a line crossing it.
    ///
    /// `size` is the mark's box, so the answer is in the mark's own pixels; pass `1.0` to
    /// get it in fractions of the box (which is what the raster pass wants).
    fn rim_toward_hub(&self, leaf: usize, size: f32) -> Vec2 {
        let centre = self.leaves[leaf];
        let away = (centre - self.hub).normalized();
        (centre - away * self.leaf_radius[leaf]) * size
    }
}

// ---------------------------------------------------------------------------------------
// Vector: the rail's mark, and anywhere else the window wants to sign itself.
// ---------------------------------------------------------------------------------------

/// Draw the mark inside `rect`, in `colour`, leaving a transparent margin.
///
/// `rect` is square by contract (the caller passes a square allocation); a non-square box
/// is not corrected here, because squashing the world is the one thing a map app's logo
/// must never do — the caller's layout is the bug, and it stays visible.
pub fn paint(painter: &Painter, rect: Rect, colour: Color32) -> Rect {
    let size = rect.width().min(rect.height());
    let centre = rect.center();
    let geo = Geometry::new();
    let at = |point: Vec2| Pos2::new(centre.x, centre.y) + point * size;
    let outlined = geo.outlined(size);

    painter.circle_stroke(centre, geo.radius * size, Stroke::new(geo.outline * size, colour));

    for leaf in 0..geo.leaves.len() {
        painter.line_segment(
            [at(geo.hub), at(geo.rim_toward_hub(leaf, 1.0))],
            Stroke::new(geo.edge * size, colour),
        );
    }

    painter.circle_filled(at(geo.hub), geo.hub_radius * size, colour);

    for leaf in 0..geo.leaves.len() {
        let radius = geo.leaf_radius[leaf] * size;
        if outlined {
            painter.circle_stroke(at(geo.leaves[leaf]), radius, Stroke::new(geo.leaf_outline * size, colour));
        } else {
            painter.circle_filled(at(geo.leaves[leaf]), radius, colour);
        }
    }

    rect
}

// ---------------------------------------------------------------------------------------
// Raster: the icon the window manager shows.
// ---------------------------------------------------------------------------------------

/// The window icon: a rounded plate with the mark on it, `px` pixels square.
///
/// Rasterised here rather than shipped as a file for two reasons: a PNG in the tree is one
/// more thing that can go missing on a fresh machine, and the icon must agree with the
/// vector mark — generating it from [`Geometry`] makes disagreement impossible.
pub fn icon(px: u32) -> egui::IconData {
    let size = px.max(16);
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    let geo = Geometry::new();
    let side = size as f32;
    let half = side / 2.0;

    for y in 0..size {
        for x in 0..size {
            // Coverage from 4×4 samples inside the pixel: the plate's corners and every
            // curved stroke get four levels of edge instead of a staircase.
            const SAMPLES: u32 = 4;
            let mut plate = 0.0;
            let mut mark = 0.0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px_x = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let px_y = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    if plate_distance(px_x - half, px_y - half, half) <= 0.0 {
                        plate += 1.0;
                    }
                    if mark_distance(px_x - half, px_y - half, side, &geo) <= 0.0 {
                        mark += 1.0;
                    }
                }
            }
            let total = (SAMPLES * SAMPLES) as f32;
            let (plate, mark) = (plate / total, mark / total);

            // The mark only exists inside the plate: clipping it there is what stops a
            // bright edge from floating outside the rounded corner.
            let grey = mix(PLATE as f32, INK as f32, mark.min(plate));
            let index = ((y * size + x) * 4) as usize;
            rgba[index] = grey.round() as u8;
            rgba[index + 1] = grey.round() as u8;
            rgba[index + 2] = grey.round() as u8;
            rgba[index + 3] = (plate * 255.0).round() as u8;
        }
    }

    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

/// Signed distance to the icon's plate: a square with noticeably rounded corners, which
/// is the shape both macOS and Windows expect a window icon to have. Negative inside.
fn plate_distance(x: f32, y: f32, half: f32) -> f32 {
    let radius = half * 0.47;
    let qx = x.abs() - (half - radius);
    let qy = y.abs() - (half - radius);
    let outside = Vec2::new(qx.max(0.0), qy.max(0.0)).length();
    outside + qx.max(qy).min(0.0) - radius
}

/// Signed distance to the nearest piece of the mark: negative inside a stroke or a node.
///
/// Adding the pieces with `min` is the union of the drawing, which is what a coverage test
/// needs — an edge runs from the hub to a leaf's rim and touches both, and counting the
/// overlap twice would print a darker patch at every junction.
fn mark_distance(x: f32, y: f32, size: f32, geo: &Geometry) -> f32 {
    let point = Vec2::new(x, y);
    let ring = (point.length() - geo.radius * size).abs() - geo.outline * size / 2.0;
    let hub = (point - geo.hub * size).length() - geo.hub_radius * size;

    let mut nearest = ring.min(hub);

    for leaf in 0..geo.leaves.len() {
        let centre = geo.leaves[leaf] * size;
        let radius = geo.leaf_radius[leaf] * size;
        let rim = geo.rim_toward_hub(leaf, size);
        let edge = segment_distance(point, geo.hub * size, rim) - geo.edge * size / 2.0;
        let node = if geo.outlined(size) {
            // An outline: the rim, so the inside of the leaf is as far away as the outside.
            ((point - centre).length() - radius).abs() - geo.leaf_outline * size / 2.0
        } else {
            (point - centre).length() - radius
        };
        nearest = nearest.min(edge).min(node);
    }

    nearest
}

/// Distance from `p` to the segment `a`–`b`, the usual clamped projection.
fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length_squared = dx * dx + dy * dy;
    let t = if length_squared > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / length_squared).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - Vec2::new(a.x + t * dx, a.y + t * dy)).length()
}

fn mix(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pixel_of_the_icon_is_grey() {
        let icon = icon(64);
        for (index, chunk) in icon.rgba.chunks_exact(4).enumerate() {
            assert!(
                chunk[0] == chunk[1] && chunk[1] == chunk[2],
                "pixel {index} is tinted: {chunk:?} — the window is monochrome by rule"
            );
        }
    }

    #[test]
    fn the_corners_are_empty_and_the_plate_is_solid() {
        let icon = icon(64);
        let alpha = |x: u32, y: u32| icon.rgba[((y * 64 + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "the corner is outside the rounded plate");
        assert_eq!(alpha(63, 63), 0, "the corner is outside the rounded plate");
        assert_eq!(alpha(32, 4), 255, "the plate's top edge should be solid");
        assert_eq!(alpha(32, 32), 255, "the middle of the plate should be solid");
    }

    #[test]
    fn the_mark_is_drawn_somewhere_and_does_not_fill_the_plate() {
        let icon = icon(128);
        let light = icon
            .rgba
            .chunks_exact(4)
            .filter(|pixel| pixel[0] as i32 > PLATE as i32 + 40)
            .count();
        let total = (128 * 128) as usize;
        assert!(light > total / 60, "the mark is missing: {light} light pixels");
        assert!(
            light < total / 3,
            "the mark has taken over the plate: {light} of {total} light pixels"
        );
    }

    #[test]
    fn the_hub_is_solid_and_the_world_inside_it_is_empty() {
        let size = 256.0;
        let geo = Geometry::new();
        let point = |p: Vec2| mark_distance(p.x * size, p.y * size, size, &geo);
        // The hub is ink…
        assert!(point(geo.hub) < 0.0, "the hub must be solid");
        // …and the middle of the world, between the hub and the ring, is not: the mark is
        // line work with one solid shape, not a filled disc.
        assert!(point(Vec2::new(0.0, -0.22)) > 0.0, "the world must be empty inside");
        // The world's own outline is ink.
        assert!(point(Vec2::new(0.0, -geo.radius)) < 0.0, "the ring is missing");
    }

    #[test]
    fn the_leaves_are_outlines_at_full_size_and_solid_dots_when_small() {
        let geo = Geometry::new();
        let size = 256.0;
        let leaf = geo.leaves[0];
        assert!(
            mark_distance(leaf.x * size, leaf.y * size, size, &geo) > 0.0,
            "the middle of an outlined leaf must be empty"
        );
        // The rim itself is ink, one leaf-radius away from the centre.
        let on_the_rim = leaf + Vec2::new(geo.leaf_radius[0], 0.0);
        assert!(
            mark_distance(on_the_rim.x * size, on_the_rim.y * size, size, &geo) < 0.0,
            "the leaf's rim is missing at {on_the_rim:?}"
        );
        // Below the outline threshold the leaf is filled, so its centre is ink.
        let small = 16.0;
        assert!(!geo.outlined(small));
        assert!(mark_distance(leaf.x * small, leaf.y * small, small, &geo) < 0.0);
    }

    #[test]
    fn every_node_sits_inside_the_world_it_is_drawn_on() {
        let geo = Geometry::new();
        for leaf in 0..geo.leaves.len() {
            assert!(
                geo.leaves[leaf].length() + geo.leaf_radius[leaf] < geo.radius,
                "a leaf crossed the world's outline: {:?}",
                geo.leaves[leaf]
            );
        }
        assert!(geo.hub.length() + geo.hub_radius < geo.radius);
    }

    #[test]
    fn an_edge_stops_at_the_rim_it_reaches() {
        let geo = Geometry::new();
        let size = 100.0;
        for leaf in 0..geo.leaves.len() {
            let rim = geo.rim_toward_hub(leaf, size);
            let distance_to_centre = (rim - geo.leaves[leaf] * size).length();
            assert!(
                (distance_to_centre - geo.leaf_radius[leaf] * size).abs() < 1e-3,
                "the edge stops {distance_to_centre} from the leaf, not on its rim"
            );
        }
    }

    #[test]
    fn the_icon_is_the_size_it_was_asked_for_and_repeatable() {
        let icon = icon(48);
        assert_eq!((icon.width, icon.height), (48, 48));
        assert_eq!(icon.rgba.len(), 48 * 48 * 4);
        assert_eq!(icon.rgba, super::icon(48).rgba, "the icon must not depend on chance");
        // A tiny request is raised to the smallest size the mark survives, never zero.
        assert_eq!(super::icon(2).width, 16);
    }

    #[test]
    fn a_small_mark_keeps_the_world_the_hub_and_both_leaves() {
        let size = 16.0;
        let geo = Geometry::new();
        let point = |p: Vec2| mark_distance(p.x * size, p.y * size, size, &geo);
        assert!(point(Vec2::new(0.0, -geo.radius)) < 0.0, "the ring is missing");
        assert!(point(geo.hub) < 0.0, "the hub is missing");
        assert!(point(geo.leaves[0]) < 0.0, "a leaf is missing");
        assert!(point(geo.leaves[1]) < 0.0, "a leaf is missing");
    }
}
