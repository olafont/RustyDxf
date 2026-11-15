# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 art-tech

"""Generate a deliberately awkward DXF with ezdxf (an independent writer) to
exercise RustyDxf. Usage: python make_complex.py OUT_DIR

Writes complex.dxf (R2010 ASCII), complex_bin.dxf (binary) and
complex_r2000.dxf, and prints ezdxf's own view of the file as JSON
(entity counts, extents) for comparison.
"""
import json
import math
import sys
from collections import Counter
from pathlib import Path

import ezdxf
from ezdxf import bbox, units
from ezdxf.enums import TextEntityAlignment
from ezdxf.math import Vec2

# Survey-style coordinates: far from the origin, to stress f32 precision.
OX, OY = 512_000.0, 4_180_000.0


def P(x, y):
    return (OX + x, OY + y)


def build(version="R2010"):
    doc = ezdxf.new(version, setup=True)
    doc.units = units.MM
    doc.header["$LTSCALE"] = 1.0
    msp = doc.modelspace()

    L = doc.layers
    L.add("OUTLINE", color=7, lineweight=50)
    L.add("HOLES", color=1)
    L.add("CENTER", color=2, linetype="CENTER")
    L.add("HIDDEN", color=8, linetype="DASHED")
    L.add("DIMS", color=3)
    L.add("TEXT", color=4)
    L.add("HATCH", color=5)
    L.add("BLOCKS", color=6)
    L.add("SPLINES", color=30)
    L.add("GRID", color=251)
    tc = L.add("TRUECOLOR", color=7)
    tc.rgb = (255, 128, 0)
    L.add("Ünïcode Läyer 図面", color=140)
    off = L.add("LAYER_OFF", color=1)
    off.off()
    frozen = L.add("LAYER_FROZEN", color=1)
    frozen.freeze()
    locked = L.add("LAYER_LOCKED", color=9)
    locked.lock()

    # ---- 1. Flange plate (0..400 x 0..300) --------------------------------
    # Rounded rectangle as one LWPOLYLINE with bulges.
    r, w, h = 30.0, 400.0, 300.0
    b = math.tan(math.pi / 8)  # quarter-circle bulge
    pts = [
        (r, 0, b * 0), (w - r, 0, b), (w, r, 0), (w, h - r, b),
        (w - r, h, 0), (r, h, b), (0, h - r, 0), (0, r, b),
    ]
    msp.add_lwpolyline(
        [(OX + x, OY + y, 0, 0, bulge) for x, y, bulge in pts],
        format="xyseb", close=True, dxfattribs={"layer": "OUTLINE"},
    )
    # Bolt circle: 12 holes + centre marks.
    cx, cy, bcr = 200.0, 150.0, 100.0
    msp.add_circle(P(cx, cy), 60.0, dxfattribs={"layer": "HOLES"})
    msp.add_circle(P(cx, cy), bcr, dxfattribs={"layer": "CENTER"})
    for i in range(12):
        a = i * math.tau / 12
        hx, hy = cx + bcr * math.cos(a), cy + bcr * math.sin(a)
        msp.add_circle(P(hx, hy), 8.0, dxfattribs={"layer": "HOLES"})
        msp.add_line(P(hx - 12, hy), P(hx + 12, hy), dxfattribs={"layer": "CENTER"})
        msp.add_line(P(hx, hy - 12), P(hx, hy + 12), dxfattribs={"layer": "CENTER"})
    # Slots: closed polylines with two semicircle bulges (CW and CCW).
    for sx, sy, cw in ((40, 40, False), (360, 40, True), (40, 260, True), (360, 260, False)):
        sl = [(sx - 15, sy - 6, 0), (sx + 15, sy - 6, 1), (sx + 15, sy + 6, 0), (sx - 15, sy + 6, 1)]
        if cw:
            sl = [(x, y, -bg) for x, y, bg in reversed(sl)]
            sl = sl[-1:] + sl[:-1]
        msp.add_lwpolyline([(OX + x, OY + y, bg) for x, y, bg in sl], format="xyb",
                           close=True, dxfattribs={"layer": "HOLES"})
    # Arcs incl. one crossing 0 degrees and one with negative extrusion.
    msp.add_arc(P(200, 150), 75, 300, 60, dxfattribs={"layer": "HIDDEN"})
    msp.add_arc(P(200, 150), 70, 120, 240, dxfattribs={"layer": "HIDDEN"})
    # OCS with extrusion (0,0,-1): x is mirrored. Centre given in OCS.
    msp.add_arc((-(OX + 200), OY + 150), 45, 10, 80,
                dxfattribs={"layer": "TRUECOLOR", "extrusion": (0, 0, -1)})
    msp.add_circle((-(OX + 330), OY + 150), 14,
                   dxfattribs={"layer": "TRUECOLOR", "extrusion": (0, 0, -1)})
    # Ellipses: full, partial, rotated, ratio 1.
    msp.add_ellipse(P(80, 150), major_axis=(25, 0), ratio=0.5, dxfattribs={"layer": "HOLES"})
    msp.add_ellipse(P(80, 150), major_axis=(20, 20), ratio=0.3,
                    start_param=0.5, end_param=4.5, dxfattribs={"layer": "HIDDEN"})
    msp.add_ellipse(P(330, 150), major_axis=(0, 30), ratio=1.0, dxfattribs={"layer": "CENTER"})

    # ---- 2. Splines (right of the plate) ---------------------------------
    ctrl = [P(450, 0), P(500, 120), P(560, -60), P(620, 150), P(700, 40), P(760, 90)]
    msp.add_open_spline([(x, y, 0) for x, y in ctrl], degree=3, dxfattribs={"layer": "SPLINES"})
    msp.add_rational_spline([(x, y + 80, 0) for x, y in ctrl], weights=[1, 4, 1, 0.3, 2, 1],
                            degree=3, dxfattribs={"layer": "SPLINES", "color": 1})
    ring = [(OX + 600 + 60 * math.cos(t) * (1 + 0.3 * math.sin(5 * t)),
             OY + 240 + 40 * math.sin(t) * (1 + 0.3 * math.sin(5 * t)), 0)
            for t in [i * math.tau / 15 for i in range(15)]]
    msp.add_open_spline(ring + ring[:1], degree=3, dxfattribs={"layer": "SPLINES", "color": 4})
    fit = [P(450, 200), P(480, 280), P(520, 210), P(540, 290)]
    s = msp.add_spline(dxfattribs={"layer": "SPLINES", "color": 6})  # fit points only
    s.fit_points = [(x, y, 0) for x, y in fit]
    for x, y in ctrl:
        msp.add_point((x, y), dxfattribs={"layer": "SPLINES"})

    # ---- 3. Old-style POLYLINE entities ----------------------------------
    msp.add_polyline2d([P(450, -150), P(520, -100), P(590, -150), P(660, -100)],
                       format="xy", dxfattribs={"layer": "OUTLINE", "color": 3})
    pl = msp.add_polyline2d([(OX + 450, OY - 60, 0.5), (OX + 520, OY - 60, -0.5),
                             (OX + 590, OY - 60, 1.0), (OX + 660, OY - 60, 0)],
                            format="xyb", dxfattribs={"layer": "OUTLINE", "color": 5})
    pl.close(True)
    msp.add_polyline3d([(OX + 700, OY - 150, 0), (OX + 740, OY - 100, 50), (OX + 780, OY - 150, 100)],
                       dxfattribs={"layer": "OUTLINE", "color": 2})
    # Wide polyline (constant width) and a zero-length line / zero-radius circle.
    msp.add_lwpolyline([P(700, -60), P(780, -60), P(780, -20)],
                       dxfattribs={"layer": "OUTLINE", "const_width": 4})
    msp.add_line(P(10, 10), P(10, 10), dxfattribs={"layer": "OUTLINE"})
    msp.add_circle(P(20, 20), 0.0, dxfattribs={"layer": "OUTLINE"})

    # ---- 4. Blocks: nested, scaled, mirrored, rotated, with attributes ----
    bolt = doc.blocks.new("BOLT")
    bolt.add_circle((0, 0), 5, dxfattribs={"layer": "0", "color": 0})       # BYBLOCK
    bolt.add_lwpolyline([(5 * math.cos(i * math.tau / 6), 5 * math.sin(i * math.tau / 6))
                         for i in range(6)], close=True, dxfattribs={"layer": "0"})  # BYLAYER on 0
    bolt.add_line((-7, 0), (7, 0), dxfattribs={"layer": "CENTER"})
    bolt.add_arc((0, 0), 3.5, 0, 270, dxfattribs={"layer": "0", "color": 1})

    bracket = doc.blocks.new("BRACKET", base_point=(10, 10))
    bracket.add_lwpolyline([(10, 10), (70, 10), (70, 20), (20, 20), (20, 60), (10, 60)],
                           close=True, dxfattribs={"layer": "0"})
    bracket.add_blockref("BOLT", (45, 15), dxfattribs={"layer": "0", "color": 0})
    bracket.add_blockref("BOLT", (15, 40), dxfattribs={"xscale": 0.6, "yscale": 0.6, "rotation": 30})
    bracket.add_text("BRK", height=4, dxfattribs={"layer": "TEXT"}).set_placement((25, 25))
    bracket.add_attdef("TAG", (25, 32), dxfattribs={"height": 3, "layer": "TEXT"})

    assembly = doc.blocks.new("ASSEMBLY")
    assembly.add_blockref("BRACKET", (0, 0))
    assembly.add_blockref("BRACKET", (0, 0), dxfattribs={"xscale": -1})          # mirrored
    assembly.add_blockref("BRACKET", (0, 0), dxfattribs={"yscale": -1, "rotation": 15})
    assembly.add_ellipse((0, 0), major_axis=(8, 0), ratio=0.5)

    y0 = -320
    for i, (sx, sy, rot, color) in enumerate([
        (1, 1, 0, 256), (2, 2, 45, 1), (-1.5, 1.5, 0, 3), (1, 0.5, 20, 256), (0.5, 2, -30, 5),
    ]):
        ref = msp.add_blockref("ASSEMBLY", P(80 + i * 160, y0), dxfattribs={
            "layer": "BLOCKS", "xscale": sx, "yscale": sy, "rotation": rot, "color": color})
    # Insert with attributes.
    ref = msp.add_blockref("BRACKET", P(0, -520), dxfattribs={"layer": "BLOCKS"})
    ref.add_auto_attribs({"TAG": "ATTR-42"})
    # Rectangular array (MINSERT-style: row/column counts).
    msp.add_blockref("BOLT", P(200, -520), dxfattribs={
        "layer": "BLOCKS", "column_count": 8, "row_count": 3,
        "column_spacing": 25, "row_spacing": 20, "rotation": 10})
    # Self-referencing block and a reference to a missing block must not hang or crash.
    loop_a = doc.blocks.new("LOOP_A")
    loop_b = doc.blocks.new("LOOP_B")
    loop_a.add_line((0, 0), (10, 0))
    loop_a.add_blockref("LOOP_B", (0, 0))
    loop_b.add_line((0, 0), (0, 10))
    loop_b.add_blockref("LOOP_A", (5, 5))
    msp.add_blockref("LOOP_A", P(460, -520), dxfattribs={"layer": "BLOCKS"})

    # ---- 5. Hatches ------------------------------------------------------
    hx, hy = 850.0, 0.0
    h1 = msp.add_hatch(color=5, dxfattribs={"layer": "HATCH"})          # solid + island
    h1.paths.add_polyline_path([P(hx, hy), P(hx + 120, hy), P(hx + 120, hy + 80), P(hx, hy + 80)],
                               is_closed=True)
    h1.paths.add_polyline_path([(OX + hx + 40, OY + hy + 40, 1), (OX + hx + 80, OY + hy + 40, 1)],
                               is_closed=True)                          # circle from two bulges
    h2 = msp.add_hatch(dxfattribs={"layer": "HATCH"})                   # pattern
    h2.set_pattern_fill("ANSI31", scale=2.0, color=2)
    h2.paths.add_polyline_path([P(hx, hy + 100), P(hx + 120, hy + 100), P(hx + 60, hy + 180)],
                               is_closed=True)
    h3 = msp.add_hatch(dxfattribs={"layer": "HATCH"})                   # edge path, true color
    if version >= "R2004":
        h3.rgb = (40, 160, 90)
    ep = h3.paths.add_edge_path()
    ex, ey = OX + hx + 160, OY + hy
    ep.add_line((ex, ey), (ex + 100, ey))
    ep.add_arc((ex + 100, ey + 40), radius=40, start_angle=270, end_angle=90, ccw=True)
    ep.add_line((ex + 100, ey + 80), (ex, ey + 80))
    ep.add_ellipse((ex, ey + 40), major_axis=(0, 40), ratio=0.5,
                   start_angle=0, end_angle=180, ccw=True)
    h4 = msp.add_hatch(dxfattribs={"layer": "HATCH"})                   # gradient
    if version >= "R2004":
        h4.set_gradient(color1=(255, 0, 0), color2=(0, 0, 255), rotation=45)
    else:
        h4.set_solid_fill(color=1)
    h4.paths.add_polyline_path([P(hx + 160, hy + 100), P(hx + 260, hy + 100),
                                P(hx + 260, hy + 180), P(hx + 160, hy + 180)], is_closed=True)
    h5 = msp.add_hatch(color=1, dxfattribs={"layer": "HATCH"})          # spline edge
    ep = h5.paths.add_edge_path()
    sx_, sy_ = OX + hx, OY + hy - 120
    ep.add_spline(control_points=[(sx_, sy_), (sx_ + 40, sy_ + 90), (sx_ + 90, sy_ - 60),
                                  (sx_ + 130, sy_ + 40)], degree=3)
    ep.add_line((sx_ + 130, sy_ + 40), (sx_ + 130, sy_ - 40))
    ep.add_line((sx_ + 130, sy_ - 40), (sx_, sy_ - 40))
    ep.add_line((sx_, sy_ - 40), (sx_, sy_))
    msp.add_solid([P(hx + 160, -160), P(hx + 260, -160), P(hx + 160, -80), P(hx + 260, -80)],
                  dxfattribs={"layer": "HATCH", "color": 30})
    msp.add_3dface([(OX + hx + 280, OY - 160, 0), (OX + hx + 340, OY - 160, 10),
                    (OX + hx + 340, OY - 80, 20), (OX + hx + 280, OY - 80, 0)],
                   dxfattribs={"layer": "HATCH", "color": 6})

    # ---- 6. Text ---------------------------------------------------------
    tx, ty = 0.0, 340.0
    for i, (label, align) in enumerate([
        ("LEFT", TextEntityAlignment.LEFT), ("CENTER", TextEntityAlignment.CENTER),
        ("RIGHT", TextEntityAlignment.RIGHT), ("MIDDLE", TextEntityAlignment.MIDDLE_CENTER),
        ("TOP_LEFT", TextEntityAlignment.TOP_LEFT), ("BOTTOM_RIGHT", TextEntityAlignment.BOTTOM_RIGHT),
    ]):
        p = P(tx + 100, ty + i * 14)
        msp.add_text(f"{label} align", height=8, dxfattribs={"layer": "TEXT"}).set_placement(p, align=align)
        msp.add_point(p, dxfattribs={"layer": "CENTER"})
    msp.add_text("Rotated 30%%d  %%c50  %%p0.05  %%uunderlined%%u", height=7,
                 dxfattribs={"layer": "TEXT", "rotation": 30}).set_placement(P(220, 340))
    msp.add_text("Wide text", height=7, dxfattribs={"layer": "TEXT", "width": 1.8, "oblique": 15}
                 ).set_placement(P(220, 430))
    msp.add_text("Ünïcödé — 日本語 — Ω ± ∅", height=7,
                 dxfattribs={"layer": "Ünïcode Läyer 図面"}).set_placement(P(0, 440))
    msp.add_text("ALIGNED between two points", height=5, dxfattribs={"layer": "TEXT"}
                 ).set_placement(P(0, 460), P(180, 480), align=TextEntityAlignment.ALIGNED)
    m = msp.add_mtext(
        r"{\fArial|b1|i0;\H1.5x;TITLE BLOCK}\P"
        r"Material: {\C1;AISI 304}\PTolerance: \S+0.05^-0.02;\P"
        r"\LUnderlined\l and \Ooverlined\o\P"
        r"Diameter %%c25, angle 45%%d, \U+00B1 0.1\P"
        r"A long paragraph that has to wrap inside the reference rectangle width given by group code 41, "
        r"so the viewer needs word wrapping to keep it inside the frame.",
        dxfattribs={"layer": "TEXT", "char_height": 6, "width": 220, "attachment_point": 1})
    m.set_location(P(420, 480))
    m2 = msp.add_mtext("Rotated\\Pmiddle-centre\\PMTEXT", dxfattribs={
        "layer": "TEXT", "char_height": 6, "attachment_point": 5, "rotation": 45})
    m2.set_location(P(720, 400))
    msp.add_text("On a frozen layer: must be hidden", height=10,
                 dxfattribs={"layer": "LAYER_FROZEN"}).set_placement(P(0, 520))
    msp.add_text("On an off layer: must be hidden", height=10,
                 dxfattribs={"layer": "LAYER_OFF"}).set_placement(P(0, 540))
    msp.add_line(P(0, 515), P(300, 515), dxfattribs={"layer": "LAYER_FROZEN"})

    # ---- 7. Dimensions and leaders --------------------------------------
    dims = {"layer": "DIMS"}
    over = {"dimtxt": 6, "dimasz": 5, "dimexe": 2, "dimexo": 2, "dimgap": 1.5}
    msp.add_linear_dim(base=P(0, -40), p1=P(0, 0), p2=P(400, 0), override=over, dxfattribs=dims).render()
    msp.add_linear_dim(base=P(-40, 0), p1=P(0, 0), p2=P(0, 300), angle=90, override=over,
                       dxfattribs=dims).render()
    msp.add_aligned_dim(p1=P(450, 0), p2=P(760, 90), distance=-30, override=over, dxfattribs=dims).render()
    msp.add_radius_dim(center=P(200, 150), radius=60, angle=35, override=over, dxfattribs=dims).render()
    msp.add_diameter_dim(center=P(200, 150), radius=100, angle=140, override=over, dxfattribs=dims).render()
    msp.add_angular_dim_3p(base=P(260, 190), center=P(200, 150), p1=P(300, 150),
                           p2=(OX + 200 + 100 * math.cos(math.pi / 6), OY + 150 + 100 * math.sin(math.pi / 6)),
                           override=over, dxfattribs=dims).render()
    msp.add_leader([P(330, 150), P(380, 220), P(420, 220)], dxfattribs={"layer": "DIMS"})
    msp.add_text("Ø28 THRU", height=6, dxfattribs={"layer": "DIMS"}).set_placement(P(424, 217))

    # ---- 8. Things RustyDxf does not draw (must be counted, not crash) ----
    msp.add_ray(P(900, 300), (1, 0.2), dxfattribs={"layer": "GRID"})
    msp.add_xline(P(900, 320), (1, -0.1), dxfattribs={"layer": "GRID"})
    msp.add_mline([P(850, 350), P(950, 350), P(950, 420)], dxfattribs={"layer": "GRID"})
    msp.add_mesh(dxfattribs={"layer": "GRID"})
    msp.add_shape("X", P(900, 440), dxfattribs={"layer": "GRID"}) if hasattr(msp, "add_shape") else None
    mleader = msp.add_multileader_mtext("Standard") if hasattr(msp, "add_multileader_mtext") else None
    if mleader is not None:
        mleader.set_content("MULTILEADER", char_height=6)
        mleader.add_leader_line(ezdxf.render.ConnectionSide.left, [Vec2(P(860, 260))])
        mleader.build(insert=Vec2(P(900, 280)))
    try:
        msp.add_image(doc.add_image_def("missing.png", size_in_pixel=(64, 64)), P(980, 300), (60, 60))
    except Exception:
        pass
    # Paper-space entity: must not appear in the model view.
    psp = doc.layout("Layout1")
    psp.add_circle((100, 100), 5000)
    psp.add_text("PAPER SPACE ONLY", height=50).set_placement((0, 0))

    # ---- 9. Volume: a dense grid for performance -------------------------
    gx, gy, n, step = 0.0, -1700.0, 110, 10.0
    for i in range(n + 1):
        msp.add_line(P(gx + i * step, gy), P(gx + i * step, gy + n * step), dxfattribs={"layer": "GRID"})
        msp.add_line(P(gx, gy + i * step), P(gx + n * step, gy + i * step), dxfattribs={"layer": "GRID"})
    for i in range(n):
        for j in range(n):
            x, y = gx + (i + 0.5) * step, gy + (j + 0.5) * step
            kind = (i * 7 + j * 3) % 4
            attrs = {"layer": "GRID", "color": 1 + (i + j) % 6}
            if kind == 0:
                msp.add_circle(P(x, y), 3.5, dxfattribs=attrs)
            elif kind == 1:
                msp.add_arc(P(x, y), 3.5, (i * 17) % 360, (i * 17 + 90 + j * 5) % 360, dxfattribs=attrs)
            elif kind == 2:
                msp.add_lwpolyline([(OX + x - 3, OY + y - 3, 0.4), (OX + x + 3, OY + y - 3, 0),
                                    (OX + x + 3, OY + y + 3, -0.4), (OX + x - 3, OY + y + 3, 0)],
                                   format="xyb", close=True, dxfattribs=attrs)
            else:
                msp.add_blockref("BOLT", P(x, y), dxfattribs={**attrs, "xscale": 0.5, "yscale": 0.5,
                                                              "rotation": (i * 13 + j * 29) % 360})
    return doc


def main(out):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    doc = build("R2010")
    doc.saveas(out / "complex.dxf")
    doc.saveas(out / "complex_bin.dxf", fmt="bin")
    build("R2000").saveas(out / "complex_r2000.dxf")

    msp = doc.modelspace()
    counts = Counter(e.dxftype() for e in msp)
    # Exclude infinite construction lines from the reference extents.
    # ezdxf itself recurses forever on the cyclic LOOP_A/LOOP_B blocks.
    box = bbox.extents(e for e in msp if e.dxftype() not in ("RAY", "XLINE")
                       and e.dxf.layer not in ("LAYER_OFF", "LAYER_FROZEN")
                       and not (e.dxftype() == "INSERT" and e.dxf.name.startswith("LOOP_")))
    texts = sum(1 for e in msp if e.dxftype() in ("TEXT", "MTEXT"))
    print(json.dumps({
        "modelspace_counts": dict(sorted(counts.items())),
        "modelspace_total": len(msp),
        "layers": len(doc.layers),
        "blocks": [b.name for b in doc.blocks if not b.name.startswith("*")],
        "text_entities": texts,
        "extents_min": [box.extmin.x - OX, box.extmin.y - OY],
        "extents_max": [box.extmax.x - OX, box.extmax.y - OY],
        "origin": [OX, OY],
    }, indent=1))


if __name__ == "__main__":
    main(sys.argv[1])
