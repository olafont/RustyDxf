// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! NURBS evaluation (de Boor) and fit-point interpolation for SPLINE entities.

use crate::geom::{Vec2, vec2};
use crate::model::Spline;

/// Upper bound on the points produced for one spline.
const MAX_POINTS: usize = 20_000;

/// Flatten a spline into a point chain. `samples_per_span` is the number of
/// segments used for each knot span (or each fit-point interval).
///
/// Malformed splines (wrong knot count, bad weights, non-finite values) are
/// repaired where possible and otherwise degrade to their control polygon, so
/// this never panics.
pub fn flatten_spline(s: &Spline, samples_per_span: usize) -> Vec<Vec2> {
    let samples = samples_per_span.clamp(1, 256);
    let mut pts = if s.control_points.len() >= 2 {
        flatten_nurbs(s, samples)
    } else if s.fit_points.len() >= 2 {
        flatten_fit_points(&s.fit_points, s.closed, samples)
    } else {
        Vec::new()
    };
    pts.retain(|p| p.is_finite());
    pts
}

fn flatten_nurbs(s: &Spline, samples: usize) -> Vec<Vec2> {
    let ctrl = &s.control_points;
    let n = ctrl.len();
    let degree = s.degree.clamp(1, n - 1).min(16);

    let weights_ok = s.weights.len() == n && s.weights.iter().all(|w| w.is_finite() && *w > 0.0);
    let weight = |i: usize| if weights_ok { s.weights[i] } else { 1.0 };

    let knots_ok = degree == s.degree
        && s.knots.len() == n + degree + 1
        && s.knots.iter().all(|k| k.is_finite())
        && s.knots.windows(2).all(|w| w[0] <= w[1])
        && s.knots[degree] < s.knots[n];
    let knots: Vec<f64> = if knots_ok {
        s.knots.clone()
    } else {
        clamped_uniform_knots(n, degree)
    };

    // Non-empty knot spans inside the valid parameter range.
    let spans: Vec<usize> = (degree..n).filter(|&i| knots[i] < knots[i + 1]).collect();
    if spans.is_empty() {
        return ctrl.clone();
    }
    let samples = samples.min((MAX_POINTS / spans.len()).max(1));

    // Homogeneous control points.
    let hom: Vec<[f64; 3]> = (0..n)
        .map(|i| {
            let w = weight(i);
            [ctrl[i].x * w, ctrl[i].y * w, w]
        })
        .collect();

    let mut out = Vec::with_capacity(spans.len() * samples + 1);
    let mut scratch = vec![[0.0; 3]; degree + 1];
    for &span in &spans {
        let (u0, u1) = (knots[span], knots[span + 1]);
        for j in 0..samples {
            let u = u0 + (u1 - u0) * j as f64 / samples as f64;
            out.push(de_boor(&hom, &knots, degree, span, u, &mut scratch));
        }
    }
    let last = *spans.last().unwrap_or(&degree);
    out.push(de_boor(
        &hom,
        &knots,
        degree,
        last,
        knots[last + 1],
        &mut scratch,
    ));
    out
}

fn clamped_uniform_knots(n: usize, degree: usize) -> Vec<f64> {
    let interior = n - degree; // number of spans
    let mut k = vec![0.0; degree + 1];
    k.extend((1..interior).map(|i| i as f64));
    k.extend(std::iter::repeat_n(interior as f64, degree + 1));
    k
}

fn de_boor(
    hom: &[[f64; 3]],
    knots: &[f64],
    degree: usize,
    span: usize,
    u: f64,
    d: &mut [[f64; 3]],
) -> Vec2 {
    for j in 0..=degree {
        d[j] = hom[span - degree + j];
    }
    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = span - degree + j;
            let denom = knots[i + degree + 1 - r] - knots[i];
            let alpha = if denom.abs() < 1e-300 {
                0.0
            } else {
                (u - knots[i]) / denom
            };
            let prev = d[j - 1];
            for (cur, prev) in d[j].iter_mut().zip(prev) {
                *cur = (1.0 - alpha) * prev + alpha * *cur;
            }
        }
    }
    let p = d[degree];
    if p[2].abs() < 1e-300 {
        vec2(p[0], p[1])
    } else {
        vec2(p[0] / p[2], p[1] / p[2])
    }
}

/// Catmull-Rom interpolation through fit points. This approximates the curve
/// a CAD system would compute from the same fit points.
fn flatten_fit_points(fit: &[Vec2], closed: bool, samples: usize) -> Vec<Vec2> {
    let n = fit.len();
    let segs = if closed { n } else { n - 1 };
    let samples = samples.min((MAX_POINTS / segs.max(1)).max(1));
    let at = |i: isize| -> Vec2 {
        if closed {
            fit[i.rem_euclid(n as isize) as usize]
        } else {
            fit[i.clamp(0, n as isize - 1) as usize]
        }
    };
    let mut out = Vec::with_capacity(segs * samples + 1);
    for i in 0..segs as isize {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        for j in 0..samples {
            let t = j as f64 / samples as f64;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f64, b: f64, c: f64, d: f64| {
                0.5 * (2.0 * b
                    + (c - a) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (3.0 * b - a - 3.0 * c + d) * t3)
            };
            out.push(vec2(f(p0.x, p1.x, p2.x, p3.x), f(p0.y, p1.y, p2.y, p3.y)));
        }
    }
    out.push(at(segs as isize));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spline(degree: usize, ctrl: &[(f64, f64)], knots: &[f64], weights: &[f64]) -> Spline {
        Spline {
            degree,
            control_points: ctrl.iter().map(|&(x, y)| vec2(x, y)).collect(),
            weights: weights.to_vec(),
            knots: knots.to_vec(),
            fit_points: Vec::new(),
            closed: false,
        }
    }

    #[test]
    fn cubic_bezier_matches_closed_form() {
        let s = spline(
            3,
            &[(0.0, 0.0), (1.0, 2.0), (3.0, 2.0), (4.0, 0.0)],
            &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
            &[],
        );
        let pts = flatten_spline(&s, 8);
        assert_eq!(pts.len(), 9);
        assert_eq!(pts[0], vec2(0.0, 0.0));
        assert!(pts[8].distance(vec2(4.0, 0.0)) < 1e-12);
        // Bezier midpoint: (P0 + 3 P1 + 3 P2 + P3) / 8.
        assert!(pts[4].distance(vec2(2.0, 1.5)) < 1e-12);
    }

    #[test]
    fn rational_quadratic_is_a_circle_arc() {
        // Quarter circle of radius 1 as a rational quadratic Bezier.
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let s = spline(
            2,
            &[(1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            &[1.0, w, 1.0],
        );
        for p in flatten_spline(&s, 16) {
            assert!((p.length() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn bad_knots_fall_back_without_panicking() {
        let s = spline(
            3,
            &[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)],
            &[1.0, 0.0],
            &[5.0],
        );
        let pts = flatten_spline(&s, 8);
        assert!(pts.len() >= 2);
        assert_eq!(pts[0], vec2(0.0, 0.0));
        assert!(pts.last().unwrap().distance(vec2(2.0, 0.0)) < 1e-12);

        let nan = spline(
            2,
            &[(0.0, 0.0), (f64::NAN, 1.0), (2.0, 0.0)],
            &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            &[],
        );
        assert!(flatten_spline(&nan, 4).iter().all(|p| p.is_finite()));

        let empty = spline(3, &[], &[], &[]);
        assert!(flatten_spline(&empty, 8).is_empty());
    }

    #[test]
    fn fit_points_are_interpolated() {
        let s = Spline {
            degree: 3,
            control_points: Vec::new(),
            weights: Vec::new(),
            knots: Vec::new(),
            fit_points: vec![vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(2.0, 0.0)],
            closed: false,
        };
        let pts = flatten_spline(&s, 4);
        assert_eq!(pts.len(), 9);
        assert!(pts[4].distance(vec2(1.0, 1.0)) < 1e-12);
        assert_eq!(*pts.last().unwrap(), vec2(2.0, 0.0));
    }
}
