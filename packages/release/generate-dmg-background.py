#!/usr/bin/env python3
from __future__ import annotations

import argparse
import subprocess
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter


def rgb(value: str) -> tuple[int, int, int]:
    value = value.removeprefix("#")
    if len(value) != 6:
        raise argparse.ArgumentTypeError("colors must be six-digit hex values")
    return tuple(int(value[i:i + 2], 16) for i in (0, 2, 4))


def make(width: int, height: int, scale: int, background: tuple[int, int, int], header: tuple[int, int, int], accent: tuple[int, int, int], icon_path: Path) -> Image.Image:
    image = Image.new("RGBA", (width, height), (*background, 255))
    draw = ImageDraw.Draw(image)
    band = 86 * scale
    draw.rectangle((0, 0, width, band), fill=(*header, 255))
    draw.rectangle((0, band - 2 * scale, width, band), fill=(*accent, 255))
    arrow = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    ad = ImageDraw.Draw(arrow)
    stroke = 4 * scale
    ad.line([(315 * scale, 231 * scale), (463 * scale, 231 * scale)], fill=(*accent, 255), width=stroke)
    ad.line([(438 * scale, 205 * scale), (466 * scale, 231 * scale), (438 * scale, 257 * scale)], fill=(*accent, 255), width=stroke, joint="curve")
    image.alpha_composite(arrow.filter(ImageFilter.GaussianBlur(scale)))
    image.alpha_composite(arrow)
    if icon_path.exists():
        icon = Image.open(icon_path).convert("RGBA").resize((48 * scale, 48 * scale), Image.Resampling.LANCZOS)
        image.alpha_composite(icon, (34 * scale, 20 * scale))
    return image.convert("RGB")


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--output-dir", required=True, type=Path)
    p.add_argument("--icon", required=True, type=Path)
    p.add_argument("--background", required=True, type=rgb)
    p.add_argument("--header", required=True, type=rgb)
    p.add_argument("--accent", required=True, type=rgb)
    a = p.parse_args()
    a.output_dir.mkdir(parents=True, exist_ok=True)
    low, high, tiff = (a.output_dir / name for name in ("background.png", "background@2x.png", "background.tiff"))
    make(660, 400, 1, a.background, a.header, a.accent, a.icon).save(low)
    make(1320, 800, 2, a.background, a.header, a.accent, a.icon).save(high)
    subprocess.run(["tiffutil", "-cathidpicheck", str(low), str(high), "-out", str(tiff)], check=True)
    print(tiff)


if __name__ == "__main__":
    main()
