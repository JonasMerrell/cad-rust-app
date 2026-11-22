"""Block-stroke glyphs as sketch items (closed polylines), so no overlapping font contours are lost."""
import math

def _rect(x0, y0, x1, y1):
    return {"type": "polyline", "closed": True, "points": [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]}

def _bar(p, q, t):
    (x0, y0), (x1, y1) = p, q
    dx, dy = x1 - x0, y1 - y0; L = math.hypot(dx, dy); nx, ny = -dy / L * t / 2, dx / L * t / 2
    return {"type": "polyline", "closed": True, "points": [[x0 + nx, y0 + ny], [x1 + nx, y1 + ny], [x1 - nx, y1 - ny], [x0 - nx, y0 - ny]]}

def _arc_band(cx, cy, rx, ry, t, a0, a1, n=10):
    pts_o = [[cx + rx * math.cos(math.radians(a0 + (a1 - a0) * i / n)), cy + ry * math.sin(math.radians(a0 + (a1 - a0) * i / n))] for i in range(n + 1)]
    pts_i = [[cx + (rx - t) * math.cos(math.radians(a0 + (a1 - a0) * i / n)), cy + (ry - t) * math.sin(math.radians(a0 + (a1 - a0) * i / n))] for i in range(n, -1, -1)]
    return {"type": "polyline", "closed": True, "points": [[round(a, 3), round(b, 3)] for a, b in pts_o + pts_i]}

SEG = {"0": "abcdef", "1": "bc", "2": "abged", "3": "abgcd", "4": "fgbc", "5": "afgcd", "6": "afgedc", "7": "abc", "8": "abcdefg", "9": "abcdfg"}

def glyph(ch, x, y, h, w):
    t = 0.17 * h
    it = []
    if ch in SEG:
        s = SEG[ch]; m = y + h / 2
        if "a" in s: it.append(_rect(x, y + h - t, x + w, y + h))
        if "g" in s: it.append(_rect(x, m - t / 2, x + w, m + t / 2))
        if "d" in s: it.append(_rect(x, y, x + w, y + t))
        if "f" in s: it.append(_rect(x, m, x + t, y + h))
        if "b" in s: it.append(_rect(x + w - t, m, x + w, y + h))
        if "e" in s: it.append(_rect(x, y, x + t, m))
        if "c" in s: it.append(_rect(x + w - t, y, x + w, m))
        if ch == "1": it = [_rect(x + w / 2 - t / 2, y, x + w / 2 + t / 2, y + h)]
        if ch == "7": it = [_rect(x, y + h - t, x + w, y + h), _bar((x + w - t / 2, y + h - t), (x + w * 0.35, y), t)]
        return it
    if ch == "D":
        return [_rect(x, y, x + t, y + h), _rect(x, y, x + w * 0.45, y + t), _rect(x, y + h - t, x + w * 0.45, y + h),
                _arc_band(x + w * 0.45, y + h / 2, w * 0.55, h / 2, t, -90, 90)]
    if ch == "C":
        return [_arc_band(x + w / 2, y + h / 2, w / 2, h / 2, t, 40, 320, 14)]
    if ch == "O":
        return [_arc_band(x + w / 2, y + h / 2, w / 2, h / 2, t, 0, 180, 10), _arc_band(x + w / 2, y + h / 2, w / 2, h / 2, t, 180, 360, 10)]
    if ch == "E":
        return [_rect(x, y, x + t, y + h), _rect(x, y + h - t, x + w, y + h), _rect(x, y + h / 2 - t / 2, x + w * 0.8, y + h / 2 + t / 2), _rect(x, y, x + w, y + t)]
    if ch == "L":
        return [_rect(x, y, x + t, y + h), _rect(x, y, x + w, y + t)]
    if ch == "T":
        return [_rect(x, y + h - t, x + w, y + h), _rect(x + w / 2 - t / 2, y, x + w / 2 + t / 2, y + h - t)]
    if ch == "V":
        return [_bar((x + t / 2, y + h), (x + w / 2, y + t / 2), t), _bar((x + w - t / 2, y + h), (x + w / 2, y + t / 2), t)]
    if ch == "A":
        return [_bar((x + t / 2, y), (x + w / 2, y + h - t / 2), t), _bar((x + w - t / 2, y), (x + w / 2, y + h - t / 2), t), _rect(x + w * 0.25, y + h * 0.3, x + w * 0.75, y + h * 0.3 + t)]
    if ch == "W":
        return [_bar((x + t / 2, y + h), (x + w * 0.25, y + t / 2), t), _bar((x + w * 0.25, y + t / 2), (x + w / 2, y + h * 0.7), t),
                _bar((x + w / 2, y + h * 0.7), (x + w * 0.75, y + t / 2), t), _bar((x + w * 0.75, y + t / 2), (x + w - t / 2, y + h), t)]
    raise KeyError(ch)

def text_items(txt, x, y, h, mirror=False):
    """Glyphs left to right from (x, y) baseline; mirror=True writes towards -x (for a flipped plane whose sketch x is world -X)."""
    out = []; cx = x
    for ch in txt:
        w = (1.25 if ch == "W" else 0.62) * h
        if ch == " ":
            cx += 0.5 * h; continue
        out += glyph(ch, cx, y, h, w)
        cx += w + 0.2 * h
    return out, len([c for c in txt if c != " "])
