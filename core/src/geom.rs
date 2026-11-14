//! Small 2D math toolkit: points, affine transforms, bounding boxes and the
//! curve helpers (bulge arcs, elliptical arcs) shared by loading, measuring
//! and tessellation.

use serde::Serialize;
use std::f64::consts::{PI, TAU};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

pub const fn vec2(x: f64, y: f64) -> Vec2 {
    Vec2 { x, y }
}

impl Vec2 {
    pub const ZERO: Vec2 = vec2(0.0, 0.0);

    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }
    pub fn dot(self, o: Vec2) -> f64 {
        self.x * o.x + self.y * o.y
    }
    pub fn cross(self, o: Vec2) -> f64 {
        self.x * o.y - self.y * o.x
    }
    pub fn distance(self, o: Vec2) -> f64 {
        (self - o).length()
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
    pub fn lerp(self, o: Vec2, t: f64) -> Vec2 {
        self + (o - self) * t
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        vec2(self.x + o.x, self.y + o.y)
    }
}
impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        vec2(self.x - o.x, self.y - o.y)
    }
}
impl std::ops::Mul<f64> for Vec2 {
    type Output = Vec2;
    fn mul(self, s: f64) -> Vec2 {
        vec2(self.x * s, self.y * s)
    }
}

/// 2D affine transform: `p' = [a c; b d] * p + (tx, ty)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Affine2 {
    pub const IDENTITY: Affine2 = Affine2 {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn translate(t: Vec2) -> Self {
        Affine2 {
            tx: t.x,
            ty: t.y,
            ..Self::IDENTITY
        }
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Affine2 {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    pub fn rotate(angle_rad: f64) -> Self {
        let (s, c) = angle_rad.sin_cos();
        Affine2 {
            a: c,
            b: s,
            c: -s,
            d: c,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Linear map with the given images of the X and Y unit vectors.
    pub fn from_axes(x_axis: Vec2, y_axis: Vec2) -> Self {
        Affine2 {
            a: x_axis.x,
            b: x_axis.y,
            c: y_axis.x,
            d: y_axis.y,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// `self * rhs`: applies `rhs` first, then `self`.
    pub fn then_after(self, rhs: Affine2) -> Affine2 {
        Affine2 {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            tx: self.a * rhs.tx + self.c * rhs.ty + self.tx,
            ty: self.b * rhs.tx + self.d * rhs.ty + self.ty,
        }
    }

    pub fn point(&self, p: Vec2) -> Vec2 {
        vec2(
            self.a * p.x + self.c * p.y + self.tx,
            self.b * p.x + self.d * p.y + self.ty,
        )
    }

    /// Transform a direction (no translation).
    pub fn vector(&self, v: Vec2) -> Vec2 {
        vec2(self.a * v.x + self.c * v.y, self.b * v.x + self.d * v.y)
    }

    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// True when the transform keeps circles circular (rotation, uniform
    /// scale, translation, optionally mirrored).
    pub fn is_similarity(&self) -> bool {
        let x = vec2(self.a, self.b);
        let y = vec2(self.c, self.d);
        let (lx, ly) = (x.length(), y.length());
        let scale = lx.max(ly);
        if scale == 0.0 {
            return false;
        }
        (lx - ly).abs() <= 1e-9 * scale && x.dot(y).abs() <= 1e-9 * scale * scale
    }

    /// Length scale factor; exact for similarity transforms.
    pub fn uniform_scale(&self) -> f64 {
        self.determinant().abs().sqrt()
    }

    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.tx, self.ty]
            .iter()
            .all(|v| v.is_finite())
    }
}

/// DXF "arbitrary axis algorithm": the OCS X and Y axes, projected onto the
/// world XY plane, for an entity with the given extrusion direction.
///
/// For the default extrusion (0,0,1) this is the identity; for (0,0,-1) it
/// mirrors X, which is how mirrored arcs and inserts are commonly stored.
pub fn ocs_to_wcs(nx: f64, ny: f64, nz: f64) -> Affine2 {
    let len = (nx * nx + ny * ny + nz * nz).sqrt();
    if !len.is_finite() || len < 1e-12 {
        return Affine2::IDENTITY;
    }
    let (nx, ny, nz) = (nx / len, ny / len, nz / len);
    if nx.abs() < 1e-12 && ny.abs() < 1e-12 && nz > 0.0 {
        return Affine2::IDENTITY;
    }
    const LIMIT: f64 = 1.0 / 64.0;
    // Ax = (Wy or Wz) x N
    let (ax, ay, az) = if nx.abs() < LIMIT && ny.abs() < LIMIT {
        // Wy x N
        (nz, 0.0, -nx)
    } else {
        // Wz x N
        (-ny, nx, 0.0)
    };
    let al = (ax * ax + ay * ay + az * az).sqrt();
    let (ax, ay, az) = (ax / al, ay / al, az / al);
    // Ay = N x Ax
    let bx = ny * az - nz * ay;
    let by = nz * ax - nx * az;
    Affine2::from_axes(vec2(ax, ay), vec2(bx, by))
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BBox {
    pub min: Vec2,
    pub max: Vec2,
}

impl BBox {
    pub fn from_point(p: Vec2) -> Self {
        BBox { min: p, max: p }
    }

    pub fn from_corners(a: Vec2, b: Vec2) -> Self {
        BBox {
            min: vec2(a.x.min(b.x), a.y.min(b.y)),
            max: vec2(a.x.max(b.x), a.y.max(b.y)),
        }
    }

    pub fn expand(&mut self, p: Vec2) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }

    pub fn union(&mut self, o: &BBox) {
        self.expand(o.min);
        self.expand(o.max);
    }

    pub fn width(&self) -> f64 {
        self.max.x - self.min.x
    }
    pub fn height(&self) -> f64 {
        self.max.y - self.min.y
    }
    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }
    pub fn diagonal(&self) -> f64 {
        self.min.distance(self.max)
    }
    pub fn intersects(&self, o: &BBox) -> bool {
        self.min.x <= o.max.x
            && self.max.x >= o.min.x
            && self.min.y <= o.max.y
            && self.max.y >= o.min.y
    }
}

/// Accumulates an optional bounding box, ignoring non-finite points.
#[derive(Debug, Clone, Copy, Default)]
pub struct BBoxBuilder(pub Option<BBox>);

impl BBoxBuilder {
    pub fn add(&mut self, p: Vec2) {
        if !p.is_finite() {
            return;
        }
        match &mut self.0 {
            Some(b) => b.expand(p),
            None => self.0 = Some(BBox::from_point(p)),
        }
    }
    pub fn add_box(&mut self, b: &BBox) {
        self.add(b.min);
        self.add(b.max);
    }
}

/// A circular arc described by centre, radius and a signed sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BulgeArc {
    pub center: Vec2,
    pub radius: f64,
    pub start_angle: f64,
    /// Signed: positive is counter-clockwise.
    pub sweep: f64,
}

/// Convert a polyline segment with a bulge (`tan(sweep / 4)`) into an arc.
/// Returns `None` for straight or degenerate segments.
pub fn bulge_arc(a: Vec2, b: Vec2, bulge: f64) -> Option<BulgeArc> {
    if !bulge.is_finite() || bulge.abs() < 1e-12 {
        return None;
    }
    let chord = a.distance(b);
    if !chord.is_finite() || chord < 1e-300 {
        return None;
    }
    let sweep = 4.0 * bulge.atan();
    let radius = chord * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
    // Signed distance from the chord midpoint to the centre, along the left
    // normal of a->b.
    let h = chord * (1.0 - bulge * bulge) / (4.0 * bulge);
    let mid = a.lerp(b, 0.5);
    let dir = (b - a) * (1.0 / chord);
    let left = vec2(-dir.y, dir.x);
    let center = mid + left * h;
    let start_angle = (a.y - center.y).atan2(a.x - center.x);
    if !(radius.is_finite() && center.is_finite()) {
        return None;
    }
    Some(BulgeArc {
        center,
        radius,
        start_angle,
        sweep,
    })
}

/// Length of a polyline segment, taking its bulge into account.
pub fn bulge_segment_length(a: Vec2, b: Vec2, bulge: f64) -> f64 {
    match bulge_arc(a, b, bulge) {
        Some(arc) => arc.radius * arc.sweep.abs(),
        None => a.distance(b),
    }
}

/// Number of segments needed to flatten an arc of the given radius and sweep
/// so the chord error stays under `tolerance`.
pub fn arc_segments(radius: f64, sweep: f64, tolerance: f64) -> usize {
    let sweep = sweep.abs();
    if !(radius.is_finite() && sweep.is_finite()) || radius <= 0.0 || sweep <= 0.0 {
        return 1;
    }
    let step = if tolerance > 0.0 && tolerance < radius {
        2.0 * (1.0 - tolerance / radius).acos()
    } else {
        PI / 4.0
    };
    let step = step.clamp(TAU / 720.0, PI / 4.0);
    ((sweep / step).ceil() as usize).clamp(1, 720)
}

/// An elliptical arc in a form that is closed under affine transforms:
/// `p(t) = center + major * cos(t) + minor * sin(t)` for `t` in
/// `[start, start + sweep]` (sweep > 0). Circles and circular arcs are the
/// special case of perpendicular, equal-length axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EllipseArc {
    pub center: Vec2,
    pub major: Vec2,
    pub minor: Vec2,
    pub start: f64,
    pub sweep: f64,
}

impl EllipseArc {
    pub fn circle(center: Vec2, radius: f64) -> Self {
        EllipseArc {
            center,
            major: vec2(radius, 0.0),
            minor: vec2(0.0, radius),
            start: 0.0,
            sweep: TAU,
        }
    }

    /// Counter-clockwise circular arc from `start` to `end` (radians).
    pub fn circular(center: Vec2, radius: f64, start: f64, end: f64) -> Self {
        EllipseArc {
            center,
            major: vec2(radius, 0.0),
            minor: vec2(0.0, radius),
            start,
            sweep: ccw_sweep(start, end),
        }
    }

    pub fn point_at(&self, t: f64) -> Vec2 {
        let (s, c) = t.sin_cos();
        self.center + self.major * c + self.minor * s
    }

    pub fn start_point(&self) -> Vec2 {
        self.point_at(self.start)
    }
    pub fn end_point(&self) -> Vec2 {
        self.point_at(self.start + self.sweep)
    }

    pub fn is_closed(&self) -> bool {
        self.sweep >= TAU - 1e-9
    }

    /// The radius, when the arc is circular.
    pub fn radius(&self) -> Option<f64> {
        let (a, b) = (self.major.length(), self.minor.length());
        let m = a.max(b);
        if m > 0.0 && (a - b).abs() <= 1e-9 * m && self.major.dot(self.minor).abs() <= 1e-9 * m * m
        {
            Some(a)
        } else {
            None
        }
    }

    pub fn transformed(&self, m: &Affine2) -> Self {
        EllipseArc {
            center: m.point(self.center),
            major: m.vector(self.major),
            minor: m.vector(self.minor),
            start: self.start,
            sweep: self.sweep,
        }
    }

    pub fn length(&self) -> f64 {
        if let Some(r) = self.radius() {
            return r * self.sweep;
        }
        // Simpson's rule on the speed |p'(t)|.
        let n = 256;
        let h = self.sweep / n as f64;
        let speed = |t: f64| {
            let (s, c) = t.sin_cos();
            (self.minor * c - self.major * s).length()
        };
        let mut sum = speed(self.start) + speed(self.start + self.sweep);
        for i in 1..n {
            let w = if i % 2 == 1 { 4.0 } else { 2.0 };
            sum += w * speed(self.start + h * i as f64);
        }
        sum * h / 3.0
    }

    pub fn bbox(&self) -> BBox {
        let mut b = BBox::from_point(self.start_point());
        b.expand(self.end_point());
        // Extrema of each coordinate: t = atan2(minor, major) (+ pi).
        for (mj, mn) in [(self.major.x, self.minor.x), (self.major.y, self.minor.y)] {
            let t0 = mn.atan2(mj);
            for t in [t0, t0 + PI] {
                if self.contains_param(t) {
                    b.expand(self.point_at(t));
                }
            }
        }
        b
    }

    fn contains_param(&self, t: f64) -> bool {
        if self.is_closed() {
            return true;
        }
        (t - self.start).rem_euclid(TAU) <= self.sweep
    }

    /// Points along the arc, including both ends.
    pub fn flatten(&self, tolerance: f64) -> Vec<Vec2> {
        let r = self.major.length().max(self.minor.length());
        let n = arc_segments(r, self.sweep, tolerance);
        (0..=n)
            .map(|i| self.point_at(self.start + self.sweep * i as f64 / n as f64))
            .collect()
    }
}

/// Counter-clockwise sweep from `start` to `end`, in `(0, TAU]`.
pub fn ccw_sweep(start: f64, end: f64) -> f64 {
    let s = (end - start).rem_euclid(TAU);
    if s < 1e-12 { TAU } else { s }
}

/// Total length of an open or closed point chain.
pub fn chain_length(points: &[Vec2], closed: bool) -> f64 {
    let mut len: f64 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    if closed && points.len() > 2 {
        len += points[points.len() - 1].distance(points[0]);
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }
    fn pclose(a: Vec2, b: Vec2) -> bool {
        close(a.x, b.x) && close(a.y, b.y)
    }

    #[test]
    fn bbox_accumulates_and_ignores_nan() {
        let mut b = BBoxBuilder::default();
        assert!(b.0.is_none());
        b.add(vec2(1.0, 2.0));
        b.add(vec2(-3.0, 5.0));
        b.add(vec2(f64::NAN, 100.0));
        b.add(vec2(f64::INFINITY, 0.0));
        let bb = b.0.unwrap();
        assert_eq!(bb.min, vec2(-3.0, 2.0));
        assert_eq!(bb.max, vec2(1.0, 5.0));
        assert!(close(bb.width(), 4.0) && close(bb.height(), 3.0));
        assert!(close(bb.diagonal(), 5.0));
        assert_eq!(bb.center(), vec2(-1.0, 3.5));
    }

    #[test]
    fn bbox_intersection() {
        let a = BBox::from_corners(vec2(0.0, 0.0), vec2(2.0, 2.0));
        let b = BBox::from_corners(vec2(3.0, 3.0), vec2(1.0, 1.0));
        let c = BBox::from_corners(vec2(5.0, 5.0), vec2(6.0, 6.0));
        assert!(a.intersects(&b));
        assert!(!a.intersects(&c));
    }

    #[test]
    fn bulge_one_is_a_ccw_semicircle() {
        let arc = bulge_arc(vec2(0.0, 0.0), vec2(2.0, 0.0), 1.0).unwrap();
        assert!(pclose(arc.center, vec2(1.0, 0.0)));
        assert!(close(arc.radius, 1.0));
        assert!(close(arc.sweep, PI));
        // CCW from (0,0) to (2,0) passes through the bottom point (1,-1).
        let mid = arc.start_angle + arc.sweep / 2.0;
        let p = arc.center + vec2(mid.cos(), mid.sin()) * arc.radius;
        assert!(pclose(p, vec2(1.0, -1.0)));
        assert!(close(
            bulge_segment_length(vec2(0.0, 0.0), vec2(2.0, 0.0), 1.0),
            PI
        ));
    }

    #[test]
    fn negative_bulge_is_clockwise() {
        let arc = bulge_arc(vec2(0.0, 0.0), vec2(2.0, 0.0), -1.0).unwrap();
        assert!(close(arc.sweep, -PI));
        let mid = arc.start_angle + arc.sweep / 2.0;
        let p = arc.center + vec2(mid.cos(), mid.sin()) * arc.radius;
        assert!(pclose(p, vec2(1.0, 1.0)));
    }

    #[test]
    fn quarter_bulge() {
        // tan(90deg / 4): quarter circle of radius 1 from (1,0) to (0,1).
        let bulge = (PI / 8.0).tan();
        let arc = bulge_arc(vec2(1.0, 0.0), vec2(0.0, 1.0), bulge).unwrap();
        assert!(pclose(arc.center, vec2(0.0, 0.0)));
        assert!(close(arc.radius, 1.0));
        assert!(close(arc.sweep, PI / 2.0));
    }

    #[test]
    fn degenerate_bulges_are_straight() {
        assert!(bulge_arc(vec2(0.0, 0.0), vec2(1.0, 0.0), 0.0).is_none());
        assert!(bulge_arc(vec2(0.0, 0.0), vec2(0.0, 0.0), 1.0).is_none());
        assert!(bulge_arc(vec2(0.0, 0.0), vec2(1.0, 0.0), f64::NAN).is_none());
        assert!(close(
            bulge_segment_length(vec2(0.0, 0.0), vec2(3.0, 4.0), 0.0),
            5.0
        ));
    }

    #[test]
    fn arc_segment_count_respects_tolerance() {
        let n = arc_segments(100.0, TAU, 0.01);
        let step = TAU / n as f64;
        let err = 100.0 * (1.0 - (step / 2.0).cos());
        assert!(err <= 0.01 + 1e-12, "chord error {err}");
        assert!(arc_segments(1.0, TAU, 0.0) >= 8);
        assert_eq!(arc_segments(f64::NAN, 1.0, 0.1), 1);
        assert!(arc_segments(1e9, TAU, 1e-9) <= 720);
    }

    #[test]
    fn affine_composition_order() {
        // Insert transform: scale, then rotate, then translate.
        let m = Affine2::translate(vec2(10.0, 0.0))
            .then_after(Affine2::rotate(PI / 2.0))
            .then_after(Affine2::scale(2.0, 2.0));
        assert!(pclose(m.point(vec2(1.0, 0.0)), vec2(10.0, 2.0)));
        assert!(m.is_similarity());
        assert!(close(m.uniform_scale(), 2.0));
        assert!(!Affine2::scale(1.0, 2.0).is_similarity());
        assert!(Affine2::scale(-1.0, 1.0).is_similarity());
        assert!(Affine2::scale(-1.0, 1.0).determinant() < 0.0);
    }

    #[test]
    fn ocs_default_and_flipped() {
        assert_eq!(ocs_to_wcs(0.0, 0.0, 1.0), Affine2::IDENTITY);
        let m = ocs_to_wcs(0.0, 0.0, -1.0);
        assert!(pclose(m.point(vec2(1.0, 2.0)), vec2(-1.0, 2.0)));
        assert_eq!(ocs_to_wcs(0.0, 0.0, 0.0), Affine2::IDENTITY);
    }

    #[test]
    fn ellipse_arc_circle_properties() {
        let c = EllipseArc::circle(vec2(1.0, 1.0), 2.0);
        assert_eq!(c.radius(), Some(2.0));
        assert!(close(c.length(), TAU * 2.0));
        let b = c.bbox();
        assert!(pclose(b.min, vec2(-1.0, -1.0)) && pclose(b.max, vec2(3.0, 3.0)));
        assert!(c.is_closed());
    }

    #[test]
    fn quarter_arc_bbox_and_length() {
        let a = EllipseArc::circular(vec2(0.0, 0.0), 1.0, 0.0, PI / 2.0);
        let b = a.bbox();
        assert!(pclose(b.min, vec2(0.0, 0.0)) && pclose(b.max, vec2(1.0, 1.0)));
        assert!(close(a.length(), PI / 2.0));
        // Arc crossing 0 degrees: 315 -> 45.
        let a = EllipseArc::circular(vec2(0.0, 0.0), 1.0, -PI / 4.0, PI / 4.0);
        assert!(close(a.sweep, PI / 2.0));
        assert!(close(a.bbox().max.x, 1.0));
    }

    #[test]
    fn ellipse_under_nonuniform_scale() {
        let e = EllipseArc::circle(vec2(0.0, 0.0), 1.0).transformed(&Affine2::scale(2.0, 1.0));
        assert_eq!(e.radius(), None);
        let b = e.bbox();
        assert!(pclose(b.min, vec2(-2.0, -1.0)) && pclose(b.max, vec2(2.0, 1.0)));
        // Ramanujan's approximation for a=2, b=1 is 9.688448...
        assert!((e.length() - 9.688_448_220_5).abs() < 1e-6);
    }

    #[test]
    fn flatten_hits_both_ends() {
        let a = EllipseArc::circular(vec2(0.0, 0.0), 5.0, 0.0, PI);
        let pts = a.flatten(0.01);
        assert!(pclose(pts[0], vec2(5.0, 0.0)));
        assert!(pclose(*pts.last().unwrap(), vec2(-5.0, 0.0)));
        assert!(pts.len() > 8);
    }

    #[test]
    fn chain_lengths() {
        let sq = [
            vec2(0.0, 0.0),
            vec2(1.0, 0.0),
            vec2(1.0, 1.0),
            vec2(0.0, 1.0),
        ];
        assert!(close(chain_length(&sq, false), 3.0));
        assert!(close(chain_length(&sq, true), 4.0));
    }
}
