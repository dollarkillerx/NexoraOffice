"""Generates the refined concept-C logo files and a contact sheet.

Geometry sits on a 16x16 grid of 64-unit cells inside a 1024 canvas, so at 16 px
every edge lands on a pixel boundary. All gaps are one cell wide.

Usage, from the repository root:
    python3 assets/brand/logo-concepts/c-refined/generate.py \
        assets/brand/logo-concepts/c-refined assets/brand/logo-concepts

The second argument is the directory holding Codex's original concept-c-mark.svg,
which the contact sheet shows as "before". Render the sheet to PNG on macOS with:
    qlmanage -t -s 1600 -o <dir> <dir>/contact-sheet.svg
"""
import pathlib, re, sys

out = pathlib.Path(sys.argv[1])
codex = pathlib.Path(sys.argv[2])

INK, PAPER = "#0b0b0b", "#ffffff"
ACCENTS = {"cobalt": "#2F6BEB", "violet": "#7C5CE0", "vermilion": "#CC4415"}

PAGE = '<polygon points="192,192 384,192 448,256 448,832 192,832"/>'
CELLS = "".join(
    f'<rect x="{x}" y="{y}" width="128" height="128"/>' for y in (192, 384) for x in (512, 704)
)
SLIDE = '<rect x="512" y="576" width="320" height="256"/>'


def lum(h):
    r, g, b = (int(h[i : i + 2], 16) / 255 for i in (1, 3, 5))
    f = lambda c: c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)


def contrast(a, b):
    hi, lo = sorted((lum(a), lum(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


def mark(body, slide):
    return f'<g fill="{body}">{PAGE}{CELLS}</g><g fill="{slide}">{SLIDE}</g>'


def svg(title, inner, w=1024, h=1024):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" version="1.1" width="{w}" height="{h}" '
        f'viewBox="0 0 {w} {h}">\n  <title>{title}</title>\n  {inner}\n</svg>\n'
    )


def icons(accent):
    return {
        "dark": f'<rect width="1024" height="1024" rx="224" fill="{INK}"/>' + mark("#f7f7f6", accent),
        "light": f'<rect x="4" y="4" width="1016" height="1016" rx="220" fill="{PAPER}" stroke="#d9d9d9" stroke-width="8"/>'
        + mark(INK, accent),
        "accent": f'<rect width="1024" height="1024" rx="224" fill="{accent}"/>' + mark(PAPER, PAPER),
    }


files = {"c-mark.svg": svg("Aster Office mark", mark(INK, INK))}
for name, accent in ACCENTS.items():
    for kind, inner in icons(accent).items():
        files[f"c-{name}-{kind}.svg"] = svg(f"Aster Office icon, {name}, {kind}", inner)
    lockup = (
        f'<g transform="translate(40,40) scale({320/1024})">{mark(INK, accent)}</g>'
        f'<text x="420" y="252" font-family="Inter, \'Helvetica Neue\', Helvetica, Arial, sans-serif" '
        f'font-size="150" font-weight="600" letter-spacing="-3" fill="{INK}">Aster Office</text>'
    )
    files[f"c-{name}-lockup.svg"] = svg(f"Aster Office lockup, {name}", lockup, 1400, 400)
for filename, text in files.items():
    (out / filename).write_text(text, encoding="utf-8")

# ---- contact sheet -------------------------------------------------------
def inner_of(path):
    s = path.read_text(encoding="utf-8")
    s = re.sub(r"<title>.*?</title>", "", s, flags=re.S)
    return re.search(r"<svg[^>]*>(.*)</svg>", s, flags=re.S).group(1)


S = 1600
font = 'font-family="Helvetica, Arial, sans-serif"'
sheet = [f'<rect width="{S}" height="{S}" fill="#f7f7f6"/>']
put = lambda x, y, size, inner: sheet.append(
    f'<g transform="translate({x},{y}) scale({size/1024})">{inner}</g>'
)
label = lambda x, y, text, size=15, weight=400, fill="#555": sheet.append(
    f'<text x="{x}" y="{y}" {font} font-size="{size}" font-weight="{weight}" fill="{fill}">{text}</text>'
)

label(40, 56, "Concept C refined", 28, 600, INK)
label(40, 96, "Before: Codex original", 15)
sheet.append('<rect x="40" y="110" width="220" height="220" fill="#fff" stroke="#d9d9d9"/>')
put(40, 110, 220, inner_of(codex / "concept-c-mark.svg"))
label(300, 96, "After: right edges aligned, every edge on a 16-px grid", 15)
sheet.append('<rect x="300" y="110" width="220" height="220" fill="#fff" stroke="#d9d9d9"/>')
put(300, 110, 220, mark(INK, INK))
# pixel grid overlay on the "after" mark
for i in range(17):
    p = 300 + i * 220 / 16
    q = 110 + i * 220 / 16
    sheet.append(f'<line x1="{p}" y1="110" x2="{p}" y2="330" stroke="#2F6BEB" stroke-opacity="0.18"/>')
    sheet.append(f'<line x1="300" y1="{q}" x2="520" y2="{q}" stroke="#2F6BEB" stroke-opacity="0.18"/>')
label(580, 150, "Mark, single colour", 15)
for size, dx in ((128, 0), (64, 150), (32, 236), (16, 290)):
    put(580 + dx, 170 + (128 - size) / 2, size, mark(INK, INK))

y = 400
for name, accent in ACCENTS.items():
    ic = icons(accent)
    label(40, y, f"{name}  {accent}", 20, 600, INK)
    label(
        300,
        y,
        f"contrast on white {contrast(accent, PAPER):.2f} : 1   ·   on near-black {contrast(accent, INK):.2f} : 1",
        15,
    )
    x = 40
    for kind in ("dark", "light", "accent"):
        put(x, y + 20, 240, ic[kind])
        x += 264
    for kind, dy in (("dark", 20), ("light", 110), ("accent", 200)):
        for size, dx in ((64, 0), (32, 84), (16, 132)):
            put(850 + dx, y + dy + (64 - size) / 2, size, ic[kind])
    sheet.append(f'<rect x="1040" y="{y+20}" width="520" height="150" fill="#fff" stroke="#d9d9d9"/>')
    sheet.append(
        f'<g transform="translate(1050,{y+22}) scale({500/1400})">{inner_of(out / f"c-{name}-lockup.svg")}</g>'
    )
    # the accent as it would appear in the interface
    sheet.append(f'<rect x="1040" y="{y+190}" width="150" height="44" rx="9" fill="{accent}"/>')
    label(1070, y + 218, "Primary", 16, 600, PAPER)
    sheet.append(f'<rect x="1210" y="{y+190}" width="150" height="44" rx="9" fill="#fff" stroke="{accent}" stroke-width="2"/>')
    label(1240, y + 218, "Selected", 16, 600, accent)
    label(1390, y + 218, "Link text", 16, 400, accent)
    y += 390

(out / "contact-sheet.svg").write_text(svg("Aster Office, concept C refined", "\n".join(sheet), S, S), encoding="utf-8")
for name, accent in ACCENTS.items():
    print(f"{name:<10}{accent}  on white {contrast(accent, PAPER):.2f}  on ink {contrast(accent, INK):.2f}")
