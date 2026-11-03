import copy
import hashlib
import unittest

from renderright.contract import ContractError
from renderright.product_registration import (
    check_source_sha, resolve_registration, sample_registration, validate_registration,
)

SYNTHETIC_SHA = hashlib.sha256(b"synthetic company glb").hexdigest()


def company_registration(**changes):
    block = {
        "glb_to_knife": [[1, 0, 0, 0.1], [0, 1, 0, 0.0], [0, 0, 1, 0.0], [0, 0, 0, 1]],
        "pivot_knife_mm": [1.0, 2.0, 3.0],
        "bore": {"axis_origin_mm": [0, 0, 0], "axis_datum_mm": 1.0, "ring_mm": [2.8, 3.2]},
    }
    block.update(changes)
    return block


class SampleRegistrationTests(unittest.TestCase):
    def test_sample_registration_carries_the_fenrir_constants(self):
        block = sample_registration()
        self.assertEqual(block["source_sha256"], "48af6070d85c1cb34920a7db531f0a89fe8491c86e29d5ab6ab809c9c3ad9cb4")
        self.assertEqual(block["archive_obj_sha256"], "443a01021ca884f5bcd40a0988f47d1d3175403397d66321f391e7ba3ae95d91")
        self.assertEqual(block["pivot_knife_mm"], [-3.136, 0.237, 10.0])
        self.assertEqual(block["glb_to_knife"][1], [0, 0, -1, 0.01951251])
        self.assertEqual(block["archive_reference"]["source_obj_sha256"], block["archive_obj_sha256"])
        self.assertEqual(block["tolerances"]["residual_mm"], 0.05)

    def test_spec_without_registration_falls_back_to_sample_default(self):
        block, source = resolve_registration({"schema": 1, "finishes": {}})
        self.assertEqual(source, "sample_default")
        self.assertIsNotNone(block["source_sha256"])

    def test_sample_default_rejects_other_glb(self):
        block, _ = resolve_registration(None)
        with self.assertRaisesRegex(ContractError, "not the source declared"):
            check_source_sha(block, SYNTHETIC_SHA, "the sample registration")


class CompanyRegistrationTests(unittest.TestCase):
    def test_company_spec_registration_is_used_and_defaults_filled(self):
        spec = {"schema": 1, "registration": company_registration()}
        block, source = resolve_registration(spec)
        self.assertEqual(source, "product_spec")
        self.assertEqual(block["tip_band_mm"], 0.15)
        self.assertEqual(block["blade_marker"], "BLADE")
        self.assertEqual(block["part_map"], {})
        self.assertNotIn("source_sha256", block)

    def test_undeclared_sha_accepts_any_glb(self):
        block = validate_registration(company_registration())
        check_source_sha(block, SYNTHETIC_SHA, "the product spec registration")

    def test_declared_sha_is_enforced(self):
        block = validate_registration(company_registration(source_sha256=SYNTHETIC_SHA))
        check_source_sha(block, SYNTHETIC_SHA, "the product spec registration")
        with self.assertRaisesRegex(ContractError, "not the source declared"):
            check_source_sha(block, "0" * 64, "the product spec registration")

    def test_part_map_and_blade_marker_are_accepted(self):
        block = validate_registration(company_registration(
            part_map={"HOUSING_A": "housing", "HOUSING_B": "housing"}, blade_marker="EDGE"))
        self.assertEqual(block["part_map"]["HOUSING_A"], "housing")
        self.assertEqual(block["blade_marker"], "EDGE")

    def test_archive_reference_must_match_declared_obj(self):
        reference = {"source_obj_sha256": SYNTHETIC_SHA, "rest_normal": [0, 0, 1],
                     "pivot_world_m": [0, 0, 0], "tip_world_m": [0, 0, 0]}
        with self.assertRaisesRegex(ContractError, "another OBJ"):
            validate_registration(company_registration(archive_obj_sha256="1" * 64, archive_reference=reference))

    def test_rejects_malformed_matrix_and_unknown_tolerance(self):
        bad_row = copy.deepcopy(company_registration())
        bad_row["glb_to_knife"][3] = [0, 0, 0, 2]
        with self.assertRaisesRegex(ContractError, "last row"):
            validate_registration(bad_row)
        with self.assertRaisesRegex(ContractError, "tolerances"):
            validate_registration(company_registration(tolerances={"teleport": 1.0}))

    def test_rejects_bad_ring_and_sha_text(self):
        with self.assertRaisesRegex(ContractError, "increasing positive pair"):
            validate_registration(company_registration(bore={"axis_origin_mm": [0, 0, 0], "axis_datum_mm": 1.0, "ring_mm": [3.2, 2.8]}))
        with self.assertRaisesRegex(ContractError, "SHA-256"):
            validate_registration(company_registration(source_sha256="ABC"))


if __name__ == "__main__":
    unittest.main()
