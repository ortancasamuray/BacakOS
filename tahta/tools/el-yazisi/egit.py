"""Trains tahta's handwriting symbol classifier with plain numpy.

Model (must match tahta/src/handwriting.rs):
  conv 1->16 3x3 pad1, ReLU, maxpool 2   -> 16x14x14
  conv 16->32 3x3 pad1, ReLU, maxpool 2  -> 32x7x7
  fc 1568->128, ReLU
  fc 128->C

usage: python3 -I egit.py semboller.npz model.bin [epochs]
"""
import multiprocessing as mp
import struct
import sys
import time

import numpy as np
from PIL import Image

from hazirla import PEN, render_points

rng = np.random.default_rng(7)


# ---------- augmentation (runs in worker processes) ----------

def augment_one(args):
    """Random affine on the centerline points, redrawn with tahta's pen."""
    pts, seed = args
    r = np.random.default_rng(seed)
    c = (pts.min(0) + pts.max(0)) / 2
    ang = np.radians(r.normal(0, 7))
    sh = r.normal(0, 0.15)
    sx, sy = r.uniform(0.85, 1.15, 2)
    m = np.array([[np.cos(ang), -np.sin(ang)], [np.sin(ang), np.cos(ang)]]) @ np.array([[1, sh], [0, 1]]) @ np.diag([sx, sy])
    q = (pts - c) @ m.T + r.normal(0, 0.12, pts.shape)
    return render_points(q.astype(np.float32), r.uniform(PEN - 0.3, PEN + 0.3))


def plain(pts):
    return render_points(pts)


# ---------- layers ----------

def conv_forward(x, W, b):
    B, C, H, Wd = x.shape
    xp = np.pad(x, ((0, 0), (0, 0), (1, 1), (1, 1)))
    win = np.lib.stride_tricks.sliding_window_view(xp, (3, 3), axis=(2, 3))  # B,C,H,W,3,3
    cols = win.transpose(0, 2, 3, 1, 4, 5).reshape(B * H * Wd, C * 9)
    out = cols @ W.reshape(W.shape[0], -1).T + b
    return out.reshape(B, H, Wd, -1).transpose(0, 3, 1, 2), cols


def conv_backward(dout, cols, x_shape, W):
    B, C, H, Wd = x_shape
    O = W.shape[0]
    d = dout.transpose(0, 2, 3, 1).reshape(-1, O)
    dW = (d.T @ cols).reshape(W.shape)
    db = d.sum(0)
    dcols = (d @ W.reshape(O, -1)).reshape(B, H, Wd, C, 3, 3)
    dxp = np.zeros((B, C, H + 2, Wd + 2), np.float32)
    for ky in range(3):
        for kx in range(3):
            dxp[:, :, ky: ky + H, kx: kx + Wd] += dcols[:, :, :, :, ky, kx].transpose(0, 3, 1, 2)
    return dxp[:, :, 1:-1, 1:-1], dW, db


def pool_forward(x):
    B, C, H, W = x.shape
    r = x.reshape(B, C, H // 2, 2, W // 2, 2)
    out = r.max(axis=(3, 5))
    mask = r == out[:, :, :, None, :, None]
    return out, mask


def pool_backward(dout, mask):
    d = mask * dout[:, :, :, None, :, None]
    # ties: split evenly so the gradient isn't duplicated
    d = d / np.maximum(mask.sum(axis=(3, 5), keepdims=True), 1)
    B, C, h, _, w, _ = d.shape
    return d.reshape(B, C, h * 2, w * 2)


def init(C):
    def he(shape, fan_in):
        return (rng.standard_normal(shape) * np.sqrt(2.0 / fan_in)).astype(np.float32)
    return {
        "c1w": he((16, 1, 3, 3), 9), "c1b": np.zeros(16, np.float32),
        "c2w": he((32, 16, 3, 3), 144), "c2b": np.zeros(32, np.float32),
        "f1w": he((128, 1568), 1568), "f1b": np.zeros(128, np.float32),
        "f2w": he((C, 128), 128), "f2b": np.zeros(C, np.float32),
    }


def forward(P, x, train=False):
    c1, cols1 = conv_forward(x, P["c1w"], P["c1b"])
    r1 = np.maximum(c1, 0)
    p1, m1 = pool_forward(r1)
    c2, cols2 = conv_forward(p1, P["c2w"], P["c2b"])
    r2 = np.maximum(c2, 0)
    p2, m2 = pool_forward(r2)
    flat = p2.reshape(len(x), -1)
    h = flat @ P["f1w"].T + P["f1b"]
    hr = np.maximum(h, 0)
    drop = None
    if train:
        drop = (rng.random(hr.shape) > 0.3).astype(np.float32) / 0.7
        hr = hr * drop
    logits = hr @ P["f2w"].T + P["f2b"]
    cache = (x, c1, cols1, m1, p1, c2, cols2, m2, flat, h, hr, drop)
    return logits, cache


def backward(P, cache, dlogits):
    x, c1, cols1, m1, p1, c2, cols2, m2, flat, h, hr, drop = cache
    G = {}
    G["f2w"] = dlogits.T @ hr
    G["f2b"] = dlogits.sum(0)
    dhr = dlogits @ P["f2w"]
    if drop is not None:
        dhr = dhr * drop
    dh = dhr * (h > 0)
    G["f1w"] = dh.T @ flat
    G["f1b"] = dh.sum(0)
    dflat = dh @ P["f1w"]
    dp2 = dflat.reshape(-1, 32, 7, 7)
    dr2 = pool_backward(dp2, m2)
    dc2 = dr2 * (c2 > 0)
    dp1, G["c2w"], G["c2b"] = conv_backward(dc2, cols2, p1.shape, P["c2w"])
    dr1 = pool_backward(dp1, m1)
    dc1 = dr1 * (c1 > 0)
    _, G["c1w"], G["c1b"] = conv_backward(dc1, cols1, x.shape, P["c1w"])
    return G


def softmax_xent(logits, y):
    z = logits - logits.max(1, keepdims=True)
    e = np.exp(z)
    p = e / e.sum(1, keepdims=True)
    loss = -np.log(p[np.arange(len(y)), y] + 1e-9).mean()
    d = p
    d[np.arange(len(y)), y] -= 1
    return loss, d / len(y)


def to_input(x):
    x = x.astype(np.float32)
    if x.max() > 1.5:  # uint8 probe images
        x = x / 255.0
    return x[:, None, :, :]


def evaluate(P, X, Y, C):
    preds, top3 = [], []
    for i in range(0, len(X), 1024):
        logits, _ = forward(P, to_input(X[i: i + 1024]))
        preds.append(logits.argmax(1))
        top3.append(np.argsort(-logits, 1)[:, :3])
    pred = np.concatenate(preds)
    t3 = np.concatenate(top3)
    acc = (pred == Y).mean()
    acc3 = (t3 == Y[:, None]).any(1).mean()
    print(f"  ilk 3 içinde: {acc3:.4f}", flush=True)
    per = {C[c]: round(float((pred[Y == c] == c).mean()), 3) for c in range(len(C)) if (Y == c).any()}
    conf = {}
    for t, p in zip(Y, pred):
        if t != p:
            conf[(C[t], C[p])] = conf.get((C[t], C[p]), 0) + 1
    top = sorted(conf.items(), key=lambda kv: -kv[1])[:12]
    return acc, per, top


def export(P, C, path, X_probe):
    with open(path, "wb") as f:
        f.write(b"TZK1")
        names = "\n".join(C).encode()
        f.write(struct.pack("<II", len(C), len(names)))
        f.write(names)
        for k in ("c1w", "c1b", "c2w", "c2b", "f1w", "f1b", "f2w", "f2b"):
            f.write(P[k].astype("<f4").tobytes())
    # parity probe for the Rust test: inputs + expected logits
    logits, _ = forward(P, to_input(X_probe))
    with open(path + ".probe", "wb") as f:
        f.write(struct.pack("<I", len(X_probe)))
        f.write(X_probe.astype(np.uint8).tobytes())
        f.write(logits.astype("<f4").tobytes())


def main(data, out, epochs):
    d = np.load(data)
    C = [str(c) for c in d["classes"]]
    def split(w):
        pts, off = d[f"p_{w}"], d[f"o_{w}"]  # load each array once
        return [pts[a:b] for a, b in zip(off[:-1], off[1:])]
    Ptr, Ytr, Pte, Yte = split("train"), d["y_train"], split("test"), d["y_test"]
    P = init(len(C))
    M = {k: np.zeros_like(v) for k, v in P.items()}
    V = {k: np.zeros_like(v) for k, v in P.items()}
    step = 0
    batch = 256
    pool = mp.Pool(mp.cpu_count())
    # Test images: drawn exactly as tahta draws, no augmentation.
    Xte = np.stack(pool.map(plain, Pte, chunksize=256))
    Xtr_len = len(Ptr)
    total_steps = epochs * (Xtr_len // batch)
    for ep in range(epochs):
        t0 = time.time()
        seeds = rng.integers(0, 2**31, Xtr_len)
        Xaug = np.stack(pool.map(augment_one, zip(Ptr, seeds), chunksize=256))
        order = rng.permutation(Xtr_len)
        losses = []
        for i in range(0, len(order) - batch + 1, batch):
            idx = order[i: i + batch]
            logits, cache = forward(P, to_input(Xaug[idx]), train=True)
            loss, dl = softmax_xent(logits, Ytr[idx])
            G = backward(P, cache, dl)
            step += 1
            lr = 2e-3 * 0.5 * (1 + np.cos(np.pi * step / total_steps))
            for k in P:
                g = G[k] + 1e-4 * P[k]
                M[k] = 0.9 * M[k] + 0.1 * g
                V[k] = 0.999 * V[k] + 0.001 * g * g
                mh = M[k] / (1 - 0.9 ** step)
                vh = V[k] / (1 - 0.999 ** step)
                P[k] -= (lr * mh / (np.sqrt(vh) + 1e-8)).astype(np.float32)
            losses.append(loss)
        acc, _, _ = evaluate(P, Xte, Yte, C)
        print(f"epok {ep + 1}/{epochs} kayıp {np.mean(losses):.4f} test {acc:.4f} ({time.time() - t0:.0f} s)", flush=True)
    acc, per, top = evaluate(P, Xte, Yte, C)
    print("sınıf başına:", per)
    print("en sık karışanlar (gerçek, tahmin): sayı", top)
    probe = (Xte[:8] * 255).round().astype(np.uint8)
    export(P, C, out, probe)
    print("yazıldı:", out)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 12)
