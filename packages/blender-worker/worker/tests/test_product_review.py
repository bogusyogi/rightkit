import json
import os
import tempfile
import unittest

from renderright.contract import ContractError, sha256_file
from renderright.product_review import load_product_spec, review_summary


class ProductReviewTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        self.source = os.path.join(self.root, 'source.glb')
        with open(self.source, 'wb') as f:
            f.write(b'synthetic source')
        self.shader = os.path.join(self.root, 'finish.blend')
        with open(self.shader, 'wb') as f:
            f.write(b'synthetic shader placeholder')
        self.spec_path = os.path.join(self.root, 'product.json')
        self.spec = {'schema': 1, 'product_id': 'SYNTHETIC', 'variant': 'test', 'owner': 'Synthetic Test Owner',
                     'confirmed': True, 'confirmed_by': 'Synthetic Test Owner',
                     'parts': {'body': {'substrate': 'steel', 'density_g_cm3': 7.8,
                                        'slots': {'steel-slot': 'steel'}}},
                     'finishes': {'steel': {'version': 'v1', 'approved': True,
                                            'approved_by': 'Synthetic Test Owner',
                                            'shader': {'path': self.shader, 'material': 'SyntheticSteel'}}}}
        self.write_spec()

    def tearDown(self):
        self.tmp.cleanup()

    def write_spec(self):
        with open(self.spec_path, 'w', encoding='utf-8') as f:
            json.dump(self.spec, f)

    def job(self):
        return {'source': self.source, 'params': {'product_spec': self.spec_path}}

    def test_optional_spec_load_and_complete_review_summary(self):
        spec = load_product_spec(self.job())
        result = review_summary(spec, sha256_file(self.source), assignments_match=True, shader_applied=True)
        self.assertTrue(result['reviewed'])
        self.assertEqual(result['finish_tags'], {'steel': 'v1'})
        self.assertEqual(result['approved_by'], 'Synthetic Test Owner')

    def test_missing_or_incomplete_spec_stays_draft(self):
        self.assertIsNone(load_product_spec({'params': {}}))
        self.spec['confirmed'] = False
        self.write_spec()
        result = review_summary(load_product_spec(self.job()), sha256_file(self.source), True, True)
        self.assertFalse(result['reviewed'])
        self.assertIsNone(result['approved_by'])

    def test_material_override_forces_draft(self):
        result = review_summary(self.spec, sha256_file(self.source), True, True, overridden=True)
        self.assertFalse(result['reviewed'])

    def test_incomplete_identity_or_malformed_slot_cannot_be_reviewed(self):
        self.spec["product_id"] = ""
        self.assertFalse(review_summary(self.spec, "source", True, True)["reviewed"])
        self.spec["parts"]["body"]["slots"]["steel-slot"] = ["steel"]
        self.assertFalse(review_summary(self.spec, "source", True, True)["reviewed"])

    def test_invalid_requested_shader_path_is_rejected(self):
        self.spec['finishes']['steel']['shader']['path'] = os.path.join(self.root, 'missing.blend')
        self.write_spec()
        with self.assertRaisesRegex(ContractError, 'shader path'):
            load_product_spec(self.job())


if __name__ == '__main__':
    unittest.main()
