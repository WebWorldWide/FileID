#!/usr/bin/env python3
"""Writes shared/test-corpus/face-mesh-geometry.json.

The fixture pins the Face Mesh crop, resampling and eye-ratio steps that both engines
run (models/face_mesh.rs and FaceMeshService.swift), so their eye scores stay equal.
Expected values come from this independent float64 reference; the engines compute in
float32 and compare with a small tolerance.
"""
import json
import math
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / "shared/test-corpus/face-mesh-geometry.json"
INPUT, POINTS, MARGIN = 256, 478, 1.5
RIGHT_EYE = [33, 160, 158, 133, 153, 144]
LEFT_EYE = [362, 385, 387, 263, 373, 380]
CLOSED, OPEN = 0.14, 0.24
WIDTH, HEIGHT = 97, 61


def pixel(x, y):
    return [(7 * x + 3 * y) % 256, (x * x + 5 * y) % 256, (x * y) % 256]


def rolled(cx, cy, side, a, b, below):
    dx, dy = b[0] - a[0], b[1] - a[1]
    flip = dx * (below[1] - a[1]) - dy * (below[0] - a[0]) < 0
    angle = math.atan2(dy, dx) + (math.pi if flip else 0)
    return {"cx": cx, "cy": cy, "side": side, "cos": math.cos(angle), "sin": math.sin(angle)}


def project(c, u, v):
    lx, ly = (u / INPUT - 0.5) * c["side"], (v / INPUT - 0.5) * c["side"]
    return [c["cx"] + lx * c["cos"] - ly * c["sin"], c["cy"] + lx * c["sin"] + ly * c["cos"]]


def crop_from_detection(bbox, eye_a, eye_b, nose):
    x1, y1, x2, y2 = bbox
    side = max(x2 - x1, y2 - y1, 1.0) * MARGIN
    left, right = (eye_a, eye_b) if eye_a[0] <= eye_b[0] else (eye_b, eye_a)
    return rolled((x1 + x2) / 2, (y1 + y2) / 2, side, left, right, nose)


def crop_from_points(points):
    c = rolled(0, 0, 0, points[33], points[263], points[152])
    local = [(x * c["cos"] + y * c["sin"], -x * c["sin"] + y * c["cos"]) for x, y in points[:468]]
    lo_x, hi_x = min(p[0] for p in local), max(p[0] for p in local)
    lo_y, hi_y = min(p[1] for p in local), max(p[1] for p in local)
    mx, my = (lo_x + hi_x) / 2, (lo_y + hi_y) / 2
    side = max(hi_x - lo_x, hi_y - lo_y) * MARGIN
    return {"cx": mx * c["cos"] - my * c["sin"], "cy": mx * c["sin"] + my * c["cos"], "side": side, "cos": c["cos"], "sin": c["sin"]}


def sample(c, u, v):
    x, y = project(c, u + 0.5, v + 0.5)
    x, y = min(max(x - 0.5, 0), WIDTH - 1), min(max(y - 0.5, 0), HEIGHT - 1)
    x0, y0 = int(x), int(y)
    x1, y1 = min(x0 + 1, WIDTH - 1), min(y0 + 1, HEIGHT - 1)
    fx, fy = x - x0, y - y0
    out = []
    for k in range(3):
        top = pixel(x0, y0)[k] * (1 - fx) + pixel(x1, y0)[k] * fx
        bottom = pixel(x0, y1)[k] * (1 - fx) + pixel(x1, y1)[k] * fx
        out.append((top * (1 - fy) + bottom * fy) / 255)
    return out


def ear(points):
    d = lambda a, b: math.hypot(points[a][0] - points[b][0], points[a][1] - points[b][1])
    ratios = [(d(e[1], e[5]) + d(e[2], e[4])) / (2 * d(e[0], e[3])) for e in (RIGHT_EYE, LEFT_EYE) if d(e[0], e[3]) > 0]
    return sum(ratios) / len(ratios)


def mesh_points(cx, cy, scale, angle, gap):
    """A face-shaped point cloud: an ellipse of points, eyes with a given lid gap, chin below."""
    pts = []
    for i in range(POINTS):
        t = i * 2.399963
        r = math.sqrt((i % 468 + 0.5) / 468)
        pts.append([0.5 * r * math.cos(t), 0.62 * r * math.sin(t)])
    for eye, ex in ((RIGHT_EYE, -0.22), (LEFT_EYE, 0.22)):
        corner_out, corner_in = (ex - 0.1, ex + 0.1) if ex < 0 else (ex + 0.1, ex - 0.1)
        pts[eye[0]], pts[eye[3]] = [corner_out, -0.15], [corner_in, -0.15]
        for top, bottom, f in ((eye[1], eye[5], 0.33), (eye[2], eye[4], 0.67)):
            x = corner_out + (corner_in - corner_out) * f
            pts[top], pts[bottom] = [x, -0.15 - gap / 2], [x, -0.15 + gap / 2]
    pts[152] = [0.0, 0.6]
    ca, sa = math.cos(angle), math.sin(angle)
    return [[round(cx + scale * (x * ca - y * sa), 3), round(cy + scale * (x * sa + y * ca), 3)] for x, y in pts]


def main():
    detections = []
    for bbox, eye_a, eye_b, nose in [
        ([30.0, 12.0, 70.0, 56.0], [42.0, 28.0], [58.0, 28.0], [50.0, 38.0]),
        ([20.5, 8.25, 61.75, 47.0], [52.0, 20.0], [33.0, 26.0], [45.0, 34.0]),
        ([40.0, 10.0, 80.0, 50.0], [62.0, 40.0], [48.0, 40.0], [55.0, 28.0]),
        ([2.0, 1.0, 12.0, 30.0], [5.0, 9.0], [5.5, 20.0], [9.0, 14.0]),
    ]:
        c = crop_from_detection(bbox, eye_a, eye_b, nose)
        samples = [{"u": u, "v": v, "rgb": sample(c, u, v)} for u, v in [(0, 0), (128, 128), (37, 201), (255, 3), (90, 90), (255, 255)]]
        detections.append({"bbox": bbox, "eyeA": eye_a, "eyeB": eye_b, "nose": nose, "crop": c,
                           "project": {"u": 64.0, "v": 192.0, "expect": project(c, 64.0, 192.0)}, "samples": samples})
    meshes = []
    for cx, cy, scale, angle, gap in [(150.0, 120.0, 100.0, 0.0, 0.06), (300.5, 210.25, 64.0, 0.4, 0.015), (90.0, 400.0, 180.0, 2.9, 0.03)]:
        pts = mesh_points(cx, cy, scale, angle, gap)
        r = ear(pts)
        meshes.append({"points": pts, "crop": crop_from_points(pts), "ratio": r, "openness": min(1.0, max(0.0, (r - CLOSED) / (OPEN - CLOSED)))})
    fixture = {
        "description": "Face Mesh geometry shared by models/face_mesh.rs and FaceMeshService.swift: the first crop from a face box (1.5x the long side, levelled on the eyes, flipped when the nose is above them), the refit crop from 478 mesh points, bilinear resampling into the 256x256 input, and the eye aspect ratio and openness. Generated by shared/scripts/eval/face_mesh_geometry_fixture.py in float64; engines compare within 1e-3.",
        "image": {"width": WIDTH, "height": HEIGHT, "pixel": "r=(7x+3y)%256, g=(x*x+5y)%256, b=(x*y)%256"},
        "detections": detections,
        "meshes": meshes,
        "openness": [{"ratio": r, "expect": min(1.0, max(0.0, (r - CLOSED) / (OPEN - CLOSED)))} for r in (0.05, 0.14, 0.175, 0.2, 0.24, 0.31)],
    }
    text = json.dumps(fixture, indent=1)
    text = re.sub(r"\[\s+(-?[\d.e-]+),\s+(-?[\d.e-]+)(?:,\s+(-?[\d.e-]+))?\s+\]",
                  lambda m: "[" + ", ".join(g for g in m.groups() if g is not None) + "]", text)
    OUT.write_text(text + "\n", encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
