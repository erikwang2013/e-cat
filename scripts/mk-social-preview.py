#!/usr/bin/env python3
"""生成 e-cat 的 GitHub Social Preview（1280×640，中英双语）。

配色取自上一版（保持视觉一致）；字体用 Noto Sans CJK SC —— 一个字体覆盖中英，
双语排版不会出现字形不匹配。
"""
import subprocess
from PIL import Image, ImageDraw, ImageFont

W, H = 1280, 640
BG = (249, 246, 241)
INK = (43, 38, 35)          # 标题
ACCENT = (153, 109, 90)     # 「一只猫」/ 强调
ZH = (122, 110, 102)        # 中文行
EN = (148, 144, 139)        # 英文行（更弱的灰）
BADGE_BG = (240, 232, 224)
BADGE_BD = (226, 214, 202)
CIRCLE_BG = (250, 250, 250)   # 与猫图自身底色一致 ⇒ 方边不可见
CIRCLE_BD = (232, 222, 210)

FONT_BOLD = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc"
FONT_REG = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"
SC = 2  # .ttc 里 index 2 = Noto Sans CJK SC


def font(path, size):
    return ImageFont.truetype(path, size, index=SC)


def fit(draw, text, path, start, max_w):
    """从 start 号开始逐号缩小，直到文本宽度 <= max_w。"""
    size = start
    while size > 12:
        f = font(path, size)
        if draw.textlength(text, font=f) <= max_w:
            return f
        size -= 1
    return font(path, 12)


im = Image.new("RGB", (W, H), BG)
d = ImageDraw.Draw(im)

# ── 左侧：猫（从 SVG 渲染）────────────────────────────────────────────
subprocess.run(
    ["rsvg-convert", "-w", "520", "-h", "520", "-o", "/tmp/cat.png", "docs/e-cat.svg"],
    check=True,
)
cat = Image.open("/tmp/cat.png").convert("RGBA")

CX, CY, R = 250, 300, 172
d.ellipse([CX - R, CY - R, CX + R, CY + R], fill=CIRCLE_BG, outline=CIRCLE_BD, width=3)
# 把猫缩到圆内并居中贴上去（保留透明通道）
# 缩到略大于圆（让猫填满圆窗、边缘不外露），再用**圆形蒙版**裁掉方角
scale = (2 * R * 1.06) / max(cat.size)
cat_s = cat.resize((int(cat.width * scale), int(cat.height * scale)), Image.LANCZOS)
mask = Image.new("L", cat_s.size, 0)
ImageDraw.Draw(mask).ellipse([0, 0, cat_s.width - 1, cat_s.height - 1], fill=255)
im.paste(cat_s, (CX - cat_s.width // 2, CY - cat_s.height // 2), mask)

# ── 右侧：文字 ───────────────────────────────────────────────────────
X = 490
MAXW = W - X - 46

# 标题：Ecat 一只猫
f_title = font(FONT_BOLD, 104)
d.text((X, 96), "Ecat", font=f_title, fill=INK)
x2 = X + d.textlength("Ecat", font=f_title) + 22
f_zh_title = font(FONT_BOLD, 54)
d.text((x2, 96 + (104 - 54) - 6), "一只猫", font=f_zh_title, fill=ACCENT)

y = 250
LINE_GAP_ZH = 56
LINE_GAP_EN = 40

pairs = [
    ("Rust 微服务框架 · 对标 go-kratos/kratos",
     "Rust microservices framework · benchmarked against go-kratos/kratos"),
    ("HTTP · gRPC · 16 数据后端 · 完整 ORM",
     "HTTP · gRPC · 16 data backends · full ORM"),
]

for zh, en in pairs:
    d.text((X, y), zh, font=fit(d, zh, FONT_BOLD, 38, MAXW), fill=ZH)
    y += LINE_GAP_ZH
    d.text((X, y), en, font=fit(d, en, FONT_REG, 30, MAXW), fill=EN)
    y += LINE_GAP_EN + 18

# 徽章：56 crates
f_badge = font(FONT_REG, 30)
btext = "56 crates"
tw = d.textlength(btext, font=f_badge)
bx, by = X, y + 4
d.rounded_rectangle([bx, by, bx + tw + 42, by + 54], radius=27,
                    fill=BADGE_BG, outline=BADGE_BD, width=2)
d.text((bx + 21, by + 12), btext, font=f_badge, fill=(150, 136, 124))

# ── 页脚 ─────────────────────────────────────────────────────────────
FY = 566
d.line([(X - 20, FY), (W - 40, FY)], fill=(226, 216, 204), width=2)
f_foot = font(FONT_REG, 27)
d.text((X - 20, FY + 22), "github.com/erikwang2013/e-cat", font=f_foot, fill=(150, 136, 124))
right = "Apache-2.0"
d.text((W - 40 - d.textlength(right, font=f_foot), FY + 22), right,
       font=f_foot, fill=(150, 136, 124))

im.save("docs/social-preview.png", optimize=True)
print("✅ docs/social-preview.png", im.size)
