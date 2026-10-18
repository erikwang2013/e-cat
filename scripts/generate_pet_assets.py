#!/usr/bin/env python3
"""Generate the Ecat project pet as pure-vector SVG.

Emits docs/e-cat.svg, docs/pet-expressions/*.svg and the twelve localized
copies under docs/i18n/*/e-cat.svg from one shared design.  No raster
images are embedded; every expression is drawn with vector geometry.

Usage:  python3 scripts/generate-pet-assets.py
"""

import sys
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
PET_DIR = ROOT / "docs" / "pet-expressions"
I18N_DIR = ROOT / "docs" / "i18n"
# ecat-cli 需要自带一份资源才能随 crate 发布；生成器保证两份字节一致。
CLI_ASSET_DIR = ROOT / "ecat-cli" / "assets" / "pet"

FUR = "#E8743B"
FUR_DARK = "#C7552B"
CREAM = "#FFF1DC"
INK = "#3A2E28"
PINK = "#F29A9E"
AMBER = "#F7B93D"
TEAL = "#2F8F8A"
BADGE = "#FFF8EE"

CSS = """
@keyframes breathe { 0%,100% { transform: scaleY(1) translateY(0); } 50% { transform: scaleY(1.012) translateY(-2px); } }
@keyframes tail-sway { 0%,100% { transform: rotate(-2deg); } 50% { transform: rotate(2.5deg); } }
@keyframes blink { 0%,94%,100% { transform: scaleY(0); } 96%,98% { transform: scaleY(1); } }
@keyframes fx-float { 0%,100% { transform: translateY(0) scale(1); opacity: .85; } 50% { transform: translateY(-10px) scale(1.08); opacity: 1; } }
@keyframes fx-fall { 0% { transform: translateY(-6px); opacity: 0; } 25% { opacity: 1; } 100% { transform: translateY(34px); opacity: 0; } }
@keyframes fx-rain { 0% { transform: translateY(0); opacity: 0; } 20% { opacity: .9; } 100% { transform: translateY(42px); opacity: 0; } }
@keyframes fx-confetti { 0% { transform: translateY(-8px) rotate(0deg); opacity: 1; } 100% { transform: translateY(56px) rotate(220deg); opacity: 0; } }
@keyframes fx-pulse { 0%,100% { transform: scale(.9); opacity: .35; } 50% { transform: scale(1.12); opacity: 1; } }
@keyframes fx-rise { 0% { transform: translateY(6px) scale(.85); opacity: 0; } 35% { opacity: .9; } 100% { transform: translateY(-26px) scale(1.1); opacity: 0; } }
@keyframes fx-knock { 0%,100% { transform: rotate(0deg); } 35% { transform: rotate(-16deg); } 55% { transform: rotate(4deg); } }
@keyframes fx-shake { 0%,100% { transform: translateX(0); } 25% { transform: translateX(-2px); } 75% { transform: translateX(2px); } }
@keyframes fx-wiggle { 0%,100% { transform: rotate(-6deg); } 50% { transform: rotate(6deg); } }
@keyframes head-tilt { 0%,100% { transform: rotate(-4deg); } 50% { transform: rotate(5deg); } }
.breathe { transform-box: view-box; transform-origin: 240px 402px; animation: breathe 4.2s ease-in-out infinite; }
.tail-sway { transform-box: view-box; transform-origin: 318px 372px; animation: tail-sway 4.6s ease-in-out infinite; }
.blink { transform-box: view-box; transform-origin: 240px 172px; animation: blink 5.6s infinite; }
.head-tilt { transform-box: view-box; transform-origin: 240px 250px; animation: head-tilt 5s ease-in-out infinite; }
.fx-float { transform-box: fill-box; transform-origin: center; animation: fx-float 2.6s ease-in-out infinite; }
.fx-fall { transform-box: fill-box; transform-origin: center; animation: fx-fall 1.9s ease-in infinite; }
.fx-rain { transform-box: fill-box; transform-origin: center; animation: fx-rain 1.5s linear infinite; }
.fx-confetti { transform-box: fill-box; transform-origin: center; animation: fx-confetti 2.6s linear infinite; }
.fx-pulse { transform-box: fill-box; transform-origin: center; animation: fx-pulse 1.5s ease-in-out infinite; }
.fx-rise { transform-box: fill-box; transform-origin: center; animation: fx-rise 2.2s ease-out infinite; }
.fx-knock { transform-box: view-box; transform-origin: 282px 330px; animation: fx-knock .6s ease-in-out infinite; }
.fx-shake { transform-box: fill-box; transform-origin: center; animation: fx-shake .22s linear infinite; }
.fx-wiggle { transform-box: fill-box; transform-origin: top center; animation: fx-wiggle 1.6s ease-in-out infinite; }
@media (prefers-reduced-motion: reduce) {
  .breathe, .tail-sway, .blink, .head-tilt, .fx-float, .fx-fall, .fx-rain,
  .fx-confetti, .fx-pulse, .fx-rise, .fx-knock, .fx-shake, .fx-wiggle { animation: none; }
}
"""


def hexagon(cx, cy, r):
    """Pointy-top regular hexagon as a polygon point string."""
    import math
    points = []
    for i in range(6):
        angle = math.radians(60 * i - 90)
        points.append(f"{cx + r * math.cos(angle):.2f},{cy + r * math.sin(angle):.2f}")
    return " ".join(points)


def hand(cx, cy, r=14):
    """Cream paw with three toe lines."""
    return (
        f'<ellipse cx="{cx}" cy="{cy}" rx="{r}" ry="{r - 2}" fill="{CREAM}" '
        f'stroke="{INK}" stroke-width="4"/>'
        f'<path d="M {cx - r * .45:.1f} {cy - r * .35:.1f} L {cx - r * .45:.1f} {cy + r * .45:.1f} '
        f'M {cx + r * .45:.1f} {cy - r * .35:.1f} L {cx + r * .45:.1f} {cy + r * .45:.1f}" '
        f'stroke="{INK}" stroke-width="2.4" stroke-linecap="round" opacity=".55"/>'
    )


def eye_open(cx, cy, scale=1.0):
    rx, ry = 20 * scale, 23 * scale
    return (
        f'<g>'
        f'<ellipse cx="{cx}" cy="{cy}" rx="{rx:.1f}" ry="{ry:.1f}" fill="{AMBER}" stroke="{INK}" stroke-width="4.5"/>'
        f'<ellipse cx="{cx}" cy="{cy + 2}" rx="{7.5 * scale:.1f}" ry="{12.5 * scale:.1f}" fill="{INK}"/>'
        f'<circle cx="{cx + 6 * scale:.1f}" cy="{cy - 9 * scale:.1f}" r="{5.5 * scale:.1f}" fill="#FFFFFF"/>'
        f'<circle cx="{cx - 5 * scale:.1f}" cy="{cy + 7 * scale:.1f}" r="{2.4 * scale:.1f}" fill="#FFFFFF" opacity=".8"/>'
        f'</g>'
    )


def eye_happy(cx, cy, flip=False):
    sign = -1 if flip else 1
    return (
        f'<path d="M {cx - 18} {cy + 6} Q {cx} {cy - 14 * sign} {cx + 18} {cy + 6}" '
        f'fill="none" stroke="{INK}" stroke-width="6" stroke-linecap="round"/>'
    )


def eye_lens(cx, cy):
    return (
        f'<path d="M {cx - 19} {cy} Q {cx} {cy - 13} {cx + 19} {cy} Q {cx} {cy + 13} {cx - 19} {cy} Z" '
        f'fill="{AMBER}" stroke="{INK}" stroke-width="4"/>'
        f'<ellipse cx="{cx}" cy="{cy + 1}" rx="5" ry="9" fill="{INK}"/>'
        f'<circle cx="{cx + 5}" cy="{cy - 4}" r="3" fill="#FFFFFF"/>'
    )


def eye_scared(cx, cy):
    return (
        f'<circle cx="{cx}" cy="{cy}" r="24" fill="#FFFFFF" stroke="{INK}" stroke-width="4.5"/>'
        f'<circle cx="{cx}" cy="{cy + 2}" r="6" fill="{INK}"/>'
        f'<circle cx="{cx + 7}" cy="{cy - 7}" r="2.5" fill="{FUR}" opacity=".5"/>'
    )


def eye_pleading(cx, cy):
    return (
        f'<ellipse cx="{cx}" cy="{cy}" rx="22" ry="25" fill="{AMBER}" stroke="{INK}" stroke-width="4.5"/>'
        f'<ellipse cx="{cx}" cy="{cy + 2}" rx="9" ry="16" fill="{INK}"/>'
        f'<circle cx="{cx + 7}" cy="{cy - 10}" r="6" fill="#FFFFFF"/>'
        f'<circle cx="{cx - 6}" cy="{cy + 8}" r="3" fill="#FFFFFF" opacity=".85"/>'
        f'<circle cx="{cx + 4}" cy="{cy - 2}" r="2" fill="#FFFFFF" opacity=".7"/>'
    )


def lids(cx, cy, top):
    """Fur-coloured upper lid; top is the y where the lid line sits."""
    return (
        f'<path d="M {cx - 21} {cy - 26} L {cx + 21} {cy - 26} L {cx + 21} {top} '
        f'Q {cx} {top - 5} {cx - 21} {top} Z" fill="{FUR}"/>'
        f'<path d="M {cx - 20} {top} Q {cx} {top - 5} {cx + 20} {top}" '
        f'fill="none" stroke="{INK}" stroke-width="3" stroke-linecap="round" opacity=".8"/>'
    )


def brow(x1, y1, x2, y2):
    return (
        f'<path d="M {x1} {y1} L {x2} {y2}" fill="none" stroke="{INK}" '
        f'stroke-width="6" stroke-linecap="round"/>'
    )


def mouth(kind):
    if kind == "smile":
        return (
            f'<path d="M 240 207 L 240 213 M 240 213 Q 231 223 225 214 M 240 213 Q 249 223 255 214" '
            f'fill="none" stroke="{INK}" stroke-width="3.5" stroke-linecap="round"/>'
        )
    if kind == "open-smile":
        return (
            f'<path d="M 221 209 Q 240 240 259 209 Q 240 217 221 209 Z" fill="#7A3B36" stroke="{INK}" stroke-width="3"/>'
            f'<ellipse cx="240" cy="228" rx="9" ry="6" fill="{PINK}"/>'
        )
    if kind == "frown":
        return f'<path d="M 228 221 Q 240 208 252 221" fill="none" stroke="{INK}" stroke-width="3.5" stroke-linecap="round"/>'
    if kind == "wavy":
        return (
            f'<path d="M 226 217 Q 232 211 238 217 Q 244 223 250 217 Q 254 213 258 216" '
            f'fill="none" stroke="{INK}" stroke-width="3.3" stroke-linecap="round"/>'
        )
    if kind == "scream":
        return (
            f'<ellipse cx="240" cy="219" rx="14" ry="18" fill="#7A3B36" stroke="{INK}" stroke-width="3"/>'
            f'<ellipse cx="240" cy="230" rx="8" ry="5" fill="{PINK}"/>'
        )
    if kind == "gritted":
        return (
            f'<rect x="224" y="209" width="32" height="17" rx="7" fill="#7A3B36" stroke="{INK}" stroke-width="3"/>'
            f'<rect x="228" y="212" width="24" height="5" rx="2.5" fill="#FFFFFF"/>'
            f'<rect x="228" y="218" width="24" height="5" rx="2.5" fill="#FFFFFF"/>'
            f'<path d="M 236 212 L 236 223 M 244 212 L 244 223" stroke="{INK}" stroke-width="2" opacity=".55"/>'
        )
    if kind == "tongue":
        return (
            f'<path d="M 240 207 L 240 213 M 240 213 Q 231 223 225 214 M 240 213 Q 249 223 255 214" '
            f'fill="none" stroke="{INK}" stroke-width="3.5" stroke-linecap="round"/>'
            f'<path d="M 233 217 Q 240 240 247 217 Q 240 224 233 217 Z" fill="{PINK}" stroke="{INK}" stroke-width="2.6"/>'
            f'<path d="M 240 224 L 240 232" stroke="{INK}" stroke-width="1.8" opacity=".5"/>'
        )
    if kind == "small-o":
        return f'<ellipse cx="240" cy="218" rx="7" ry="9" fill="#7A3B36" stroke="{INK}" stroke-width="2.6"/>'
    if kind == "neutral":
        return f'<path d="M 229 215 L 251 215" fill="none" stroke="{INK}" stroke-width="3.4" stroke-linecap="round"/>'
    return f'<path d="M 229 217 Q 240 214 251 217" fill="none" stroke="{INK}" stroke-width="3.4" stroke-linecap="round"/>'


def sparkle(x, y, s, cls="fx-float", delay=0.0, color="#FFD166"):
    return (
        f'<path class="{cls}" style="animation-delay:{delay}s" d="M {x} {y - s} '
        f'Q {x + s * .18} {y - s * .18} {x + s} {y} Q {x + s * .18} {y + s * .18} {x} {y + s} '
        f'Q {x - s * .18} {y + s * .18} {x - s} {y} Q {x - s * .18} {y - s * .18} {x} {y - s} Z" '
        f'fill="{color}"/>'
    )


def heart(x, y, s, cls="fx-float", delay=0.0):
    return (
        f'<path class="{cls}" style="animation-delay:{delay}s" d="M {x} {y + s * .38} '
        f'C {x - s * .75} {y - s * .1} {x - s * .42} {y - s * .72} {x} {y - s * .28} '
        f'C {x + s * .42} {y - s * .72} {x + s * .75} {y - s * .1} {x} {y + s * .38} Z" '
        f'fill="#F26D7D"/>'
    )


def drop(x, y, s, cls="fx-fall", delay=0.0, color="#7EC8F2"):
    return (
        f'<path class="{cls}" style="animation-delay:{delay}s" d="M {x} {y - s} '
        f'Q {x + s * .72} {y + s * .25} {x} {y + s} Q {x - s * .72} {y + s * .25} {x} {y - s} Z" '
        f'fill="{color}" opacity=".9"/>'
    )


def sparkles(positions, color="#FFD166"):
    return "".join(sparkle(x, y, s, delay=d, color=color) for x, y, s, d in positions)


def build_face(eyes="open", brows="", mouth_kind="smile", blush=False, extra_face=""):
    left, right = 202, 278
    if eyes == "open":
        eye_svg = eye_open(left, 172) + eye_open(right, 172)
    elif eyes == "happy":
        eye_svg = eye_happy(left, 172) + eye_happy(right, 172)
    elif eyes == "wink":
        eye_svg = eye_happy(left, 172) + eye_open(right, 172)
    elif eyes == "lens":
        eye_svg = eye_lens(left, 172) + eye_lens(right, 172)
    elif eyes == "scared":
        eye_svg = eye_scared(left, 170) + eye_scared(right, 170)
    elif eyes == "pleading":
        eye_svg = eye_pleading(left, 172) + eye_pleading(right, 172)
    elif eyes == "half":
        eye_svg = eye_open(left, 172) + eye_open(right, 172) + lids(left, 172, 166) + lids(right, 172, 166)
    elif eyes == "sad":
        eye_svg = eye_open(left, 176) + eye_open(right, 176) + lids(left, 176, 162) + lids(right, 176, 162)
    elif eyes == "covered":
        eye_svg = ""
    else:
        eye_svg = eye_open(left, 172) + eye_open(right, 172)

    blink = ""
    if eyes in ("open", "wink"):
        blink = (
            f'<g class="blink" style="transform:scaleY(0);transform-box:view-box;transform-origin:240px 172px">'
            f'<rect x="180" y="147" width="44" height="48" rx="22" fill="{FUR}"/>'
            f'<rect x="256" y="147" width="44" height="48" rx="22" fill="{FUR}"/>'
            f'</g>'
        )

    blush_svg = ""
    if blush:
        blush_svg = (
            f'<ellipse cx="186" cy="211" rx="15" ry="8" fill="{PINK}" opacity=".4"/>'
            f'<ellipse cx="294" cy="211" rx="15" ry="8" fill="{PINK}" opacity=".4"/>'
        )

    return f'<g class="face">{eye_svg}{blink}{brows}{blush_svg}{extra_face}{mouth(mouth_kind)}</g>'


EAR_L = f'<path d="M 168 140 L 152 54 Q 150 45 159 50 L 236 92 Z" fill="{FUR}" stroke="{INK}" stroke-width="5" stroke-linejoin="round"/>'
EAR_R = f'<path d="M 312 140 L 328 54 Q 330 45 321 50 L 244 92 Z" fill="{FUR}" stroke="{INK}" stroke-width="5" stroke-linejoin="round"/>'
EAR_IN_L = f'<path d="M 170 126 L 160 68 Q 159 63 165 66 L 220 94 Z" fill="{PINK}"/>'
EAR_IN_R = f'<path d="M 310 126 L 320 68 Q 321 63 315 66 L 260 94 Z" fill="{PINK}"/>'

HEAD = f'''
<g class="head">
  {EAR_L}{EAR_R}{EAR_IN_L}{EAR_IN_R}
  <path d="M 240 84 C 292 84 332 114 334 160 C 336 186 328 208 310 224
           C 294 238 272 248 240 248 C 208 248 186 238 170 224
           C 152 208 144 186 146 160 C 148 114 188 84 240 84 Z"
        fill="{FUR}" stroke="{INK}" stroke-width="5"/>
  <path d="M 222 92 C 219 103 219 112 222 121 M 240 88 C 240 100 240 111 240 123
           M 258 92 C 261 103 261 112 258 121" fill="none" stroke="{INK}"
        stroke-width="7" stroke-linecap="round"/>
  <path d="M 152 186 L 172 190 M 150 202 L 170 204 M 328 186 L 308 190 M 330 202 L 310 204"
        fill="none" stroke="{INK}" stroke-width="5" stroke-linecap="round"/>
  <circle cx="221" cy="209" r="21" fill="{CREAM}"/>
  <circle cx="259" cy="209" r="21" fill="{CREAM}"/>
  <ellipse cx="240" cy="228" rx="27" ry="15" fill="{CREAM}"/>
  {{face}}
  <path d="M 232 196 Q 240 190 248 196 Q 245 205 240 207 Q 235 205 232 196 Z"
        fill="{PINK}" stroke="{INK}" stroke-width="3"/>
  <path d="M 204 206 L 170 198 M 202 214 L 168 214 M 204 222 L 172 230
           M 276 206 L 310 198 M 278 214 L 312 214 M 276 222 L 308 230"
        fill="none" stroke="{INK}" stroke-width="2.5" stroke-linecap="round" opacity=".5"/>
</g>'''

BODY = f'''
<g class="body">
  <ellipse cx="240" cy="407" rx="114" ry="11" fill="{INK}" opacity=".07"/>
  <path d="M 240 230 C 205 230 182 250 175 284 C 166 324 152 362 156 386
           Q 160 400 178 400 L 302 400 Q 320 400 324 386
           C 328 362 314 324 305 284 C 298 250 275 230 240 230 Z"
        fill="{FUR}" stroke="{INK}" stroke-width="5"/>
  <path d="M 190 330 Q 206 360 190 392 M 290 330 Q 274 360 290 392"
        fill="none" stroke="{INK}" stroke-width="3" stroke-linecap="round" opacity=".22"/>
  <path d="M 240 258 C 220 258 208 272 206 296 C 204 322 202 348 208 368
           Q 214 378 222 370 Q 231 380 240 372 Q 249 380 258 370
           Q 266 378 272 368 C 278 348 276 322 274 296 C 272 272 260 258 240 258 Z"
        fill="{CREAM}" stroke="{INK}" stroke-width="4"/>
  {{legs}}
</g>'''

LEFT_LEG = f'''
<path d="M 200 318 C 195 344 195 374 197 389 Q 199 400 214 400 Q 229 400 231 389
         C 233 374 233 344 228 318 Z" fill="{FUR}" stroke="{INK}" stroke-width="5"/>
<path d="M 197 382 Q 197 400 214 400 Q 231 400 231 382 Q 222 388 214 388 Q 206 388 197 382 Z"
      fill="{CREAM}" stroke="{INK}" stroke-width="4"/>
<path d="M 208 391 L 208 398 M 220 391 L 220 398"
      stroke="{INK}" stroke-width="2.4" stroke-linecap="round" opacity=".5"/>'''

RIGHT_LEG = f'''
<path d="M 280 318 C 285 344 285 374 283 389 Q 281 400 266 400 Q 251 400 249 389
         C 247 374 247 344 252 318 Z" fill="{FUR}" stroke="{INK}" stroke-width="5"/>
<path d="M 283 382 Q 283 400 266 400 Q 249 400 249 382 Q 258 388 266 388 Q 274 388 283 382 Z"
      fill="{CREAM}" stroke="{INK}" stroke-width="4"/>
<path d="M 260 391 L 260 398 M 272 391 L 272 398"
      stroke="{INK}" stroke-width="2.4" stroke-linecap="round" opacity=".5"/>'''

LEGS = LEFT_LEG + RIGHT_LEG

TAIL = f'''
<g class="tail-sway">
  <path d="M 316 376 C 368 392 416 360 416 314 C 416 284 402 264 384 256"
        fill="none" stroke="{INK}" stroke-width="34" stroke-linecap="round"/>
  <path d="M 316 376 C 368 392 416 360 416 314 C 416 284 402 264 384 256"
        fill="none" stroke="{FUR}" stroke-width="26" stroke-linecap="round"/>
  <circle cx="384" cy="256" r="13" fill="{CREAM}"/>
  <path d="M 352 369 L 355 390 M 384 356 L 398 373 M 400 335 L 421 344 M 400 286 L 420 289"
        fill="none" stroke="{INK}" stroke-width="7" stroke-linecap="round" opacity=".85"/>
</g>'''

COLLAR = f'''
<path d="M 180 250 Q 240 274 300 250 L 298 268 Q 240 292 182 268 Z"
      fill="{TEAL}" stroke="{INK}" stroke-width="4" stroke-linejoin="round"/>
<circle cx="240" cy="277" r="4.5" fill="none" stroke="{INK}" stroke-width="3"/>
<polygon points="{hexagon(240, 298, 18)}" fill="{CREAM}" stroke="{TEAL}" stroke-width="4" stroke-linejoin="round"/>
<ellipse cx="240" cy="304" rx="5.6" ry="4.6" fill="{TEAL}"/>
<circle cx="233" cy="296" r="2.5" fill="{TEAL}"/>
<circle cx="240" cy="293" r="2.7" fill="{TEAL}"/>
<circle cx="247" cy="296" r="2.5" fill="{TEAL}"/>'''

NODES = f'''
<g class="nodes">
  <polygon points="{hexagon(398, 122, 11)}" fill="none" stroke="{TEAL}" stroke-width="3" stroke-linejoin="round"/>
  <polygon points="{hexagon(424, 146, 6)}" fill="{FUR}" opacity=".85"/>
  <path d="M 406 131 L 419 140" stroke="{TEAL}" stroke-width="2" stroke-dasharray="3 4" opacity=".6"/>
</g>'''

BADGE_SVG = f'''
<polygon points="444,240 342,52 138,52 36,240 138,428 342,428" fill="{BADGE}"
         stroke="#EAD9C6" stroke-width="3" stroke-linejoin="round"/>
<polygon points="416,240 326,84 154,84 64,240 154,396 326,396" fill="none"
         stroke="{FUR}" stroke-width="2" stroke-dasharray="2 10" opacity=".22"/>'''


def document(title, desc, face_svg="", effects_svg="", legs_svg=None, head_class="head",
             extra_css="", pet_class="breathe", nodes=True):
    legs = LEGS if legs_svg is None else legs_svg
    head = HEAD.replace("{face}", face_svg)
    if head_class != "head":
        head = head.replace('<g class="head">', f'<g class="{head_class}">', 1)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480" width="480" height="480" role="img" aria-label="{title}">
<title>{title}</title>
<desc>{desc}</desc>
<style>{CSS}{extra_css}</style>
<g class="badge">{BADGE_SVG}{NODES if nodes else ""}</g>
<g class="pet {pet_class}" transform="translate(0,6)">
  {TAIL}
  {BODY.replace("{legs}", legs)}
  {COLLAR}
  {head}
  {effects_svg}
</g>
<text x="470" y="474" text-anchor="end" font-size="11" fill="{INK}" opacity=".28" font-family="sans-serif">erik.xyz</text>
</svg>
'''


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    print(f"wrote {path.relative_to(ROOT)} ({len(content)} bytes)")


def main():
    from pet_expressions import EXPRESSIONS

    main_svg = document(
        "Ecat 项目宠物",
        "一只猫 · Rust 微服务框架吉祥物，纯矢量动态形象",
        face_svg=build_face(eyes="open", mouth_kind="smile"),
        effects_svg=sparkles([(378, 196, 8, 0.4), (92, 208, 6, 1.1)]),
    )
    write(ROOT / "docs" / "e-cat.svg", main_svg)
    write(CLI_ASSET_DIR / "ecat.svg", main_svg)
    write(CLI_ASSET_DIR / "ASSETS-LICENSE.md", (ROOT / "ASSETS-LICENSE.md").read_text(encoding="utf-8"))
    for local in sorted(I18N_DIR.glob("*/e-cat.svg")):
        write(local, main_svg)

    for name, spec in EXPRESSIONS.items():
        svg = document(
            f"E-Cat · {spec['cn']} {spec['en']}",
            f"E-Cat 的 {spec['cn']} 表情形态 · pure vector expression",
            face_svg=build_face(**spec["face"]),
            effects_svg=spec.get("fx", ""),
            legs_svg=spec.get("legs"),
            head_class=spec.get("head_class", "head"),
            extra_css=spec.get("css", ""),
            nodes=True,
        )
        write(PET_DIR / f"{name}.svg", svg)
        write(CLI_ASSET_DIR / f"{name}.svg", svg)


if __name__ == "__main__":
    main()
