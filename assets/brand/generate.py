"""Regenerate committed Foldex icon assets: python -m pip install Pillow."""
from base64 import b64encode
from io import BytesIO
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
# Include the exact taskbar and title-bar sizes Windows requests at 100–400%
# display scaling. Each frame is sampled directly from the original artwork.
ICON_SIZES = (16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 128, 256)
source = Image.open(HERE / "foldex-source.png").convert("RGBA")
# Ignore nearly transparent export noise without changing the supplied artwork.
bounds = source.getchannel("A").point(lambda a: 255 if a > 8 else 0).getbbox()
if bounds is None:
    raise ValueError("The source icon is empty")
art = source.crop(bounds)


def icon(size, *, fill=1.0):
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    fitted = art.copy()
    # Windows already supplies space around a taskbar icon. Adding another
    # inset here makes the folder visibly smaller than neighboring app icons.
    fitted.thumbnail((round(size * fill), round(size * fill)), Image.Resampling.LANCZOS)
    image.alpha_composite(fitted, ((size - fitted.width) // 2, (size - fitted.height) // 2))
    return image


icon(256).save(HERE / "foldex.png", optimize=True)
icon(256).save(
    HERE / "foldex.ico",
    sizes=[(size, size) for size in ICON_SIZES],
    append_images=[icon(size) for size in ICON_SIZES if size != 256],
)
logo = BytesIO()
icon(512, fill=.90).save(logo, format="PNG", optimize=True)
data = b64encode(logo.getvalue()).decode("ascii")
banner = f'''<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="1200" height="420" viewBox="0 0 1200 420" role="img" aria-labelledby="title desc">
  <title id="title">Foldex</title>
  <desc id="desc">Files, without the friction. A native Windows file explorer built in Rust.</desc>
  <defs>
    <linearGradient id="bg" x2="1" y2="1"><stop stop-color="#111c2a"/><stop offset="1" stop-color="#0b1119"/></linearGradient>
    <radialGradient id="glow"><stop stop-color="#4d9bd4" stop-opacity=".18"/><stop offset="1" stop-color="#4d9bd4" stop-opacity="0"/></radialGradient>
    <linearGradient id="rule"><stop stop-color="#87c7f2"/><stop offset="1" stop-color="#87c7f2" stop-opacity="0"/></linearGradient>
  </defs>
  <rect x=".5" y=".5" width="1199" height="419" rx="24" fill="url(#bg)" stroke="#283546"/>
  <ellipse cx="242" cy="215" rx="300" ry="202" fill="url(#glow)"/>
  <image x="34" y="24" width="370" height="370" xlink:href="data:image/png;base64,{data}"/>
  <g font-family="Segoe UI, Inter, Arial, sans-serif">
    <text x="432" y="88" font-size="12" letter-spacing="3" fill="#85b9df">YOUR FILES. YOUR WORKSPACE.</text>
    <text x="426" y="198" font-size="94" font-weight="650" letter-spacing="-4" fill="#f0f5fa">Foldex</text>
    <text x="432" y="252" font-size="29" fill="#b4c2d3">Files, without the friction.</text>
    <path d="M432 296H1100" stroke="url(#rule)"/>
    <g font-size="11" font-weight="600" letter-spacing="1.8" fill="#8c9db1">
      <text x="432" y="335">NATIVE WINDOWS</text>
      <text x="650" y="335">KEYBOARD READY</text>
      <text x="885" y="335">OPEN SOURCE</text>
    </g>
  </g>
</svg>
'''
(HERE / "banner.svg").write_text(banner, encoding="utf-8")
print("Generated foldex.png, foldex.ico and banner.svg from foldex-source.png")
