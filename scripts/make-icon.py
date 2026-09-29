"""Draws the CakeVPN app icon (1024x1024 PNG) without any image library.
Run `npm run tauri icon app-icon.png` afterwards to make every icon size."""
import math, struct, sys, zlib

S = 1024
px = bytearray(S * S * 4)

def blend(x, y, rgb, a):
    if not (0 <= x < S and 0 <= y < S) or a <= 0:
        return
    i = (y * S + x) * 4
    a = min(1.0, a)
    for k in range(3):
        px[i + k] = int(px[i + k] * (1 - a) + rgb[k] * a)
    px[i + 3] = max(px[i + 3], int(255 * a))

def rounded_rect(x0, y0, x1, y1, r, color_at):
    for y in range(int(y0), int(y1) + 1):
        for x in range(int(x0), int(x1) + 1):
            cx = min(max(x, x0 + r), x1 - r)
            cy = min(max(y, y0 + r), y1 - r)
            d = math.hypot(x - cx, y - cy)
            a = max(0.0, min(1.0, r - d + 0.5))
            if a > 0:
                blend(x, y, color_at(x, y), a)

def ellipse(cx, cy, rx, ry, rgb):
    for y in range(int(cy - ry) - 1, int(cy + ry) + 2):
        for x in range(int(cx - rx) - 1, int(cx + rx) + 2):
            d = math.hypot((x - cx) / rx, (y - cy) / ry)
            a = max(0.0, min(1.0, (1 - d) * min(rx, ry) + 0.5))
            blend(x, y, rgb, a)

def lerp(c1, c2, t):
    return tuple(int(c1[k] + (c2[k] - c1[k]) * t) for k in range(3))

# background: pink to peach
rounded_rect(40, 40, 984, 984, 210, lambda x, y: lerp((255, 111, 145), (255, 179, 107), (x + y) / (2 * S)))
# plate
ellipse(512, 780, 330, 52, (255, 240, 232))
# cake body
rounded_rect(222, 470, 802, 770, 40, lambda x, y: (255, 244, 230))
# layer stripe
rounded_rect(222, 610, 802, 640, 10, lambda x, y: (240, 130, 150))
# frosting with drips
rounded_rect(212, 440, 812, 520, 40, lambda x, y: (224, 79, 116))
for dx in (270, 380, 500, 610, 730):
    rounded_rect(dx - 26, 480, dx + 26, 560 + (dx % 3) * 18, 26, lambda x, y: (224, 79, 116))
# candle
rounded_rect(488, 290, 536, 450, 14, lambda x, y: (255, 255, 255) if (y // 30) % 2 else (120, 180, 255))
# flame
ellipse(512, 250, 30, 48, (255, 196, 61))
ellipse(512, 262, 15, 26, (255, 245, 200))

raw = b"".join(b"\x00" + bytes(px[y * S * 4:(y + 1) * S * 4]) for y in range(S))
def chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", S, S, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
open(sys.argv[1] if len(sys.argv) > 1 else "app-icon.png", "wb").write(png)
