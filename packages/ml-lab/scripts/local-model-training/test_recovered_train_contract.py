import importlib.util, json, tempfile, unittest
from pathlib import Path

ROOT = Path(__file__).parent

class TestRecoveredTrainContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.recipes = json.loads((ROOT / "model-recipes.json").read_text(encoding="utf-8"))

    def test_all_families_expose_the_executable_recipe_schema(self):
        for family in ("falcon", "granite", "gemma", "sotto"):
            item, common = self.recipes["families"][family], self.recipes["common"]
            self.assertTrue(item["source_id"])
            self.assertEqual(len(item["revision"]["value"]), 40)
            for key in ("full_parameter_learning_rate", "full_parameter_epochs"):
                self.assertIn(key, item["training"])
            self.assertEqual(common["max_seq_length"], 2048)

    def test_recipes_have_no_adapter_training_knobs(self):
        recipes = json.loads((ROOT / "model-recipes.json").read_text(encoding="utf-8"))
        for family in ("falcon", "granite", "gemma", "sotto"):
            self.assertFalse(any("lora" in key.lower() for key in recipes["families"][family]["training"]))

    def test_full_weight_backend_roster_preserves_unsloth_lanes_and_adds_native_granite(self):
        self.assertEqual(self.recipes["families"]["falcon"]["training_backend"]["priority"], ["unsloth_full"])
        self.assertEqual(self.recipes["families"]["gemma"]["training_backend"]["priority"], ["unsloth_full"])
        self.assertEqual(self.recipes["families"]["sotto"]["training_backend"]["priority"], ["unsloth_full"])
        granite = self.recipes["families"]["granite"]["training_backend"]
        self.assertEqual(granite["priority"], ["transformers_native_full", "unsloth_full"])
        self.assertEqual(granite["architecture"], "GraniteMoeHybridForCausalLM")
        self.assertNotIn("lora", json.dumps(granite).lower())

    def test_falcon_1_5b_candidates_are_pinned_without_changing_default_lanes(self):
        candidates = self.recipes["candidate_pool"]["falcon_h1_1_5b"]
        self.assertEqual([candidate["id"] for candidate in candidates], ["tiiuae/Falcon-H1-1.5B-Instruct", "tiiuae/Falcon-H1-1.5B-Deep-Instruct"])
        for candidate in candidates:
            self.assertRegex(candidate["revision"], r"^[a-f0-9]{40}$")
            self.assertEqual(candidate["training_mode"], "full_parameter_only")
        self.assertEqual(sorted(self.recipes["families"]), ["falcon", "gemma", "granite", "sotto"])

    def test_sotto_serialization_is_exact(self):
        self.assertEqual(self.recipes["families"]["sotto"]["serialization"]["fallback"].replace("{input}", "hello").replace("{output}", "world"), "### Input\nhello\n### Output\nworld")

    def test_gemma_capture_preflight_matches_family_sources(self):
        recipes = json.loads((ROOT / "model-recipes.json").read_text(encoding="utf-8"))
        preflight = recipes["gemma_preflight"]
        gemma = recipes["families"]["gemma"]
        self.assertEqual((preflight["stock_source_id"], preflight["stock_revision"]), (gemma["stock"]["id"], gemma["stock"]["revision"]))
        self.assertEqual((preflight["grmr_source_id"], preflight["grmr_revision"]), (gemma["grmr"]["id"], gemma["grmr"]["revision"]))

    def test_full_finetuning_loader_contract_is_bf16_and_unquantized(self):
        spec = importlib.util.spec_from_file_location("train_launcher", ROOT / "train.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        configured = module.full_finetuning_loader_kwargs({"load_in_4bit": False, "max_seq_length": 2048})
        self.assertEqual(configured["full_finetuning"], True)
        self.assertEqual(configured["load_in_4bit"], False)
        self.assertEqual(configured["load_in_8bit"], False)
        self.assertEqual(configured["use_gradient_checkpointing"], "unsloth")
        self.assertEqual(configured["dtype"], "bf16")

    def test_resolved_gemma_loader_path_is_receipt_bound_without_replacing_canonical_identity(self):
        spec = importlib.util.spec_from_file_location("train_launcher_resolution", ROOT / "train.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        revision = self.recipes["families"]["gemma"]["stock"]["revision"]
        source = self.recipes["families"]["gemma"]["stock"]["id"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); snapshot = root / revision; snapshot.mkdir()
            receipt = root / "resolution.json"
            receipt.write_text(json.dumps({"schema_version": "heardright.l1.exact_source_resolution.v1", "status": "passed", "entries": [{"source_id": source, "revision": revision, "resolved_path": str(snapshot)}]}), "utf-8")
            argv = ["train.py", "--action", "train", "--model-source", source, "--model-revision", revision, "--resolved-model-source", str(snapshot), "--resolved-source-receipt", str(receipt)]
            rewritten, canonical, resolved = module.consume_resolved_source_args(argv)
            self.assertEqual(module.argument_value(rewritten, "--model-source"), source)
            self.assertEqual(canonical, source)
            self.assertEqual(Path(resolved), snapshot.resolve())
            self.assertNotIn("--resolved-model-source", rewritten)

    def test_resume_wrapper_preserves_the_signed_manifest_and_supplies_a_concrete_checkpoint(self):
        spec = importlib.util.spec_from_file_location("train_launcher_resume", ROOT / "train.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); child = root / "checkpoint-94"; child.mkdir()
            (child / "trainer_state.json").write_text("{}", "utf-8")
            manifest = root / "train.manifest.json"
            original = {"action": "train", "checkpoint_dir": str(root / "checkpoint"), "checkpoint_dir_sha256": "sha256:stale"}
            manifest.write_text(json.dumps(original), "utf-8")
            argv, temporary = module.rewrite_resume_checkpoint_argument(["train.py", "--action", "train", "--resume-from-checkpoint", str(manifest)])
            rewritten = json.loads(temporary.read_text("utf-8"))
            self.assertEqual(Path(argv[-1]), temporary)
            self.assertEqual(Path(rewritten["checkpoint_dir"]), child)
            self.assertEqual(rewritten["checkpoint_dir_sha256"], module.checkpoint_tree_sha(child))
            self.assertEqual(json.loads(manifest.read_text("utf-8")), original)

if __name__ == "__main__": unittest.main()
