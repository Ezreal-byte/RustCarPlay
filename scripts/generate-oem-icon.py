#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Render the original, brand-neutral OEM placeholder. Build-time only: Pillow."""
from pathlib import Path
from PIL import Image, ImageDraw

image = Image.new("RGBA", (1024, 1024), "#163846")
d = ImageDraw.Draw(image)
d.rounded_rectangle((120, 400, 904, 744), radius=88, fill="#f1f8fa")
d.polygon([(228, 430), (324, 252), (700, 252), (796, 430)], fill="#f1f8fa")
d.polygon([(328, 400), (382, 304), (642, 304), (696, 400)], fill="#163846")
d.rounded_rectangle((160, 670, 292, 808), radius=35, fill="#f1f8fa")
d.rounded_rectangle((732, 670, 864, 808), radius=35, fill="#f1f8fa")
d.ellipse((198, 494, 322, 618), fill="#163846")
d.ellipse((702, 494, 826, 618), fill="#163846")
d.rounded_rectangle((414, 526, 610, 586), radius=22, fill="#163846")
destination = Path(__file__).resolve().parents[1] / "crates/carplay-receiver/assets/oem-car.png"
destination.parent.mkdir(parents=True, exist_ok=True)
image.resize((256, 256), Image.Resampling.LANCZOS).save(destination, optimize=True)
