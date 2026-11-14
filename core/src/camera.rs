//! 2D view transform shared by the on-screen and offscreen renderers.

use crate::geom::{BBox, Vec2, vec2};

const MIN_ZOOM: f64 = 1e-12;
const MAX_ZOOM: f64 = 1e12;

/// World-to-screen mapping: `center` is the world point at the middle of the
/// viewport and `zoom` is the number of pixels per drawing unit. World Y
/// points up; screen Y points down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub center: Vec2,
    pub zoom: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            center: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl Camera {
    /// Frame `bbox` in a viewport of `size` pixels, leaving `margin` (a
    /// fraction of the viewport, e.g. 0.05) free on every side.
    pub fn fit(bbox: &BBox, size: (f64, f64), margin: f64) -> Camera {
        let usable = (1.0 - 2.0 * margin).clamp(0.1, 1.0);
        let (w, h) = (bbox.width(), bbox.height());
        let zx = if w > 0.0 {
            size.0 * usable / w
        } else {
            f64::INFINITY
        };
        let zy = if h > 0.0 {
            size.1 * usable / h
        } else {
            f64::INFINITY
        };
        let mut zoom = zx.min(zy);
        if !zoom.is_finite() || zoom <= 0.0 {
            // A single point (or an empty viewport): pick a neutral scale.
            zoom = 1.0;
        }
        Camera {
            center: bbox.center(),
            zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM),
        }
    }

    /// Screen position in pixels (origin top-left) of a world point.
    pub fn world_to_screen(&self, p: Vec2, size: (f64, f64)) -> Vec2 {
        vec2(
            size.0 / 2.0 + (p.x - self.center.x) * self.zoom,
            size.1 / 2.0 - (p.y - self.center.y) * self.zoom,
        )
    }

    pub fn screen_to_world(&self, s: Vec2, size: (f64, f64)) -> Vec2 {
        vec2(
            self.center.x + (s.x - size.0 / 2.0) / self.zoom,
            self.center.y - (s.y - size.1 / 2.0) / self.zoom,
        )
    }

    /// Zoom by `factor`, keeping the world point under `anchor` (pixels)
    /// fixed on screen.
    pub fn zoom_about(&mut self, anchor: Vec2, factor: f64, size: (f64, f64)) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let before = self.screen_to_world(anchor, size);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.screen_to_world(anchor, size);
        self.center = self.center + (before - after);
    }

    /// Pan by a screen-space delta in pixels (content follows the pointer).
    pub fn pan(&mut self, delta_px: Vec2) {
        self.center.x -= delta_px.x / self.zoom;
        self.center.y += delta_px.y / self.zoom;
    }

    /// The world rectangle visible in a viewport of `size` pixels.
    pub fn visible_bbox(&self, size: (f64, f64)) -> BBox {
        let half = vec2(size.0 / 2.0 / self.zoom, size.1 / 2.0 / self.zoom);
        BBox {
            min: self.center - half,
            max: self.center + half,
        }
    }

    /// Scale and offset mapping mesh-local coordinates (relative to
    /// `origin`) to normalised device coordinates.
    pub fn ndc_transform(&self, origin: Vec2, size: (f64, f64)) -> ([f32; 2], [f32; 2]) {
        let sx = 2.0 * self.zoom / size.0.max(1.0);
        let sy = 2.0 * self.zoom / size.1.max(1.0);
        let off = origin - self.center;
        (
            [sx as f32, sy as f32],
            [(off.x * sx) as f32, (off.y * sy) as f32],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pclose(a: Vec2, b: Vec2) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn fit_centres_and_scales() {
        let b = BBox::from_corners(vec2(0.0, 0.0), vec2(200.0, 50.0));
        let cam = Camera::fit(&b, (1000.0, 500.0), 0.05);
        assert_eq!(cam.center, vec2(100.0, 25.0));
        // Width is the limiting dimension: 900 px for 200 units.
        assert!((cam.zoom - 4.5).abs() < 1e-12);
        let size = (1000.0, 500.0);
        assert!(pclose(
            cam.world_to_screen(vec2(0.0, 25.0), size),
            vec2(50.0, 250.0)
        ));
        // Y is flipped: larger world Y is higher on screen.
        assert!(cam.world_to_screen(vec2(0.0, 50.0), size).y < 250.0);
    }

    #[test]
    fn fit_handles_degenerate_boxes() {
        let p = BBox::from_point(vec2(3.0, 4.0));
        let cam = Camera::fit(&p, (800.0, 600.0), 0.05);
        assert_eq!(cam.center, vec2(3.0, 4.0));
        assert!(cam.zoom.is_finite() && cam.zoom > 0.0);
        let hline = BBox::from_corners(vec2(0.0, 1.0), vec2(10.0, 1.0));
        let cam = Camera::fit(&hline, (800.0, 600.0), 0.0);
        assert!((cam.zoom - 80.0).abs() < 1e-9);
    }

    #[test]
    fn screen_world_round_trip() {
        let cam = Camera {
            center: vec2(12.5, -7.0),
            zoom: 3.25,
        };
        let size = (640.0, 480.0);
        let w = vec2(100.0, 42.0);
        assert!(pclose(
            cam.screen_to_world(cam.world_to_screen(w, size), size),
            w
        ));
    }

    #[test]
    fn zoom_keeps_the_anchor_fixed() {
        let mut cam = Camera {
            center: vec2(0.0, 0.0),
            zoom: 2.0,
        };
        let size = (800.0, 600.0);
        let anchor = vec2(650.0, 120.0);
        let world = cam.screen_to_world(anchor, size);
        cam.zoom_about(anchor, 1.7, size);
        assert!((cam.zoom - 3.4).abs() < 1e-12);
        assert!(pclose(cam.world_to_screen(world, size), anchor));
        cam.zoom_about(anchor, f64::NAN, size);
        cam.zoom_about(anchor, 0.0, size);
        assert!((cam.zoom - 3.4).abs() < 1e-12);
    }

    #[test]
    fn pan_moves_content_with_the_pointer() {
        let mut cam = Camera {
            center: vec2(0.0, 0.0),
            zoom: 2.0,
        };
        let size = (800.0, 600.0);
        let before = cam.world_to_screen(vec2(5.0, 5.0), size);
        cam.pan(vec2(30.0, -10.0));
        let after = cam.world_to_screen(vec2(5.0, 5.0), size);
        assert!(pclose(after - before, vec2(30.0, -10.0)));
    }

    #[test]
    fn ndc_transform_maps_view_edges() {
        let cam = Camera {
            center: vec2(1000.0, 2000.0),
            zoom: 2.0,
        };
        let size = (800.0, 600.0);
        let origin = vec2(990.0, 1990.0);
        let (scale, offset) = cam.ndc_transform(origin, size);
        // The camera centre, in mesh-local coordinates, maps to NDC (0, 0).
        let local = cam.center - origin;
        assert!((local.x as f32 * scale[0] + offset[0]).abs() < 1e-6);
        assert!((local.y as f32 * scale[1] + offset[1]).abs() < 1e-6);
        // The right edge of the view maps to NDC x = 1.
        let right = cam.visible_bbox(size).max.x - origin.x;
        assert!((right as f32 * scale[0] + offset[0] - 1.0).abs() < 1e-6);
    }
}
