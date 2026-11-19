#!/usr/bin/env python3
"""Turn a `tmux capture-pane -e -p` dump into an SVG terminal window.

Usage: term2svg.py <capture.txt> <out.svg> "<window title>" "<aria label>"

The style matches assets/demo.svg: same font, cell size, colors and frame.
"""
import html
import re
import sys

CW, LH, PX, PY = 8.4, 21.0, 18.0, 18.0
FG, BG = "#d7dae0", "#16171b"
ANSI = [
    "#16171b", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
    "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#f0f0f0",
]


def c256(n):
    if n < 16:
        return ANSI[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[(n // 6) % 6], steps[n % 6])
    v = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


def sgr(params, st):
    p = [int(x) if x else 0 for x in params.split(";")] if params else [0]
    i = 0
    while i < len(p):
        n = p[i]
        if n == 0:
            st.clear()
        elif n == 1:
            st["bold"] = True
        elif n == 2:
            st["dim"] = True
        elif n == 3:
            st["italic"] = True
        elif n == 4:
            st["ul"] = True
        elif n == 7:
            st["rev"] = True
        elif n == 22:
            st.pop("bold", None)
            st.pop("dim", None)
        elif n == 23:
            st.pop("italic", None)
        elif n == 24:
            st.pop("ul", None)
        elif n == 27:
            st.pop("rev", None)
        elif 30 <= n <= 37:
            st["fg"] = ANSI[n - 30]
        elif 90 <= n <= 97:
            st["fg"] = ANSI[n - 90 + 8]
        elif n == 39:
            st.pop("fg", None)
        elif 40 <= n <= 47:
            st["bg"] = ANSI[n - 40]
        elif 100 <= n <= 107:
            st["bg"] = ANSI[n - 100 + 8]
        elif n == 49:
            st.pop("bg", None)
        elif n in (38, 48) and i + 1 < len(p):
            key = "fg" if n == 38 else "bg"
            if p[i + 1] == 5 and i + 2 < len(p):
                st[key] = c256(p[i + 2])
                i += 2
            elif p[i + 1] == 2 and i + 4 < len(p):
                st[key] = "#%02x%02x%02x" % tuple(p[i + 2 : i + 5])
                i += 4
        i += 1


def parse(text):
    rows = []
    st = {}
    for line in text.split("\n"):
        cells = []
        for m in re.finditer(r"\x1b\[([0-9;]*)m|\x1b\[[0-9;?]*[A-Za-z]|(.)", line):
            if m.group(2) is not None:
                cells.append((m.group(2), dict(st)))
            elif m.group(0).endswith("m"):
                sgr(m.group(1), st)
        rows.append(cells)
    while rows and not any(ch.strip() or s.get("bg") for ch, s in rows[-1]):
        rows.pop()
    return rows


def colors(s):
    fg, bg = s.get("fg", FG), s.get("bg")
    if s.get("rev"):
        fg, bg = (bg or BG), fg
    return fg, bg


def render(rows, title, label):
    w = max((len(r) for r in rows), default=1)
    h = len(rows)
    width = int(w * CW + 2 * PX + 0.999)
    height = int(PY + 34 + (h - 1) * LH + 20 + 0.999)
    o = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img" aria-label="{html.escape(label)}">',
        "<style>text{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;"
        "font-size:14px;white-space:pre}</style>",
        f'<rect x="0.5" y="0.5" width="{width - 1}" height="{height - 1}" rx="12" fill="{BG}" stroke="#3a3d46"/>',
        '<circle cx="22" cy="19" r="6" fill="#ff5f57"/><circle cx="42" cy="19" r="6" fill="#febc2e"/>'
        '<circle cx="62" cy="19" r="6" fill="#28c840"/>',
        f'<text x="{width / 2}" y="24" text-anchor="middle" fill="#9a9ea8" style="font-size:12px">'
        f"{html.escape(title)}</text>",
    ]
    top = PY + 34
    for y, row in enumerate(rows):
        base = top + y * LH
        for x, (ch, s) in enumerate(row):
            _, bg = colors(s)
            if bg and bg != BG:
                o.append(
                    f'<rect x="{PX + x * CW:.1f}" y="{base - 15:.1f}" width="{CW + 0.3:.1f}" '
                    f'height="{LH}" fill="{bg}"/>'
                )
        x = 0
        while x < len(row):
            ch, s = row[x]
            if not ch.strip():
                x += 1
                continue
            key = (colors(s)[0], s.get("bold"), s.get("dim"), s.get("ul"), s.get("italic"))
            start, run = x, ""
            while x < len(row) and row[x][0].strip() and (
                colors(row[x][1])[0], row[x][1].get("bold"), row[x][1].get("dim"),
                row[x][1].get("ul"), row[x][1].get("italic"),
            ) == key:
                run += row[x][0]
                x += 1
            attrs = f'fill="{key[0]}"'
            if key[1]:
                attrs += ' font-weight="700"'
            if key[2]:
                attrs += ' opacity=".55"'
            if key[3]:
                attrs += ' text-decoration="underline"'
            if key[4]:
                attrs += ' font-style="italic"'
            o.append(
                f'<text x="{PX + start * CW:.1f}" y="{base:.1f}" textLength="{(x - start) * CW:.1f}" '
                f'lengthAdjust="spacingAndGlyphs" {attrs}>{html.escape(run)}</text>'
            )
    o.append("</svg>")
    return "\n".join(o) + "\n"


if __name__ == "__main__":
    src, out, title, label = sys.argv[1:5]
    with open(src, encoding="utf-8") as f:
        rows = parse(f.read())
    with open(out, "w", encoding="utf-8") as f:
        f.write(render(rows, title, label))
