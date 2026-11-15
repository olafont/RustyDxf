// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Supplementary DXF group-code scanner.
//!
//! The `dxf` crate is the main parser, but version 0.6.2 silently drops a few
//! things RustyDxf needs. This module re-reads the raw group codes (ASCII or
//! binary) and recovers only those:
//!
//! * HATCH entities (boundary loops and the solid-fill flag),
//! * DIMENSION entities (the crate drops R12-style and 2-line angular ones),
//! * the frozen flag and true color of layers,
//! * the type names of every entity, so unsupported ones can be counted.
//!
//! The scanner is tolerant: on malformed input it stops and returns what it
//! has. It never panics.

use crate::aci::ColorSpec;
use crate::geom::{Vec2, ccw_sweep, ocs_to_wcs, vec2};
use crate::model::{PolyVertex, Spline, flatten_polyline};
use crate::spline::flatten_spline;
use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;

const BINARY_SENTINEL: &[u8] = b"AutoCAD Binary DXF\r\n\x1a\0";

/// Angular step used when flattening hatch boundary arcs.
const HATCH_ARC_STEP: f64 = TAU / 96.0;
/// Upper bounds that keep hostile files from allocating without limit.
const MAX_HATCH_LOOPS: usize = 100_000;
const MAX_LOOP_POINTS: usize = 2_000_000;

#[derive(Debug, Default)]
pub(crate) struct RawScan {
    /// Keyed by upper-cased layer name.
    pub layers: HashMap<String, RawLayer>,
    pub model: RawContainer,
    /// Keyed by upper-cased block name.
    pub blocks: HashMap<String, RawContainer>,
    /// True when at least one `SECTION` was seen.
    pub saw_section: bool,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct RawLayer {
    pub flags: i64,
    pub true_color: Option<i32>,
}

impl RawLayer {
    pub fn frozen(&self) -> bool {
        self.flags & 1 != 0
    }
}

#[derive(Debug, Default)]
pub(crate) struct RawContainer {
    pub hatches: Vec<RawHatch>,
    pub dimensions: Vec<RawDimension>,
    /// Model-space entities in this container by DXF type name.
    pub type_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawCommon {
    pub layer: String,
    pub color: ColorSpec,
    pub visible: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct RawHatch {
    pub common: RawCommon,
    pub solid: bool,
    /// Boundary loops in world XY (OCS already applied).
    pub loops: Vec<Vec<Vec2>>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawDimension {
    pub common: RawCommon,
    pub block_name: String,
}

// ---------------------------------------------------------------------------
// Group-code pairs

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Val {
    Str(String),
    Float(f64),
    Int(i64),
}

impl Val {
    fn as_f64(&self) -> f64 {
        match self {
            Val::Str(s) => s.trim().parse().unwrap_or(0.0),
            Val::Float(f) => *f,
            Val::Int(i) => *i as f64,
        }
    }
    fn as_i64(&self) -> i64 {
        match self {
            Val::Str(s) => {
                let s = s.trim();
                s.parse::<i64>()
                    .ok()
                    .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
                    .unwrap_or(0)
            }
            Val::Float(f) => *f as i64,
            Val::Int(i) => *i,
        }
    }
    fn as_str(&self) -> String {
        match self {
            Val::Str(s) => s.trim().to_string(),
            Val::Float(f) => f.to_string(),
            Val::Int(i) => i.to_string(),
        }
    }
}

type Pair = (i32, Val);

fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        // Close enough to the Windows-1252 default for names and labels.
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

struct AsciiPairs<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> AsciiPairs<'a> {
    fn line(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let rest = &self.data[self.pos..];
        let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        self.pos += (end + 1).min(rest.len().max(1));
        let mut line = &rest[..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        Some(line)
    }
}

impl Iterator for AsciiPairs<'_> {
    type Item = Pair;
    fn next(&mut self) -> Option<Pair> {
        let code_line = self.line()?;
        let code: i32 = std::str::from_utf8(code_line).ok()?.trim().parse().ok()?;
        let value = self.line()?;
        Some((code, Val::Str(decode(value))))
    }
}

struct BinaryPairs<'a> {
    data: &'a [u8],
    pos: usize,
    two_byte_codes: bool,
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    Double,
    Short,
    Int,
    Long,
    Bool,
    Binary,
}

/// Value type for a group code; mirrors `dxf::ExpectedType`.
fn kind_of(code: i32) -> Option<Kind> {
    Some(match code {
        0..=9 => Kind::Str,
        10..=59 => Kind::Double,
        60..=79 => Kind::Short,
        90..=99 => Kind::Int,
        100..=102 | 105 => Kind::Str,
        110..=149 => Kind::Double,
        160..=169 => Kind::Long,
        170..=179 => Kind::Short,
        210..=239 => Kind::Double,
        270..=289 => Kind::Short,
        290..=299 => Kind::Bool,
        300..=309 => Kind::Str,
        310..=319 => Kind::Binary,
        320..=369 => Kind::Str,
        370..=389 => Kind::Short,
        390..=399 => Kind::Str,
        400..=409 => Kind::Short,
        410..=419 => Kind::Str,
        420..=429 => Kind::Int,
        430..=439 => Kind::Str,
        440..=459 => Kind::Int,
        460..=469 => Kind::Double,
        470..=481 => Kind::Str,
        999 => Kind::Str,
        1000..=1009 => Kind::Str,
        1010..=1059 => Kind::Double,
        1060..=1070 => Kind::Short,
        1071 => Kind::Int,
        _ => return None,
    })
}

impl BinaryPairs<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
}

impl Iterator for BinaryPairs<'_> {
    type Item = Pair;
    fn next(&mut self) -> Option<Pair> {
        let code = if self.two_byte_codes {
            let b = self.take(2)?;
            i16::from_le_bytes([b[0], b[1]]) as i32
        } else {
            let c = self.take(1)?[0];
            if c == 255 {
                let b = self.take(2)?;
                i16::from_le_bytes([b[0], b[1]]) as i32
            } else {
                c as i32
            }
        };
        let val = match kind_of(code)? {
            Kind::Str => {
                let rest = self.data.get(self.pos..)?;
                let end = rest.iter().position(|&b| b == 0)?;
                let s = decode(&rest[..end]);
                self.pos += end + 1;
                Val::Str(s)
            }
            Kind::Double => {
                let b = self.take(8)?;
                Val::Float(f64::from_le_bytes(b.try_into().ok()?))
            }
            Kind::Short => {
                let b = self.take(2)?;
                Val::Int(i16::from_le_bytes([b[0], b[1]]) as i64)
            }
            Kind::Int => {
                let b = self.take(4)?;
                Val::Int(i32::from_le_bytes(b.try_into().ok()?) as i64)
            }
            Kind::Long => {
                let b = self.take(8)?;
                Val::Int(i64::from_le_bytes(b.try_into().ok()?))
            }
            Kind::Bool => {
                if self.two_byte_codes {
                    Val::Int(self.take(1)?[0] as i64)
                } else {
                    let b = self.take(2)?;
                    Val::Int(i16::from_le_bytes([b[0], b[1]]) as i64)
                }
            }
            Kind::Binary => {
                let len = self.take(1)?[0] as usize;
                self.take(len)?;
                Val::Str(String::new())
            }
        };
        Some((code, val))
    }
}

fn pairs(data: &[u8]) -> Box<dyn Iterator<Item = Pair> + '_> {
    if data.starts_with(BINARY_SENTINEL) {
        let body = &data[BINARY_SENTINEL.len()..];
        // R13+ files use two-byte codes, so the first pair (0, "SECTION")
        // starts with two zero bytes.
        let two_byte_codes = body.len() >= 2 && body[0] == 0 && body[1] == 0;
        Box::new(BinaryPairs {
            data: body,
            pos: 0,
            two_byte_codes,
        })
    } else {
        // Skip a UTF-8 byte order mark if present.
        let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
        Box::new(AsciiPairs { data, pos: 0 })
    }
}

// ---------------------------------------------------------------------------
// Scanning

#[derive(PartialEq)]
enum Section {
    None,
    Tables,
    Blocks,
    Entities,
    Other,
}

pub(crate) fn scan(data: &[u8]) -> RawScan {
    let mut out = RawScan::default();
    let mut section = Section::None;
    let mut table = String::new();
    let mut block: Option<String> = None;

    let mut kind = String::new();
    let mut record: Vec<Pair> = Vec::new();
    let mut iter = pairs(data);

    loop {
        let next = iter.next();
        let boundary = match &next {
            Some((0, _)) | None => true,
            Some(_) => false,
        };
        if boundary && !kind.is_empty() {
            // Finish the record collected so far.
            match kind.as_str() {
                "SECTION" => {
                    out.saw_section = true;
                    section = match first_str(&record, 2).as_deref() {
                        Some("TABLES") => Section::Tables,
                        Some("BLOCKS") => Section::Blocks,
                        Some("ENTITIES") => Section::Entities,
                        _ => Section::Other,
                    };
                }
                "ENDSEC" => section = Section::None,
                "TABLE" if section == Section::Tables => {
                    table = first_str(&record, 2).unwrap_or_default().to_uppercase();
                }
                "ENDTAB" => table.clear(),
                "LAYER" if section == Section::Tables && table == "LAYER" => {
                    if let Some(name) = first_str(&record, 2) {
                        out.layers.insert(
                            name.to_uppercase(),
                            RawLayer {
                                flags: first(&record, 70).map(|v| v.as_i64()).unwrap_or(0),
                                true_color: first(&record, 420).map(|v| v.as_i64() as i32),
                            },
                        );
                    }
                }
                "BLOCK" if section == Section::Blocks => {
                    let name = first_str(&record, 2).unwrap_or_default().to_uppercase();
                    out.blocks.entry(name.clone()).or_default();
                    block = Some(name);
                }
                "ENDBLK" if section == Section::Blocks => block = None,
                _ if section == Section::Entities => {
                    record_entity(&mut out.model, &kind, &record);
                }
                _ if section == Section::Blocks => {
                    if let Some(name) = &block
                        && let Some(c) = out.blocks.get_mut(name)
                    {
                        record_entity(c, &kind, &record);
                    }
                }
                _ => {}
            }
        }
        match next {
            Some((0, v)) => {
                kind = v.as_str().to_uppercase();
                record.clear();
                if kind == "EOF" {
                    break;
                }
            }
            Some(p) => {
                // Bound memory for absurdly long records we do not parse.
                if record.len() < 4_000_000 {
                    record.push(p);
                }
            }
            None => break,
        }
    }
    out
}

fn first(record: &[Pair], code: i32) -> Option<&Val> {
    record.iter().find(|(c, _)| *c == code).map(|(_, v)| v)
}

fn first_str(record: &[Pair], code: i32) -> Option<String> {
    first(record, code).map(|v| v.as_str())
}

fn common(record: &[Pair]) -> (RawCommon, bool) {
    let layer = first_str(record, 8).unwrap_or_else(|| "0".to_string());
    let mut color = match first(record, 62).map(|v| v.as_i64()) {
        Some(0) => ColorSpec::ByBlock,
        Some(i @ 1..=255) => ColorSpec::Aci(i as u8),
        _ => ColorSpec::ByLayer,
    };
    if let Some(v) = first(record, 420) {
        color = ColorSpec::True(v.as_i64() as i32);
    }
    // Gradient hatches keep their real colors in the gradient data (421);
    // fill with the first one rather than the placeholder entity color.
    if first(record, 450).map(|v| v.as_i64()) == Some(1)
        && let Some(v) = first(record, 421)
    {
        color = ColorSpec::True(v.as_i64() as i32);
    }
    let paper_space = first(record, 67).map(|v| v.as_i64()) == Some(1);
    let visible = first(record, 60).map(|v| v.as_i64()) != Some(1);
    (
        RawCommon {
            layer,
            color,
            visible,
        },
        paper_space,
    )
}

fn record_entity(container: &mut RawContainer, kind: &str, record: &[Pair]) {
    let (common, paper_space) = common(record);
    if paper_space {
        return;
    }
    *container.type_counts.entry(kind.to_string()).or_insert(0) += 1;
    match kind {
        "HATCH" => {
            let (solid, loops) = parse_hatch(record);
            container.hatches.push(RawHatch {
                common,
                solid,
                loops,
            });
        }
        "DIMENSION" => {
            if let Some(block_name) = first_str(record, 2) {
                container
                    .dimensions
                    .push(RawDimension { common, block_name });
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// HATCH

struct Cursor<'a> {
    rec: &'a [Pair],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn peek_code(&self) -> Option<i32> {
        self.rec.get(self.i).map(|p| p.0)
    }
    fn peek_code_at(&self, offset: usize) -> Option<i32> {
        self.rec.get(self.i + offset).map(|p| p.0)
    }
    fn next(&mut self) -> Option<&'a Pair> {
        let p = self.rec.get(self.i)?;
        self.i += 1;
        Some(p)
    }
    /// Consume the next pair if it has the given code.
    fn take(&mut self, code: i32) -> Option<&'a Val> {
        if self.peek_code() == Some(code) {
            self.next().map(|p| &p.1)
        } else {
            None
        }
    }
    /// Skip forward to the next pair with the given code and consume it.
    fn seek(&mut self, code: i32) -> Option<&'a Val> {
        while let Some(p) = self.next() {
            if p.0 == code {
                return Some(&p.1);
            }
        }
        None
    }
    fn take_f64(&mut self, code: i32) -> Option<f64> {
        self.take(code).map(|v| v.as_f64())
    }
    fn take_point(&mut self, x_code: i32) -> Option<Vec2> {
        let x = self.take_f64(x_code)?;
        let y = self.take_f64(x_code + 10).unwrap_or(0.0);
        Some(vec2(x, y))
    }
}

fn parse_hatch(record: &[Pair]) -> (bool, Vec<Vec<Vec2>>) {
    let solid = first(record, 70).map(|v| v.as_i64()) == Some(1);
    let ext = |code: i32, default: f64| first(record, code).map(|v| v.as_f64()).unwrap_or(default);
    let ocs = ocs_to_wcs(ext(210, 0.0), ext(220, 0.0), ext(230, 1.0));

    let mut cur = Cursor { rec: record, i: 0 };
    let Some(count) = cur.seek(91).map(|v| v.as_i64()) else {
        return (solid, Vec::new());
    };
    let count = (count.max(0) as usize).min(MAX_HATCH_LOOPS);
    let mut loops = Vec::new();
    let mut budget = MAX_LOOP_POINTS;
    for _ in 0..count {
        let Some(flags) = cur.seek(92).map(|v| v.as_i64()) else {
            break;
        };
        let pts = if flags & 2 != 0 {
            polyline_path(&mut cur)
        } else {
            edge_path(&mut cur)
        };
        let mut pts: Vec<Vec2> = pts
            .into_iter()
            .filter(|p| p.is_finite())
            .map(|p| ocs.point(p))
            .collect();
        pts.dedup();
        if pts.len() > budget {
            break;
        }
        budget -= pts.len();
        if pts.len() >= 3 {
            loops.push(pts);
        }
    }
    (solid, loops)
}

fn polyline_path(cur: &mut Cursor) -> Vec<Vec2> {
    let mut has_bulge = false;
    let mut closed = true;
    let mut count = 0usize;
    // 72 (has bulge), 73 (closed), 93 (vertex count): tolerate any order.
    for _ in 0..3 {
        match cur.peek_code() {
            Some(72) => has_bulge = cur.next().map(|p| p.1.as_i64()) == Some(1),
            Some(73) => closed = cur.next().map(|p| p.1.as_i64()) != Some(0),
            Some(93) => {
                count = cur
                    .next()
                    .map(|p| p.1.as_i64().max(0) as usize)
                    .unwrap_or(0)
            }
            _ => break,
        }
    }
    let mut verts = Vec::new();
    for _ in 0..count.min(MAX_LOOP_POINTS) {
        let Some(pos) = cur.take_point(10) else { break };
        let bulge = if has_bulge {
            cur.take_f64(42).unwrap_or(0.0)
        } else {
            0.0
        };
        verts.push(PolyVertex { pos, bulge });
    }
    flatten_polyline(&verts, closed, 0.0)
}

fn edge_path(cur: &mut Cursor) -> Vec<Vec2> {
    let count = cur
        .take(93)
        .map(|v| v.as_i64().max(0) as usize)
        .unwrap_or(0);
    let mut pts: Vec<Vec2> = Vec::new();
    let count = count.min(MAX_LOOP_POINTS);
    for edge in 0..count {
        let Some(edge_type) = cur.take(72).map(|v| v.as_i64()) else {
            break;
        };
        match edge_type {
            1 => {
                let a = cur.take_point(10);
                let b = cur.take_point(11);
                pts.extend(a);
                pts.extend(b);
            }
            2 => {
                let c = cur.take_point(10).unwrap_or(Vec2::ZERO);
                let r = cur.take_f64(40).unwrap_or(0.0);
                let (s, e, ccw) = arc_params(cur);
                arc_points(&mut pts, c, vec2(r, 0.0), vec2(0.0, r), s, e, ccw);
            }
            3 => {
                let c = cur.take_point(10).unwrap_or(Vec2::ZERO);
                let major = cur.take_point(11).unwrap_or(Vec2::ZERO);
                let ratio = cur.take_f64(40).unwrap_or(1.0);
                let minor = vec2(-major.y, major.x) * ratio;
                let (s, e, ccw) = arc_params(cur);
                arc_points(&mut pts, c, major, minor, s, e, ccw);
            }
            4 => pts.extend(spline_edge(cur, edge + 1 == count)),
            _ => break,
        }
    }
    pts
}

fn arc_params(cur: &mut Cursor) -> (f64, f64, bool) {
    let s = cur.take_f64(50).unwrap_or(0.0).to_radians();
    let e = cur.take_f64(51).unwrap_or(360.0).to_radians();
    let ccw = cur.take(73).map(|v| v.as_i64()) != Some(0);
    (s, e, ccw)
}

/// Append points along an elliptical arc in the direction the boundary is
/// traversed. Clockwise edges store their angles mirrored about the X axis.
fn arc_points(
    out: &mut Vec<Vec2>,
    center: Vec2,
    major: Vec2,
    minor: Vec2,
    start: f64,
    end: f64,
    ccw: bool,
) {
    if !(start.is_finite() && end.is_finite()) {
        return;
    }
    let sweep = ccw_sweep(start, end);
    let sign = if ccw { 1.0 } else { -1.0 };
    let n = ((sweep / HATCH_ARC_STEP).ceil() as usize).clamp(2, 720);
    for i in 0..=n {
        let t = sign * (start + sweep * i as f64 / n as f64);
        let (s, c) = t.sin_cos();
        out.push(center + major * c + minor * s);
    }
}

fn spline_edge(cur: &mut Cursor, last_edge: bool) -> Vec<Vec2> {
    let degree = cur.take(94).map(|v| v.as_i64()).unwrap_or(3).clamp(1, 16) as usize;
    cur.take(73);
    cur.take(74);
    let n_knots = cur
        .take(95)
        .map(|v| v.as_i64().max(0) as usize)
        .unwrap_or(0);
    let n_ctrl = cur
        .take(96)
        .map(|v| v.as_i64().max(0) as usize)
        .unwrap_or(0);
    let mut knots = Vec::new();
    for _ in 0..n_knots.min(MAX_LOOP_POINTS) {
        match cur.take_f64(40) {
            Some(k) => knots.push(k),
            None => break,
        }
    }
    let mut control_points = Vec::new();
    let mut weights = Vec::new();
    for _ in 0..n_ctrl.min(MAX_LOOP_POINTS) {
        let Some(p) = cur.take_point(10) else { break };
        control_points.push(p);
        if let Some(w) = cur.take_f64(42) {
            weights.push(w);
        }
    }
    // Optional fit data (R2010+). The path's own trailing 97 (source object
    // count) uses the same code, so after the last edge only treat this as
    // fit data when fit points or another 97 follow. Between edges a 97 can
    // only be fit data.
    let mut fit_points = Vec::new();
    if cur.peek_code() == Some(97)
        && (!last_edge || matches!(cur.peek_code_at(1), Some(11) | Some(12) | Some(97)))
    {
        let n_fit = cur
            .take(97)
            .map(|v| v.as_i64().max(0) as usize)
            .unwrap_or(0);
        for _ in 0..n_fit.min(MAX_LOOP_POINTS) {
            match cur.take_point(11) {
                Some(p) => fit_points.push(p),
                None => break,
            }
        }
    }
    cur.take_point(12);
    cur.take_point(13);
    flatten_spline(
        &Spline {
            degree,
            control_points,
            weights,
            knots,
            fit_points,
            closed: false,
        },
        16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii(pairs: &[(i32, &str)]) -> Vec<u8> {
        let mut s = String::new();
        for (c, v) in pairs {
            s.push_str(&format!("{c:>3}\r\n{v}\r\n"));
        }
        s.into_bytes()
    }

    fn wrap_entities(mut body: Vec<(i32, &'static str)>) -> Vec<u8> {
        let mut all = vec![(0, "SECTION"), (2, "ENTITIES")];
        all.append(&mut body);
        all.extend([(0, "ENDSEC"), (0, "EOF")]);
        ascii(&all)
    }

    #[test]
    fn polyline_hatch_and_counts() {
        let data = wrap_entities(vec![
            (0, "LINE"),
            (8, "A"),
            (0, "HATCH"),
            (8, "FILL"),
            (62, "3"),
            (2, "SOLID"),
            (70, "1"),
            (91, "1"),
            (92, "2"),
            (72, "0"),
            (73, "1"),
            (93, "4"),
            (10, "0"),
            (20, "0"),
            (10, "10"),
            (20, "0"),
            (10, "10"),
            (20, "5"),
            (10, "0"),
            (20, "5"),
            (97, "0"),
            (75, "0"),
            (76, "1"),
            (98, "0"),
            (0, "WIPEOUT"),
            (0, "VIEWPORT"),
            (67, "1"),
        ]);
        let scan = scan(&data);
        assert!(scan.saw_section);
        assert_eq!(scan.model.type_counts["LINE"], 1);
        assert_eq!(scan.model.type_counts["HATCH"], 1);
        assert_eq!(scan.model.type_counts["WIPEOUT"], 1);
        // Paper-space entities are not counted.
        assert!(!scan.model.type_counts.contains_key("VIEWPORT"));
        let h = &scan.model.hatches[0];
        assert!(h.solid);
        assert_eq!(h.common.layer, "FILL");
        assert_eq!(h.common.color, ColorSpec::Aci(3));
        assert_eq!(
            h.loops,
            vec![vec![
                vec2(0.0, 0.0),
                vec2(10.0, 0.0),
                vec2(10.0, 5.0),
                vec2(0.0, 5.0)
            ]]
        );
    }

    #[test]
    fn edge_hatch_with_line_and_arc() {
        // A half disc: a line along the diameter, then a CCW semicircle back.
        let data = wrap_entities(vec![
            (0, "HATCH"),
            (2, "ANSI31"),
            (70, "0"),
            (91, "1"),
            (92, "1"),
            (93, "2"),
            (72, "1"),
            (10, "-1"),
            (20, "0"),
            (11, "1"),
            (21, "0"),
            (72, "2"),
            (10, "0"),
            (20, "0"),
            (40, "1"),
            (50, "0"),
            (51, "180"),
            (73, "1"),
            (97, "0"),
        ]);
        let scan = scan(&data);
        let h = &scan.model.hatches[0];
        assert!(!h.solid);
        let l = &h.loops[0];
        assert_eq!(l[0], vec2(-1.0, 0.0));
        assert!(l.iter().all(|p| p.y >= -1e-12 && p.length() <= 1.0 + 1e-9));
        assert!(l.iter().any(|p| (p.y - 1.0).abs() < 1e-9));
        assert!(l.last().unwrap().distance(vec2(-1.0, 0.0)) < 1e-9);
    }

    #[test]
    fn clockwise_arc_edge_goes_below_the_axis() {
        let data = wrap_entities(vec![
            (0, "HATCH"),
            (70, "1"),
            (91, "1"),
            (92, "1"),
            (93, "1"),
            (72, "2"),
            (10, "0"),
            (20, "0"),
            (40, "1"),
            (50, "0"),
            (51, "180"),
            (73, "0"),
            (97, "0"),
        ]);
        let scan = scan(&data);
        let l = &scan.model.hatches[0].loops[0];
        assert!(l.iter().all(|p| p.y <= 1e-12));
    }

    #[test]
    fn layers_blocks_and_dimensions() {
        let data = ascii(&[
            (0, "SECTION"),
            (2, "TABLES"),
            (0, "TABLE"),
            (2, "LAYER"),
            (0, "LAYER"),
            (2, "Walls"),
            (70, "1"),
            (62, "1"),
            (0, "LAYER"),
            (2, "Doors"),
            (70, "0"),
            (420, "1193046"),
            (0, "ENDTAB"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "BLOCKS"),
            (0, "BLOCK"),
            (2, "*D1"),
            (0, "HATCH"),
            (70, "1"),
            (91, "0"),
            (0, "ENDBLK"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "DIMENSION"),
            (8, "DIMS"),
            (2, "*D1"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        let scan = scan(&data);
        assert!(scan.layers["WALLS"].frozen());
        assert!(!scan.layers["DOORS"].frozen());
        assert_eq!(scan.layers["DOORS"].true_color, Some(0x123456));
        assert_eq!(scan.blocks["*D1"].hatches.len(), 1);
        assert_eq!(scan.model.dimensions[0].block_name, "*D1");
        assert_eq!(scan.model.dimensions[0].common.layer, "DIMS");
    }

    #[test]
    fn binary_two_byte_codes() {
        fn s(code: i16, v: &str, out: &mut Vec<u8>) {
            out.extend(code.to_le_bytes());
            out.extend(v.as_bytes());
            out.push(0);
        }
        fn f(code: i16, v: f64, out: &mut Vec<u8>) {
            out.extend(code.to_le_bytes());
            out.extend(v.to_le_bytes());
        }
        fn i16v(code: i16, v: i16, out: &mut Vec<u8>) {
            out.extend(code.to_le_bytes());
            out.extend(v.to_le_bytes());
        }
        fn i32v(code: i16, v: i32, out: &mut Vec<u8>) {
            out.extend(code.to_le_bytes());
            out.extend(v.to_le_bytes());
        }
        let mut d = BINARY_SENTINEL.to_vec();
        s(0, "SECTION", &mut d);
        s(2, "ENTITIES", &mut d);
        s(0, "HATCH", &mut d);
        s(8, "B", &mut d);
        i16v(70, 1, &mut d);
        i32v(91, 1, &mut d);
        i32v(92, 2, &mut d);
        i16v(72, 0, &mut d);
        i16v(73, 1, &mut d);
        i32v(93, 3, &mut d);
        for (x, y) in [(0.0, 0.0), (4.0, 0.0), (0.0, 3.0)] {
            f(10, x, &mut d);
            f(20, y, &mut d);
        }
        s(0, "ENDSEC", &mut d);
        s(0, "EOF", &mut d);
        let scan = scan(&d);
        let h = &scan.model.hatches[0];
        assert!(h.solid);
        assert_eq!(h.common.layer, "B");
        assert_eq!(
            h.loops,
            vec![vec![vec2(0.0, 0.0), vec2(4.0, 0.0), vec2(0.0, 3.0)]]
        );
    }

    #[test]
    fn garbage_does_not_panic() {
        for data in [
            &b""[..],
            b"\n\n\n",
            b"not a dxf at all",
            b"0\nSECTION\n2\nENTITIES\n0\nHATCH\n91\n999999999\n92\n2\n93\n-5\n",
            b"0\nSECTION\n2\nENTITIES\n0\nHATCH\n91\n2\n92\n1\n93\n3\n72\n4\n94\n99\n95\n-1\n96\n7\n",
            b"AutoCAD Binary DXF\r\n\x1a\0\0\0SECTION",
            b"AutoCAD Binary DXF\r\n\x1a\0\xff\xff\xff",
        ] {
            let _ = scan(data);
        }
    }
}
