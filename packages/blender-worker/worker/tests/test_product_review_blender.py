"""Small Blender-side checks for primitive draft swatches."""
import os
import sys
import unittest

try:
    import bpy
except ImportError:
    raise unittest.SkipTest("requires Blender's bundled Python runtime")

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from renderright.swatch import _sample_material


class DraftSwatchBlenderTests(unittest.TestCase):
    def test_preset_material_is_local_and_never_claims_review(self):
        material, finish_id = _sample_material({"params": {"material_recipe": "brass"}}, None)
        self.assertIsNone(finish_id)
        self.assertIsNotNone(material.node_tree)
        self.assertAlmostEqual(material.node_tree.nodes["Principled BSDF"].inputs["Metallic"].default_value, 0.82, places=5)


if __name__ == "__main__":
    sys.argv = [__file__]
    unittest.main()
