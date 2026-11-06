"""Builds the symbol training set for tahta's handwriting recognizer.

Every sample is reduced to its *centerline* — images are thinned to a
1-px skeleton (Zhang–Suen), synthetic strokes are already lines — and
stored as a point cloud. Training re-draws those points with the same
pen tahta uses at run time (`render_points`, mirroring
`handwriting.rs::render`), so the classifier never sees dataset texture
(MNIST vs EMNIST blur and stroke width) — only shape.

Sources: MNIST (digits), EMNIST byclass (lowercase letters), HASYv2
(+, √, π and extra digits/letters), synthetic ( and ) and +.
Every sample goes through `normalize` — crop to ink, fit the longer side
into 20 px keeping the aspect ratio, center in 28x28 — the same rule
tahta's Rust side applies to strokes rendered from the board.

usage: python3 -I hazirla.py <veri-dizini> <cikti.npz>
"""
import csv
import gzip
import sys

import numpy as np
from PIL import Image, ImageDraw

CLASSES = list("0123456789xyabcsinotlge+()") + ["√", "π"]
IDX = {c: i for i, c in enumerate(CLASSES)}
CAP = 4000  # samples per class per split source cap (keeps classes balanced)
rng = np.random.default_rng(1234)


def read_idx(path):
    with gzip.open(path, "rb") as f:
        data = f.read()
    ndim = data[3]
    dims = [int.from_bytes(data[4 + 4 * i: 8 + 4 * i], "big") for i in range(ndim)]
    return np.frombuffer(data, dtype=np.uint8, offset=4 + 4 * ndim).reshape(dims)


def normalize(img):
    """img: float32 HxW, ink=1. Returns uint8 28x28."""
    ys, xs = np.nonzero(img > 0.25)
    if len(ys) == 0:
        return None
    crop = img[ys.min(): ys.max() + 1, xs.min(): xs.max() + 1]
    h, w = crop.shape
    s = 20.0 / max(h, w)
    nh, nw = max(1, round(h * s)), max(1, round(w * s))
    pil = Image.fromarray((np.clip(crop, 0, 1) * 255).astype(np.uint8))
    pil = pil.resize((nw, nh), Image.BILINEAR)
    out = np.zeros((28, 28), np.uint8)
    y0, x0 = (28 - nh) // 2, (28 - nw) // 2
    out[y0: y0 + nh, x0: x0 + nw] = np.asarray(pil)
    return out


def thicken(u8):
    """HASY ink is 1 px at 32x32 — after scaling it's far thinner than
    MNIST/EMNIST and than the board rendering; dilate to ~2 px."""
    a = u8.astype(np.float32) / 255
    p = np.pad(a, 1)
    m = np.maximum.reduce([p[dy: dy + 28, dx: dx + 28] for dy in range(3) for dx in range(3)])
    return (np.maximum(a, m * 0.85) * 255).astype(np.uint8)


PEN = 2.4  # must match handwriting.rs


def draw_strokes(strokes, width=None):
    """Polylines (arbitrary units) -> dense centerline points (N, 2) x,y."""
    pts = np.concatenate([np.asarray(s, np.float32) for s in strokes])
    span = max(float((pts.max(0) - pts.min(0)).max()), 1e-3)
    step = span / 60.0
    out = []
    for s in strokes:
        s = np.asarray(s, np.float32)
        if len(s) == 1:
            out.append(s)
        for a, b in zip(s, s[1:]):
            n = max(1, int(np.ceil(np.linalg.norm(b - a) / step)))
            t = np.linspace(0, 1, n, endpoint=False)[:, None]
            out.append(a + (b - a) * t)
        out.append(s[-1:])
    # Same scale as skeletons from images (28-px units, ~20 px extent), so
    # augmentation noise means the same thing for every sample.
    pts = np.concatenate(out)
    pts = (pts - pts.min(0)) * (20.0 / span)
    return pts.astype(np.float32)


def zhang_suen(img):
    img = img.astype(np.uint8).copy()
    changed = True
    while changed:
        changed = False
        for step in (0, 1):
            P = np.pad(img, 1)
            p2, p3, p4 = P[:-2, 1:-1], P[:-2, 2:], P[1:-1, 2:]
            p5, p6, p7 = P[2:, 2:], P[2:, 1:-1], P[2:, :-2]
            p8, p9 = P[1:-1, :-2], P[:-2, :-2]
            seq = [p2, p3, p4, p5, p6, p7, p8, p9, p2]
            B = p2 + p3 + p4 + p5 + p6 + p7 + p8 + p9
            A = sum(((seq[k] == 0) & (seq[k + 1] == 1)).astype(np.uint8) for k in range(8))
            if step == 0:
                c = (p2 * p4 * p6 == 0) & (p4 * p6 * p8 == 0)
            else:
                c = (p2 * p4 * p8 == 0) & (p2 * p6 * p8 == 0)
            m = (img == 1) & (B >= 2) & (B <= 6) & (A == 1) & c
            if m.any():
                img[m] = 0
                changed = True
    return img


def skeleton_points(u8):
    """28x28 normalized image -> centerline points (x, y) in 28-px units."""
    if u8 is None:
        return None
    big = np.asarray(Image.fromarray(u8).resize((56, 56), Image.BILINEAR), np.float32) / 255
    sk = zhang_suen(big > 0.35)
    ys, xs = np.nonzero(sk)
    if len(xs) < 2:
        return None
    return np.stack([xs, ys], 1).astype(np.float32) / 2.0


GRID = (np.stack(np.meshgrid(np.arange(28), np.arange(28)), -1).reshape(-1, 2) + 0.5).astype(np.float32)


def render_points(pts, pen=PEN):
    """Centerline points -> 28x28 float image, exactly like handwriting.rs."""
    lo, hi = pts.min(0), pts.max(0)
    ext = max(float((hi - lo).max()), 1e-3)
    s = (20.0 - pen) / ext
    nw, nh = (hi - lo) * s + pen
    origin = np.floor((28 - np.array([nw, nh])) / 2) + pen / 2
    q = origin + (pts - lo) * s
    d = np.sqrt(((GRID[:, None, :] - q[None, :, :]) ** 2).sum(-1)).min(1)
    return np.clip(pen / 2 + 0.5 - d, 0, 1).reshape(28, 28).astype(np.float32)


def synth_paren(close):
    a0 = rng.uniform(np.radians(110), np.radians(140))
    sweep = rng.uniform(np.radians(70), np.radians(120))
    r = 1.0
    t = np.linspace(a0, a0 + sweep, 30)
    xs, ys = r * np.cos(t), r * np.sin(t)
    xs += rng.normal(0, 0.012, t.shape)
    ys += rng.normal(0, 0.012, t.shape)
    if close:
        xs = -xs
    # rotate so the arc stands upright (center angle -> pi or 0)
    mid = a0 + sweep / 2
    rot = (np.pi - mid) if not close else -(np.pi - mid)
    rot += rng.normal(0, 0.1)
    c, s = np.cos(rot), np.sin(rot)
    x2, y2 = c * xs - s * ys, s * xs + c * ys
    return draw_strokes([list(zip(x2, -y2))], rng.uniform(1.6, 3.2))


def synth_plus():
    def seg(cx, cy, ang, ln):
        dx, dy = np.cos(ang) * ln / 2, np.sin(ang) * ln / 2
        n = 10
        return [(cx + dx * (k / n * 2 - 1) + rng.normal(0, 0.01), cy + dy * (k / n * 2 - 1) + rng.normal(0, 0.01)) for k in range(n + 1)]
    h = seg(rng.normal(0, 0.06), rng.normal(0, 0.06), rng.normal(0, 0.12), rng.uniform(0.8, 1.2))
    v = seg(rng.normal(0, 0.06), rng.normal(0, 0.06), np.pi / 2 + rng.normal(0, 0.12), rng.uniform(0.8, 1.2))
    return draw_strokes([h, v], rng.uniform(1.6, 3.2))


def jit(pts, sd=0.015):
    return [(x + rng.normal(0, sd), y + rng.normal(0, sd)) for x, y in pts]


def bend(a, b, n=8, amount=0.0):
    """Points from a to b, bowed sideways by `amount` (fraction of length)."""
    (ax, ay), (bx, by) = a, b
    nx, ny = -(by - ay), bx - ax
    return [(ax + (bx - ax) * t + nx * amount * np.sin(np.pi * t), ay + (by - ay) * t + ny * amount * np.sin(np.pi * t)) for t in np.linspace(0, 1, n)]


def slant(strokes, k):
    return [[(x + k * (1 - y), y) for x, y in s] for s in strokes]


def synth_one():
    """Turkish-style 1: a flag up to the top, then the stem; often a base."""
    top = (0.35 + rng.normal(0, 0.03), 0.0)
    flag = (rng.uniform(0.0, 0.18), rng.uniform(0.18, 0.4))
    strokes = [jit(bend(flag, top, 5, rng.normal(0, 0.08)) + bend(top, (top[0] + rng.normal(0, 0.03), 1.0), 10, rng.normal(0, 0.02))[1:])]
    if rng.random() < 0.3:
        strokes.append(jit(bend((rng.uniform(0.05, 0.2), 1.0), (rng.uniform(0.5, 0.65), 1.0), 6)))
    return draw_strokes(slant(strokes, rng.normal(0.1, 0.08)), rng.uniform(1.6, 3.2))


def synth_seven():
    """7, often with the continental crossbar."""
    a, b = (rng.uniform(-0.02, 0.08), rng.uniform(0.0, 0.08)), (0.62, rng.uniform(-0.03, 0.05))
    c = (rng.uniform(0.15, 0.35), 1.0)
    strokes = [jit(bend(a, b, 6, rng.normal(0, 0.04)) + bend(b, c, 10, rng.normal(0, 0.06))[1:])]
    if rng.random() < 0.6:
        y = rng.uniform(0.45, 0.6)
        strokes.append(jit(bend((rng.uniform(0.1, 0.25), y + 0.03), (rng.uniform(0.5, 0.65), y - 0.03), 6)))
    return draw_strokes(slant(strokes, rng.normal(0.08, 0.08)), rng.uniform(1.6, 3.2))


def take(xs, n):
    if len(xs) > n:
        sel = rng.choice(len(xs), n, replace=False)
        return [xs[i] for i in sel]
    return list(xs)


def main(V, out):
    split = {"train": [], "test": []}

    def add(which, cls, sample):
        """sample: a normalized 28x28 uint8 image, or centerline points."""
        if sample is not None:
            split[which].append((sample, IDX[cls]))

    # MNIST digits
    for which, pre in (("train", "train"), ("test", "t10k")):
        X = read_idx(f"{V}/mnist/{pre}-images-idx3-ubyte.gz")
        Y = read_idx(f"{V}/mnist/{pre}-labels-idx1-ubyte.gz")
        cap = CAP if which == "train" else CAP // 4
        for d in range(10):
            for x in take(list(X[Y == d]), cap):
                add(which, str(d), normalize(x.astype(np.float32) / 255))

    # EMNIST byclass lowercase letters (images are transposed)
    letters = "xyabcsinotlge"
    for which in ("train", "test"):
        X = read_idx(f"{V}/emnist/gzip/emnist-byclass-{which}-images-idx3-ubyte.gz")
        Y = read_idx(f"{V}/emnist/gzip/emnist-byclass-{which}-labels-idx1-ubyte.gz")
        cap = CAP if which == "train" else CAP // 4
        for ch in letters:
            label = 36 + ord(ch) - ord("a")
            for x in take(list(X[Y == label]), cap):
                add(which, ch, normalize(x.T.astype(np.float32) / 255))

    # HASYv2: + √ π and extra digits/letters; users split by id parity
    want = {"+": "+", "\\sqrt{}": "√", "\\pi": "π"}
    want.update({c: c for c in "0123456789" + letters})
    for r in csv.DictReader(open(f"{V}/hasy/hasy-data-labels.csv")):
        cls = want.get(r["latex"])
        if cls is None:
            continue
        im = np.asarray(Image.open(f"{V}/hasy/{r['path']}").convert("L"), np.float32) / 255
        n = normalize(1.0 - im)
        if n is None:
            continue
        which = "test" if rng.random() < 0.15 else "train"
        add(which, cls, n)

    # synthetic
    for which, n in (("train", 3000), ("test", 500)):
        for _ in range(n):
            add(which, "(", synth_paren(False))
            add(which, ")", synth_paren(True))
        for _ in range(n // 2):
            add(which, "+", synth_plus())
        for _ in range(2 * n // 3):
            add(which, "1", synth_one())
            add(which, "7", synth_seven())

    # Thin the images to centerlines in parallel (synthetic samples
    # already are point clouds).
    import multiprocessing as mp

    with mp.Pool(mp.cpu_count()) as pool:
        for which in split:
            raw = split[which]
            is_img = [x.dtype == np.uint8 for x, _ in raw]
            imgs = [x for (x, _), im in zip(raw, is_img) if im]
            pts = iter(pool.map(skeleton_points, imgs, chunksize=256))
            done = []
            for (x, c), im in zip(raw, is_img):
                p = next(pts) if im else x
                if p is not None and len(p) >= 2:
                    done.append((p, c))
            split[which] = done

    arrays = {}
    for which, items in split.items():
        lens = np.array([len(p) for p, _ in items])
        arrays[f"p_{which}"] = np.concatenate([p for p, _ in items]).astype(np.float32)
        arrays[f"o_{which}"] = np.concatenate([[0], np.cumsum(lens)]).astype(np.int64)
        arrays[f"y_{which}"] = np.array([c for _, c in items], np.int64)
        counts = np.bincount(arrays[f"y_{which}"], minlength=len(CLASSES))
        print(which, len(items), dict(zip(CLASSES, counts.tolist())))
    np.savez_compressed(out, classes=np.array(CLASSES), **arrays)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
