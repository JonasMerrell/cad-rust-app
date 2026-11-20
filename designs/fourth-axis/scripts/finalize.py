#!/usr/bin/env python3
"""Collect the final iteration's pictures and STEP files into renders/ and step/ (pictures cropped to their content).

    finalize.py <iteration>
"""
import sys, os, json, shutil
from PIL import Image, ImageChops
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build import ROOT


def crop(src, dst, pad=40):
    im = Image.open(src).convert("RGB")
    bg = Image.new("RGB", im.size, im.getpixel((2, 2)))
    box = ImageChops.difference(im, bg).point(lambda v: 255 if v > 12 else 0).getbbox()
    if box:
        box = (max(0, box[0] - pad), max(0, box[1] - pad), min(im.size[0], box[2] + pad), min(im.size[1], box[3] + pad))
        im = im.crop(box)
    im.save(dst)


def main(it):
    out = os.path.join(ROOT, f"iter{it}"); rd = os.path.join(ROOT, "renders"); sd = os.path.join(ROOT, "step")
    for d in (rd, sd, os.path.join(rd, "parts")):
        shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
    names = {"render-iso.png": "assembly-iso.png", "render-iso-back.png": "assembly-iso-back.png", "render-front.png": "assembly-front.png",
             "render-top.png": "assembly-top.png", "render-right.png": "assembly-from-tailstock-end.png", "render-exploded.png": "exploded.png",
             "render-exploded-front.png": "exploded-front.png", "render-section.png": "section-through-spindle-and-reducer.png",
             "render-section-iso.png": "section-oblique.png"}
    for a, b in names.items():
        if os.path.exists(os.path.join(out, a)): crop(os.path.join(out, a), os.path.join(rd, b))
    m = os.path.join(out, "machine")
    for a, b in (("on-machine-iso.png", "on-the-mill-iso.png"), ("on-machine-front.png", "on-the-mill-front.png"), ("on-machine-top.png", "on-the-mill-top.png"),
                 ("on-machine-close.png", "on-the-mill-close.png")):
        if os.path.exists(os.path.join(m, a)): crop(os.path.join(m, a), os.path.join(rd, b))
    for a in sorted(os.listdir(os.path.join(out, "fea"))):
        if a.endswith(".png"): shutil.copy(os.path.join(out, "fea", a), os.path.join(rd, a))
    meta = json.load(open(os.path.join(out, "meta.json")))
    for q in meta["parts"]:
        if q["kind"] in ("machined", "printed"):
            crop(os.path.join(out, "parts", q["stem"] + ".png"), os.path.join(rd, "parts", q["stem"] + ".png"), 20)
            shutil.copy(os.path.join(out, "parts", q["stem"] + ".step"), os.path.join(sd, q["stem"] + ".step"))
    shutil.copy(os.path.join(out, "a-axis-assembly.step"), os.path.join(sd, "a-axis-assembly.step"))
    print(len(os.listdir(rd)), "renders,", len(os.listdir(sd)), "step files")


if __name__ == "__main__":
    main(int(sys.argv[1]))
