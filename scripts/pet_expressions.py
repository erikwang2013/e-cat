"""Expression data for the Ecat pet generator.

Each entry supplies facial features, optional pose overrides, effects and
CSS.  Imported by generate_pet_assets.py; kept separate to hold both files
under the project's 500-line guideline.
"""

from generate_pet_assets import (
    FUR, CREAM, INK, PINK, TEAL,
    brow, drop, hand, heart, sparkle, sparkles,
    LEFT_LEG,
)

WORRIED = brow(184, 150, 214, 142) + brow(296, 150, 266, 142)
ANGRY = brow(184, 142, 214, 152) + brow(296, 142, 266, 152)
DETERMINED = brow(184, 146, 214, 150) + brow(296, 146, 266, 150)
SCARED = brow(184, 136, 214, 132) + brow(296, 136, 266, 132)


def cloud(x, y):
    """Small grey rain cloud."""
    return (
        f'<g class="fx-pulse"><path d="M {x - 38} {y + 10} '
        f'C {x - 50} {y + 10} {x - 50} {y - 6} {x - 38} {y - 6} '
        f'C {x - 38} {y - 22} {x - 20} {y - 28} {x - 8} {y - 18} '
        f'C {x + 2} {y - 36} {x + 26} {y - 32} {x + 28} {y - 14} '
        f'C {x + 44} {y - 14} {x + 46} {y + 10} {x + 30} {y + 10} Z" '
        f'fill="#C9D6E3" stroke="{INK}" stroke-width="3" stroke-linejoin="round"/></g>'
    )


def rain_cloud(x, y):
    drops = "".join(
        drop(x + dx, y + 30, 5.5, cls="fx-rain", delay=d)
        for dx, d in ((-34, 0.0), (-18, 0.55), (-4, 0.3), (12, 0.8), (26, 0.15), (38, 0.65))
    )
    return cloud(x, y) + drops


def tear_stream():
    tracks = (
        f'<path d="M 190 196 Q 185 216 189 236" fill="none" stroke="#5FBBEA" '
        f'stroke-width="7" stroke-linecap="round" opacity=".7"/>'
        f'<path d="M 290 196 Q 295 216 291 236" fill="none" stroke="#5FBBEA" '
        f'stroke-width="7" stroke-linecap="round" opacity=".7"/>'
    )
    drops = "".join(
        drop(x, y, s, delay=d)
        for x, y, s, d in (
            (186, 204, 6, 0.2), (294, 204, 6, 0.5),
            (187, 244, 9, 0.0), (187, 272, 8, 0.8),
            (293, 244, 9, 0.4), (293, 272, 8, 1.1),
        )
    )
    return tracks + drops


def confetti():
    pieces = [
        (118, 96, "#F26D7D", 18, 0.0, "r"),
        (156, 76, "#F2C94C", -24, 0.35, "c"),
        (202, 96, "#2F8F8A", 40, 0.6, "r"),
        (272, 82, "#7EC8F2", 12, 0.15, "c"),
        (318, 100, "#F26D7D", -30, 0.45, "r"),
        (352, 78, "#F2C94C", 25, 0.8, "c"),
        (360, 128, "#2F8F8A", -15, 0.2, "r"),
        (98, 148, "#7EC8F2", 50, 0.95, "c"),
    ]
    out = []
    for x, y, color, rot, delay, kind in pieces:
        if kind == "r":
            shape = (f'<rect x="-4" y="-6" width="8" height="12" rx="2" fill="{color}" '
                     f'class="fx-confetti" style="animation-delay:{delay}s"/>')
        else:
            shape = (f'<circle r="4" fill="{color}" class="fx-confetti" '
                     f'style="animation-delay:{delay}s"/>')
        out.append(f'<g transform="translate({x},{y}) rotate({rot})">{shape}</g>')
    return "".join(out)


def zigzag(points, delay=0.0, color=INK):
    d = "M " + " L ".join(f"{x} {y}" for x, y in points)
    return (
        f'<path class="fx-pulse" style="animation-delay:{delay}s" d="{d}" fill="none" '
        f'stroke="{color}" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>'
    )


def shock():
    return (
        zigzag([(132, 152), (120, 138), (132, 126), (118, 110)], 0.0)
        + zigzag([(348, 152), (360, 138), (348, 126), (362, 110)], 0.35)
        + drop(338, 166, 8, delay=0.1)
        + drop(146, 174, 7, delay=0.6)
    )


def steam():
    puffs = (
        f'<g class="fx-rise" stroke="{INK}" stroke-width="2.5"><circle cx="128" cy="76" r="10" fill="#E6EDF2"/>'
        f'<circle cx="114" cy="62" r="7" fill="#E6EDF2"/>'
        f'<circle cx="137" cy="60" r="6" fill="#E6EDF2"/></g>'
        f'<g class="fx-rise" style="animation-delay:.9s" stroke="{INK}" stroke-width="2.5">'
        f'<circle cx="352" cy="76" r="10" fill="#E6EDF2"/>'
        f'<circle cx="366" cy="62" r="7" fill="#E6EDF2"/>'
        f'<circle cx="343" cy="60" r="6" fill="#E6EDF2"/></g>'
    )
    vein = (
        f'<g class="fx-pulse" style="animation-delay:.2s" transform="translate(312,118)">'
        f'<path d="M -15 -15 L -4 -4 M 15 15 L 4 4 M 15 -15 L 4 -4 M -15 15 L -4 4" '
        f'stroke="#E23D3D" stroke-width="5" stroke-linecap="round"/></g>'
    )
    return puffs + vein


def hearts():
    return (
        heart(112, 156, 16, cls="fx-rise", delay=0.0)
        + heart(366, 176, 13, cls="fx-rise", delay=0.55)
        + heart(348, 118, 9, cls="fx-rise", delay=1.0)
        + heart(128, 220, 8, cls="fx-rise", delay=1.35)
    )


def arm(x1, y1, x2, y2, paw_x, paw_y, cls="", delay=0.0, r=14):
    """Fur arm with a cream paw at its end."""
    path = f"M {x1} {y1} Q {(x1 + x2) / 2} {y1 - 14} {x2} {y2}"
    return (
        f'<g class="{cls}" style="animation-delay:{delay}s">'
        f'<path d="{path}" fill="none" stroke="{INK}" stroke-width="40" stroke-linecap="round"/>'
        f'<path d="{path}" fill="none" stroke="{FUR}" stroke-width="31" stroke-linecap="round"/>'
        f'{hand(paw_x, paw_y, r)}</g>'
    )


def facepalm():
    paw = (
        f'<g class="fx-shake">'
        f'<path d="M 304 334 C 310 296 302 252 284 222" fill="none" stroke="{INK}" '
        f'stroke-width="40" stroke-linecap="round"/>'
        f'<path d="M 304 334 C 310 296 302 252 284 222" fill="none" stroke="{FUR}" '
        f'stroke-width="31" stroke-linecap="round"/>'
        f'<ellipse cx="252" cy="172" rx="47" ry="33" fill="{CREAM}" stroke="{INK}" stroke-width="4"/>'
        f'<path d="M 224 158 L 224 186 M 244 152 L 244 186 M 264 152 L 264 186 M 284 158 L 284 184" '
        f'stroke="{INK}" stroke-width="2.6" stroke-linecap="round" opacity=".5"/>'
        f'</g>'
    )
    dots = "".join(
        f'<circle cx="{x}" cy="112" r="3.4" fill="{INK}" opacity=".55"/>'
        for x in (314, 328, 342)
    )
    return paw + dots + drop(348, 150, 7, delay=0.3)


def knock():
    return (
        f'<path class="fx-pulse" d="M 336 260 Q 356 274 340 290" fill="none" '
        f'stroke="{INK}" stroke-width="5" stroke-linecap="round" opacity=".85"/>'
        f'<path class="fx-pulse" style="animation-delay:.25s" d="M 348 250 Q 370 274 352 300" '
        f'fill="none" stroke="{INK}" stroke-width="5" stroke-linecap="round" opacity=".6"/>'
        f'<path class="fx-pulse" style="animation-delay:.5s" d="M 360 242 Q 384 274 364 308" '
        f'fill="none" stroke="{INK}" stroke-width="4" stroke-linecap="round" opacity=".35"/>'
    )


CUTE_LEGS = LEFT_LEG + arm(280, 330, 300, 302, 302, 296, cls="fx-wiggle")
PLEADING_LEGS = (
    arm(198, 342, 218, 334, 220, 330, cls="fx-wiggle", r=13)
    + arm(282, 342, 262, 334, 260, 330, cls="fx-wiggle", delay=0.4, r=13)
)
KNOCK_LEGS = LEFT_LEG + arm(284, 332, 316, 296, 320, 290, cls="fx-knock", r=15)


EXPRESSIONS = {
    "smile": dict(
        cn="笑", en="Smile",
        face=dict(eyes="happy", mouth_kind="smile", blush=True),
        fx=sparkles([(112, 132, 9, 0.0), (362, 214, 8, 0.7), (352, 122, 5, 0.3)]),
    ),
    "joy": dict(
        cn="欢乐", en="Joy",
        face=dict(eyes="happy", mouth_kind="open-smile", blush=True),
        fx=confetti() + sparkles([(112, 212, 7, 0.5), (370, 196, 7, 0.2)]),
    ),
    "cry": dict(
        cn="哭", en="Cry",
        face=dict(eyes="sad", brows=WORRIED, mouth_kind="wavy"),
        fx=tear_stream(),
    ),
    "depressed": dict(
        cn="沮丧", en="Depressed",
        face=dict(eyes="half", brows=WORRIED, mouth_kind="frown"),
        fx=rain_cloud(240, 70),
    ),
    "terrified": dict(
        cn="惊恐", en="Terrified",
        face=dict(eyes="scared", brows=SCARED, mouth_kind="scream"),
        head_class="head fx-shake",
        fx=shock(),
    ),
    "angry": dict(
        cn="愤怒", en="Angry",
        face=dict(eyes="lens", brows=ANGRY, mouth_kind="gritted"),
        fx=steam(),
    ),
    "coquettish": dict(
        cn="撒娇", en="Coquettish",
        face=dict(eyes="wink", mouth_kind="smile", blush=True),
        head_class="head head-tilt",
        fx=hearts(),
    ),
    "cute": dict(
        cn="卖萌", en="Cute",
        face=dict(eyes="pleading", mouth_kind="smile", blush=True),
        legs=CUTE_LEGS,
        fx=sparkles([(118, 150, 10, 0.1), (356, 150, 8, 0.6), (130, 236, 6, 1.0)]),
    ),
    "pleasing": dict(
        cn="讨好", en="Pleasing",
        face=dict(eyes="pleading", brows=WORRIED, mouth_kind="wavy", blush=True),
        legs=PLEADING_LEGS,
        fx=drop(340, 156, 7, delay=0.2) + sparkles([(120, 154, 7, 0.4)]),
    ),
    "knocking": dict(
        cn="敲打", en="Knocking",
        face=dict(eyes="lens", brows=DETERMINED, mouth_kind="neutral"),
        legs=KNOCK_LEGS,
        fx=knock(),
    ),
    "facepalm": dict(
        cn="捂脸", en="Facepalm",
        face=dict(eyes="covered", mouth_kind="wavy", blush=True),
        fx=facepalm(),
    ),
    "tongue": dict(
        cn="吐舌头", en="Tongue Out",
        face=dict(eyes="wink", mouth_kind="tongue", blush=True),
        fx=sparkles([(120, 148, 8, 0.2), (358, 206, 7, 0.7)]),
    ),
}
