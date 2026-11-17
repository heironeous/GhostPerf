"""Generates GhostPerf's icon and logo assets from one shared geometry.

Outputs (all under assets/):
  icon/<name>.svg, icon/<name>-small.svg    source SVGs (>=48px / <=32px variants)
  icon/png/<name>-<size>.png                rasterized sizes
  icon/<name>.ico                           multi-size Windows icon
  logo/ghostperf-logo-{dark,light}.{svg,png} icon + wordmark

where <name> is one of:
  ghostperf            tray (idle) — also the main brand icon
  ghostperf-recording  tray while a session is recording (red dot)
  ghostperf-agent      agent .exe (graphite background, gear badge)
  ghostperf-ui         UI .exe (violet background, magnifier badge)

Concept: the ghost (an intermittent "ghost" issue) has a bar-chart tail; the
longest bar, in the accent color, is the spike that got caught. The exe icons
share the ghost and differ by background color (readable even at 16px) plus a
corner badge.

Requires: pip install resvg-py Pillow
Run:      python assets/build_icons.py
"""

from dataclasses import dataclass
from io import BytesIO
from pathlib import Path

import resvg_py
from PIL import Image

ROOT = Path(__file__).resolve().parent

# Palette
GHOST = "#F3F5FF"
GHOST_SHADE = "#C5CBF2"
ACCENT = "#3DE0B5"
RECORDING = "#FF3B52"
AMBER = "#FFC247"
TEXT_ON_DARK = "#F3F5FF"
TEXT_ON_LIGHT = "#13152E"
ACCENT_ON_LIGHT = "#12A884"  # darker mint, keeps contrast on white

ICON_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
PNG_SIZES = [16, 24, 32, 48, 64, 128, 256, 512]
SMALL_MAX = 32  # sizes at or below this use the "small" geometry


@dataclass(frozen=True)
class Style:
    bg_top: str
    bg_bottom: str  # also used for the eyes and the badge's cut-out ring
    badge: str | None = None  # None, "recording", "gear" or "magnifier"


STYLES = {
    "ghostperf": Style("#2B3070", "#13152E"),
    "ghostperf-recording": Style("#2B3070", "#13152E", "recording"),
    "ghostperf-agent": Style("#4A5263", "#15181F", "gear"),
    "ghostperf-ui": Style("#6A35A0", "#1D1038", "magnifier"),
}


def defs(prefix: str, style: Style) -> str:
    return f"""
  <defs>
    <linearGradient id="{prefix}bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{style.bg_top}"/>
      <stop offset="1" stop-color="{style.bg_bottom}"/>
    </linearGradient>
    <linearGradient id="{prefix}ghost" x1="0" y1="40" x2="0" y2="220" gradientUnits="userSpaceOnUse">
      <stop offset="0.45" stop-color="{GHOST}"/>
      <stop offset="1" stop-color="{GHOST_SHADE}"/>
    </linearGradient>
  </defs>"""


# Two geometries in the same 256x256 space. "full" is the detailed mark for
# 48px and up; "small" is simplified for 16-32px (bigger mark, 3 legs, larger
# eyes and gaps) so it survives being only a handful of pixels wide.
GEOMETRY = {
    "full": dict(
        left=56, right=200, top=38, hem=162, gap=11, leg_rx=8,
        # Bottom y of each leg: a bar chart hanging from the ghost, rising
        # left to right; the last (longest) bar is the accent spike.
        bottoms=[190, 204, 196, 226],
        eye_dx=24, eye_y=106, eye_rx=12, eye_ry=16,
    ),
    "small": dict(
        left=42, right=214, top=30, hem=170, gap=20, leg_rx=6,
        bottoms=[204, 192, 232],
        eye_dx=30, eye_y=108, eye_rx=17, eye_ry=23,
    ),
}

# Corner badge: a colored disk in the top-right, separated from the ghost by
# a ring in the background color.
BADGE_X, BADGE_Y, BADGE_RING, BADGE_R = 212, 44, 44, 34


def badge_svg(kind: str | None, style: Style, glyph: bool = True) -> str:
    """`glyph=False` draws just the colored disk: at 24px and below the gear
    and magnifier are a few pixels wide and turn to mush, while a plain dot
    in the badge color still reads clearly."""
    if kind is None:
        return ""
    x, y, dark = BADGE_X, BADGE_Y, style.bg_bottom
    ring = f'<circle cx="{x}" cy="{y}" r="{BADGE_RING}" fill="{dark}"/>'
    if kind == "recording":
        return ring + f'<circle cx="{x}" cy="{y}" r="{BADGE_R}" fill="{RECORDING}"/>'
    if not glyph:
        color = {"gear": ACCENT, "magnifier": AMBER}[kind]
        return ring + f'<circle cx="{x}" cy="{y}" r="{BADGE_R}" fill="{color}"/>'
    if kind == "gear":
        # 8 teeth (4 rotated bars), a hub, and a hole in the disk color.
        teeth = "".join(
            f'<rect x="{x - 6}" y="{y - 25}" width="12" height="50" rx="2" '
            f'fill="{dark}" transform="rotate({a} {x} {y})"/>'
            for a in (0, 45, 90, 135)
        )
        return (
            ring
            + f'<circle cx="{x}" cy="{y}" r="{BADGE_R}" fill="{ACCENT}"/>'
            + teeth
            + f'<circle cx="{x}" cy="{y}" r="18" fill="{dark}"/>'
            + f'<circle cx="{x}" cy="{y}" r="7" fill="{ACCENT}"/>'
        )
    if kind == "magnifier":
        return (
            ring
            + f'<circle cx="{x}" cy="{y}" r="{BADGE_R}" fill="{AMBER}"/>'
            + f'<circle cx="{x - 5}" cy="{y - 5}" r="13" fill="none" stroke="{dark}" stroke-width="8"/>'
            + f'<line x1="{x + 5}" y1="{y + 5}" x2="{x + 17}" y2="{y + 17}" stroke="{dark}" '
            f'stroke-width="9" stroke-linecap="round"/>'
        )
    raise ValueError(f"unknown badge {kind!r}")


def ghost_mark(prefix: str, style: Style, size: str = "full", glyph: bool = True) -> str:
    """The icon contents in a 256x256 coordinate space."""
    g = GEOMETRY[size]
    left, right, hem, gap = g["left"], g["right"], g["hem"], g["gap"]
    radius = (right - left) / 2
    cx = (left + right) / 2
    shoulder = g["top"] + radius
    n = len(g["bottoms"])
    bar_w = (right - left - (n - 1) * gap) / n
    legs = []
    for i, bottom in enumerate(g["bottoms"]):
        x = left + i * (bar_w + gap)
        fill = ACCENT if i == n - 1 else f"url(#{prefix}ghost)"
        # Legs start under the body so only the part below the hem shows.
        legs.append(
            f'<rect x="{x:.2f}" y="{hem - 30}" width="{bar_w:.2f}" '
            f'height="{bottom - hem + 30}" rx="{g["leg_rx"]}" fill="{fill}"/>'
        )
    body = (
        f'<path d="M{left},{hem} V{shoulder} A{radius},{radius} 0 0 1 '
        f'{right},{shoulder} V{hem} Z" fill="url(#{prefix}ghost)"/>'
    )
    eyes = "".join(
        f'<ellipse cx="{cx + dx}" cy="{g["eye_y"]}" rx="{g["eye_rx"]}" '
        f'ry="{g["eye_ry"]}" fill="{style.bg_bottom}"/>'
        for dx in (-g["eye_dx"], g["eye_dx"])
    )
    return (
        f'<rect width="256" height="256" rx="56" fill="url(#{prefix}bg)"/>'
        + "".join(legs)
        + body
        + eyes
        + badge_svg(style.badge, style, glyph)
    )


def icon_svg(style: Style, size: str = "full", glyph: bool = True) -> str:
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256">'
        + defs("gp-", style)
        + ghost_mark("gp-", style, size, glyph)
        + "</svg>\n"
    )


GLYPH_MIN = 32  # badge glyphs (gear, magnifier) only at this size and up


def logo_svg(light: bool) -> str:
    style = STYLES["ghostperf"]
    text = TEXT_ON_LIGHT if light else TEXT_ON_DARK
    accent = ACCENT_ON_LIGHT if light else ACCENT
    font = "'Segoe UI Variable Display', 'Segoe UI', 'Inter', 'Helvetica Neue', Arial, sans-serif"
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1040 256" width="1040" height="256">'
        + defs("gp-", style)
        + ghost_mark("gp-", style)
        + f'<text x="296" y="178" font-family="{font}" font-size="148" font-weight="700" '
        f'letter-spacing="-3" fill="{text}">Ghost<tspan fill="{accent}">Perf</tspan></text>'
        + "</svg>\n"
    )


def render(svg: str, width: int) -> Image.Image:
    png = resvg_py.svg_to_bytes(svg_string=svg, width=width)
    return Image.open(BytesIO(bytes(png))).convert("RGBA")


def write_icon_set(name: str, style: Style) -> None:
    icon_dir = ROOT / "icon"
    png_dir = icon_dir / "png"
    png_dir.mkdir(parents=True, exist_ok=True)
    full = icon_svg(style, "full")
    small = icon_svg(style, "small")
    tiny = icon_svg(style, "small", glyph=False)
    (icon_dir / f"{name}.svg").write_text(full, encoding="utf-8")
    (icon_dir / f"{name}-small.svg").write_text(small, encoding="utf-8")

    def frame(px: int) -> Image.Image:
        if px < GLYPH_MIN:
            return render(tiny, px)
        return render(small if px <= SMALL_MAX else full, px)

    for size in PNG_SIZES:
        frame(size).save(png_dir / f"{name}-{size}.png")

    # Each ICO frame is rendered at its native size (not downscaled from 256)
    # so small frames stay crisp.
    frames = [frame(s) for s in ICON_SIZES]
    frames[-1].save(
        icon_dir / f"{name}.ico",
        format="ICO",
        sizes=[(s, s) for s in ICON_SIZES],
        append_images=frames[:-1],
    )


def main() -> None:
    for name, style in STYLES.items():
        write_icon_set(name, style)

    logo_dir = ROOT / "logo"
    logo_dir.mkdir(parents=True, exist_ok=True)
    for variant, light in (("dark", False), ("light", True)):
        svg = logo_svg(light)
        (logo_dir / f"ghostperf-logo-{variant}.svg").write_text(svg, encoding="utf-8")
        render(svg, 2080).save(logo_dir / f"ghostperf-logo-{variant}.png")

    print(f"assets written to {ROOT}")


if __name__ == "__main__":
    main()
