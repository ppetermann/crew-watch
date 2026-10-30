"""Render a `tmux capture-pane -e -p` dump as a fixed-grid HTML page.

Usage: python3 -I ansi2html.py <capture.ansi> > page.html

Handles the SGR subset tmux emits (reset, bold/dim/italic/underline/reverse,
16/256/true colour). Wide characters (the STATE emoji) get a two-cell box, so
the grid stays aligned whatever width the fallback font gives them. Geometry
is fixed by CELL_W/CELL_H/PAD below; capture.sh sizes the browser window from
the same numbers.
"""

import html
import re
import sys
import unicodedata

CELL_W = 9  # px; JetBrains Mono's advance is 0.6em at 15px
CELL_H = 19
PAD = 16

BG = "#16191f"
FG = "#c8ccd4"
# One Dark-ish 16-colour palette: black, red, green, yellow, blue, magenta,
# cyan, white, then the bright variants.
PALETTE16 = [
    "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
    "#5c6370", "#ef7b84", "#a9d48a", "#f0d08c", "#72c0ff", "#d78aee", "#67c7d3", "#e6e9ef",
]


def color256(n):
    if n < 16:
        return PALETTE16[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[(n // 6) % 6], steps[n % 6])
    v = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


class Style:
    def __init__(self):
        self.reset()

    def reset(self):
        self.fg = self.bg = None
        self.bold = self.dim = self.italic = self.underline = self.reverse = False

    def apply(self, params):
        codes = [int(p) if p else 0 for p in params.split(";")] if params else [0]
        i = 0
        while i < len(codes):
            c = codes[i]
            if c == 0:
                self.reset()
            elif c == 1:
                self.bold = True
            elif c == 2:
                self.dim = True
            elif c == 3:
                self.italic = True
            elif c == 4:
                self.underline = True
            elif c == 7:
                self.reverse = True
            elif c == 22:
                self.bold = self.dim = False
            elif c == 23:
                self.italic = False
            elif c == 24:
                self.underline = False
            elif c == 27:
                self.reverse = False
            elif 30 <= c <= 37:
                self.fg = PALETTE16[c - 30]
            elif 90 <= c <= 97:
                self.fg = PALETTE16[c - 90 + 8]
            elif 40 <= c <= 47:
                self.bg = PALETTE16[c - 40]
            elif 100 <= c <= 107:
                self.bg = PALETTE16[c - 100 + 8]
            elif c == 39:
                self.fg = None
            elif c == 49:
                self.bg = None
            elif c in (38, 48) and i + 1 < len(codes):
                if codes[i + 1] == 5 and i + 2 < len(codes):
                    col = color256(codes[i + 2])
                    i += 2
                elif codes[i + 1] == 2 and i + 4 < len(codes):
                    col = "#%02x%02x%02x" % tuple(codes[i + 2:i + 5])
                    i += 4
                else:
                    col = None
                if c == 38:
                    self.fg = col
                else:
                    self.bg = col
            i += 1

    def css(self):
        fg, bg = self.fg or FG, self.bg
        if self.reverse:
            fg, bg = bg or BG, fg
        out = ["color:" + fg]
        if bg:
            out.append("background:" + bg)
        if self.bold:
            out.append("font-weight:700")
        if self.dim:
            out.append("opacity:.6")
        if self.italic:
            out.append("font-style:italic")
        if self.underline:
            out.append("text-decoration:underline")
        return ";".join(out)


SGR = re.compile(r"\x1b\[([0-9;]*)m")


def render_line(line, style):
    parts = []
    pos = 0
    for m in list(SGR.finditer(line)) + [None]:
        text = line[pos:m.start()] if m else line[pos:]
        if text:
            cells = []
            for ch in text:
                if unicodedata.east_asian_width(ch) in ("W", "F"):
                    cells.append('<span class="w">%s</span>' % html.escape(ch))
                else:
                    cells.append(html.escape(ch))
            parts.append('<span style="%s">%s</span>' % (style.css(), "".join(cells)))
        if m:
            style.apply(m.group(1))
            pos = m.end()
    return "".join(parts)


def main():
    with open(sys.argv[1], encoding="utf-8") as f:
        lines = f.read().rstrip("\n").split("\n")
    style = Style()
    body = "\n".join(render_line(l, style) for l in lines)
    print("""<!doctype html>
<html><head><meta charset="utf-8">
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;700&family=Noto+Color+Emoji&display=block">
<style>
html, body { margin: 0; background: %(bg)s; }
pre {
  margin: 0; padding: %(pad)dpx;
  font: 15px/%(ch)dpx "JetBrains Mono", "Liberation Mono", monospace;
  color: %(fg)s; white-space: pre;
  font-variant-ligatures: none;
}
.w {
  display: inline-block; width: %(w2)dpx; text-align: center;
  font-family: "Noto Color Emoji", sans-serif; font-size: 14px;
  line-height: %(ch)dpx; vertical-align: top;
}
</style></head>
<body><pre>%(body)s</pre></body></html>""" % {
        "bg": BG, "fg": FG, "pad": PAD, "ch": CELL_H, "w2": 2 * CELL_W, "body": body})


if __name__ == "__main__":
    main()
