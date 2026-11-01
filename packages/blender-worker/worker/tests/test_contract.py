import json
import os
import tempfile
import unittest

from renderright.contract import ContractError, canonical_hash, normalize_job, validate_output_path
from renderright.pngcheck import decode_png, frame_quality


ROOT = os.path.dirname(os.path.dirname(__file__))


class ContractTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.out = os.path.join(self.tmp.name, "project")
        os.makedirs(self.out)
        self.glb = os.path.join(self.tmp.name, "source.glb")
        with open(self.glb, "wb") as stream:
            stream.write(b"synthetic source")

    def tearDown(self):
        self.tmp.cleanup()

    def job(self, **changes):
        value = {"schema": 1, "job_id": "job-1", "kind": "image", "source": self.glb,
                 "recipe": "hero", "output_dir": self.out, "params": {}}
        value.update(changes)
        return value

    def test_beauty_jobs_allow_draft_rendering_and_material_overrides(self):
        for recipe, kind in [("hero", "image"), ("turntable", "video"), ("exploded", "video"), ("film", "video")]:
            params = {"mechanism_json": "synthetic-unused.json"} if recipe == "exploded" else {}
            normalized, _ = normalize_job(self.job(recipe=recipe, kind=kind, params=params), ROOT)
            self.assertEqual(normalized["recipe"], recipe)
        normalized, _ = normalize_job(self.job(params={"material_recipe": "brass"}), ROOT)
        self.assertEqual(normalized["params"]["material_recipe"], "brass")

    def test_primitive_swatch_needs_no_product_source_or_approval(self):
        job = self.job(recipe="swatch", kind="authoring", source=None,
                       params={"samples": 96, "material_recipe": "brass"})
        normalized, _ = normalize_job(job, ROOT)
        self.assertIsNone(normalized["source"])

    def test_defaults_and_fingerprint_are_deterministic(self):
        normalized, manifest = normalize_job(self.job(), ROOT)
        self.assertEqual(normalized["params"]["width"], 1600)
        self.assertEqual(manifest["schema"], 1)
        self.assertEqual(canonical_hash({"a": 1, "b": 2}), canonical_hash({"b": 2, "a": 1}))

    def test_rejects_unbounded_dimensions_and_kind_mismatch(self):
        with self.assertRaises(ContractError):
            normalize_job(self.job(params={"width": 9000}), ROOT)
        with self.assertRaises(ContractError):
            normalize_job(self.job(kind="video"), ROOT)

    def test_probe_is_source_free_image_job(self):
        value = self.job(kind="image", source=None, recipe="probe")
        normalized, _ = normalize_job(value, ROOT)
        self.assertIsNone(normalized.get("source"))

    def test_authoring_jobs_normalize_with_safe_defaults(self):
        for recipe in ("lookdev", "measure"):
            normalized, _ = normalize_job(self.job(kind="authoring", recipe=recipe), ROOT)
            self.assertEqual(normalized["params"]["look_profile"], "product_premium_v1")
            self.assertEqual(normalized["params"]["view"], "hero")
            self.assertGreaterEqual(normalized["params"]["samples"], 96)
            self.assertEqual(normalized["params"]["frame_count"], 1)
            self.assertFalse(normalized["params"]["denoise"])

    def test_authoring_rejects_unsafe_samples_view_and_denoise(self):
        for params in ({"samples": 95}, {"view": "side"}, {"denoise": True}, {"frame_count": 2},
                       {"prompt": "override"}, {"transparent": True},
                       {"env": "studio_lightbox"}):
            with self.assertRaises(ContractError):
                normalize_job(self.job(kind="authoring", recipe="lookdev", params=params), ROOT)
        with self.assertRaises(ContractError):
            normalize_job(self.job(kind="image", recipe="lookdev"), ROOT)

    def test_look_profile_canonical_id_and_legacy_alias(self):
        canonical, _ = normalize_job(self.job(kind="authoring", recipe="lookdev",
                                              params={"look_profile": "product_premium_v1"}), ROOT)
        legacy, _ = normalize_job(self.job(kind="authoring", recipe="lookdev",
                                           params={"look_profile": "fenrir_premium_v1"}), ROOT)
        self.assertEqual(canonical["params"]["look_profile"], "product_premium_v1")
        self.assertEqual(legacy["params"]["look_profile"], "product_premium_v1")
        with self.assertRaises(ContractError):
            normalize_job(self.job(kind="authoring", recipe="lookdev",
                                   params={"look_profile": "other_company_v1"}), ROOT)

    def test_artifact_paths_cannot_escape_output(self):
        with self.assertRaises(ContractError):
            validate_output_path(self.out, "../outside.png")
        self.assertTrue(validate_output_path(self.out, "frames/f.png").startswith(self.out))

    def test_film_regen_controls_are_bounded_and_normalized(self):
        params = {"frame_count": 24, "shot_id": "profile", "shot_start_frame": 24,
                  "source_frame_count": 96, "prompt": "A close product film.",
                  "camera": {"lens_mm": 85, "azimuth_deg": -159, "elevation_deg": 19.9,
                             "distance_scale": 1.16}}
        job = self.job(kind="video", recipe="film", params=params)
        normalized, _ = normalize_job(job, ROOT)
        self.assertEqual(normalized["params"]["shot_id"], "profile")
        self.assertEqual(normalized["params"]["camera"]["lens_mm"], 85.0)

    def test_film_regen_rejects_unknown_or_nonfinite_camera_fields(self):
        for camera in ({"focal_guess": 50}, {"lens_mm": float("nan")}, {"elevation_deg": 90}):
            job = self.job(kind="video", recipe="film", params={"camera": camera})
            with self.assertRaises(ContractError):
                normalize_job(job, ROOT)

    def test_selected_shot_offset_must_fit_source_timeline(self):
        job = self.job(kind="video", recipe="film", params={"frame_count": 24, "shot_id": "hero",
                     "shot_start_frame": 80, "source_frame_count": 96})
        with self.assertRaises(ContractError):
            normalize_job(job, ROOT)

    def test_png_integrity_decoder_checks_dimensions(self):
        # 1x1 RGB PNG, generated from a fixed minimal PNG stream.
        import struct
        import zlib
        path = os.path.join(self.tmp.name, "one.png")
        def chunk(kind, payload):
            return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload) & 0xffffffff)
        png = b"\x89PNG\r\n\x1a\n"
        png += chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
        png += chunk(b"IDAT", zlib.compress(b"\0\xff\0\x00\xff"))
        png += chunk(b"IEND", b"")
        with open(path, "wb") as f:
            f.write(png)
        w, h, bpp, pixels = decode_png(path)
        self.assertEqual((w, h, bpp, len(pixels)), (1, 1, 4, 4))

    def test_frame_quality_blocks_black_frames_and_accepts_valid_silhouettes(self):
        with self.assertRaisesRegex(ValueError, "near-constant"):
            frame_quality((2, 2, 3, bytearray([0] * 12)))
        rgb = frame_quality((2, 2, 3, bytearray([0, 0, 0, 40, 20, 10, 80, 60, 30, 120, 90, 45])))
        self.assertEqual(rgb["status"], "ok")
        with self.assertRaisesRegex(ValueError, "fully transparent"):
            frame_quality((2, 2, 4, bytearray([0, 0, 0, 0] * 4)))
        rgba = frame_quality((2, 2, 4, bytearray([80, 80, 80, 0, 80, 80, 80, 255,
                                                  80, 80, 80, 0, 80, 80, 80, 255])))
        self.assertTrue(rgba["visible_alpha_silhouette"])


if __name__ == "__main__":
    unittest.main()
