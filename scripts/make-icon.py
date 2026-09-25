#!/usr/bin/env python3
# Renders the Flox mark (same geometry as the TV app launcher icon and the Mac app icon)
# into assets/flox.ico and assets/flox-256.png.
import os
from PIL import Image, ImageDraw

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets")
bg, fg = (23, 23, 23), (250, 250, 250)
rects = [(27, 21, 116, 33), (27, 21, 39, 122), (104, 21, 116, 100), (47, 42, 61, 122), (47, 42, 95, 54), (47, 66, 95, 78), (69, 88, 116, 100)]
sizes = (16, 20, 24, 32, 40, 48, 64, 128, 256)

def render(size):
    s = 8
    big = size * s
    im = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    inset = big * 100 // 1024
    d.rounded_rectangle((inset, inset, big - inset - 1, big - inset - 1), radius=big * 185 // 1024, fill=bg)
    k = (big - 2 * inset) / 144
    for x0, y0, x1, y1 in rects:
        d.rectangle((inset + x0 * k, inset + y0 * k, inset + (x1 + 1) * k - 1, inset + (y1 + 1) * k - 1), fill=fg)
    return im.resize((size, size), Image.LANCZOS)

# Each size is rendered on its own (not downscaled from 256) so the small sizes stay crisp.
frames = {size: render(size) for size in sizes}
frames[256].save(os.path.join(root, "flox-256.png"))
frames[256].save(
    os.path.join(root, "flox.ico"),
    format="ICO",
    sizes=[(size, size) for size in sizes],
    append_images=[frames[size] for size in sizes if size != 256],
)
