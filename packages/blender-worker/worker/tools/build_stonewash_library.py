"""Create three generic, unapproved procedural stonewash material studies."""

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

from renderright.stonewash import build_library, BEVEL_RADIUS_M, METALLIC, ROUGHNESS, SCRATCH_DEPTH_UM, VARIANTS


def main(args):
    if len(args) != 1 or not os.path.isabs(args[0]) or not args[0].lower().endswith(".blend"):
        raise SystemExit("usage: blender -b --python build_stonewash_library.py -- OUTPUT.blend")
    output = os.path.abspath(args[0])
    os.makedirs(os.path.dirname(output), exist_ok=True)
    build_library(output)
    note = {
        "status": "DRAFT; unapproved generic material studies",
        "materials": ["DRAFT stonewash {}mm".format(mm) for mm in VARIANTS],
        "only_variant_control": "cell_mm: 1, 2, or 3",
        "scratch_depth_um": SCRATCH_DEPTH_UM,
        "roughness": ROUGHNESS,
        "metallic": METALLIC,
        "bevel_radius_m": BEVEL_RADIUS_M,
        "anisotropy": 0,
        "coat": 0,
        "spatial_basis": "Geometry Position in world metres; independent of object scale",
        "identity": "No product or substrate identity is asserted; not approved steel, titanium, or G10.",
    }
    with open(os.path.splitext(output)[0] + ".json", "w", encoding="utf-8") as stream:
        json.dump(note, stream, indent=2)
        stream.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else sys.argv[1:]))
