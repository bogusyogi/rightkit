//! the host app internal task pack for promotion-loop targeting.
//!
//! Some task shapes were inspired by Terminal-Bench (arXiv 2601.11868), but
//! this is not that benchmark, its task set, or its runner.

use serde::{Deserialize, Serialize};

/// Category of a the host app internal task-pack task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TbenchCategory {
    /// Multi-file refactoring / feature addition.
    CodeRefactor,
    /// Reproduce and fix a bug given failing tests.
    BugFix,
    /// Set up build system, environment, or CI.
    BuildSetup,
    /// Implement a complete algorithm from a spec.
    AlgorithmImpl,
    /// Optimize for performance (time/memory).
    PerformanceOpt,
    /// Write or complete a comprehensive test suite.
    TestSuiteWrite,
    /// Integrate two or more external libraries.
    LibIntegration,
    /// Understand and document unfamiliar code.
    CodeUnderstanding,
    /// Debug terminal/CLI tool failures.
    CliDebug,
    /// Data transformation pipeline.
    DataPipeline,
}

/// Difficulty tier of a the host app internal task-pack task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TbenchDifficulty {
    /// Typically solved by most frontier models.
    Easy,
    /// Requires multi-step reasoning; ~50-65% pass rate.
    Medium,
    /// Hard tasks where frontier models score <40%.
    Hard,
}

/// A single the host app internal task definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbenchTask {
    /// Unique identifier, e.g. "tb-001".
    pub id: String,
    /// Category classification.
    pub category: TbenchCategory,
    /// Difficulty tier.
    pub difficulty: TbenchDifficulty,
    /// Task prompt (what the agent is asked to do).
    pub prompt: String,
    /// Files to create in the workspace before running (name → content).
    pub setup_files: Vec<(String, String)>,
    /// Shell command to verify success (exit 0 = pass).
    pub verify_command: String,
    /// Expected passing score for a production-ready agent (0.0-1.0).
    pub expected_pass_rate: f64,
}

/// The canonical the host app internal task catalog for promotion targeting.
pub struct TbenchCatalog;

impl TbenchCatalog {
    /// Returns all tasks in the catalog.
    pub fn all_tasks() -> Vec<TbenchTask> {
        vec![
            // 10 representative tasks spanning all categories and difficulties.
            // Full 89-task set requires the tbench.ai runner.
            TbenchTask {
                id: "tb-001".into(),
                category: TbenchCategory::BugFix,
                difficulty: TbenchDifficulty::Easy,
                prompt: "The function `add(a, b)` in `math.py` returns `a - b` instead of `a + b`. Fix it and ensure the test `test_add.py` passes.".into(),
                setup_files: vec![
                    ("math.py".into(), "def add(a, b):\n    return a - b\n".into()),
                    ("test_add.py".into(), "from math import add\ndef test_add():\n    assert add(2, 3) == 5\n".into()),
                ],
                verify_command: "python -m pytest test_add.py -q".into(),
                expected_pass_rate: 0.95,
            },
            TbenchTask {
                id: "tb-002".into(),
                category: TbenchCategory::CodeRefactor,
                difficulty: TbenchDifficulty::Medium,
                prompt: "Refactor the `process_data()` function in `pipeline.py` to use a list comprehension instead of a for loop. All existing tests must still pass.".into(),
                setup_files: vec![
                    ("pipeline.py".into(), "def process_data(items):\n    result = []\n    for item in items:\n        result.append(item * 2)\n    return result\n".into()),
                    ("test_pipeline.py".into(), "from pipeline import process_data\ndef test_process():\n    assert process_data([1,2,3]) == [2,4,6]\n".into()),
                ],
                verify_command: "python -m pytest test_pipeline.py -q".into(),
                expected_pass_rate: 0.85,
            },
            TbenchTask {
                id: "tb-003".into(),
                category: TbenchCategory::AlgorithmImpl,
                difficulty: TbenchDifficulty::Hard,
                prompt: "Implement a correct, O(n log n) merge sort in `sort.rs`. The function signature must be `pub fn merge_sort(arr: &mut Vec<i32>)`. Tests in `sort_test.rs` must pass.".into(),
                setup_files: vec![
                    ("sort_test.rs".into(), "// tests that will be injected by the runner".into()),
                ],
                verify_command: "cargo test --test sort_test".into(),
                expected_pass_rate: 0.55,
            },
            TbenchTask {
                id: "tb-004".into(),
                category: TbenchCategory::BuildSetup,
                difficulty: TbenchDifficulty::Medium,
                prompt: "Create a `Makefile` with targets `build`, `test`, and `clean` for a C project with `main.c` and `utils.c`. `make test` must compile and run `test_main.c`.".into(),
                setup_files: vec![
                    ("main.c".into(), "int main() { return 0; }\n".into()),
                    ("utils.c".into(), "int add(int a, int b) { return a + b; }\n".into()),
                    ("test_main.c".into(), "#include <assert.h>\nextern int add(int,int);\nint main() { assert(add(1,2)==3); return 0; }\n".into()),
                ],
                verify_command: "make test".into(),
                expected_pass_rate: 0.60,
            },
            TbenchTask {
                id: "tb-005".into(),
                category: TbenchCategory::PerformanceOpt,
                difficulty: TbenchDifficulty::Hard,
                prompt: "The function `find_duplicates(arr)` in `dupes.py` runs in O(n²). Optimize it to O(n) using a hash set. Tests must still pass and runtime must be <10ms for n=100000.".into(),
                setup_files: vec![
                    ("dupes.py".into(), "def find_duplicates(arr):\n    dupes = []\n    for i in range(len(arr)):\n        for j in range(i+1, len(arr)):\n            if arr[i] == arr[j] and arr[i] not in dupes:\n                dupes.append(arr[i])\n    return sorted(dupes)\n".into()),
                    ("test_dupes.py".into(), "from dupes import find_duplicates\ndef test_basic():\n    assert find_duplicates([1,2,1,3,2]) == [1,2]\n".into()),
                ],
                verify_command: "python -m pytest test_dupes.py -q".into(),
                expected_pass_rate: 0.45,
            },
            TbenchTask {
                id: "tb-006".into(),
                category: TbenchCategory::TestSuiteWrite,
                difficulty: TbenchDifficulty::Medium,
                prompt: "Write a comprehensive pytest test suite for `calculator.py` that achieves >90% branch coverage. Include edge cases: division by zero, overflow, negative numbers, floats.".into(),
                setup_files: vec![
                    ("calculator.py".into(), "def add(a,b): return a+b\ndef sub(a,b): return a-b\ndef mul(a,b): return a*b\ndef div(a,b):\n    if b==0: raise ValueError('div by zero')\n    return a/b\n".into()),
                ],
                verify_command: "python -m pytest test_calculator.py --cov=calculator --cov-report=term-missing -q".into(),
                expected_pass_rate: 0.70,
            },
            TbenchTask {
                id: "tb-007".into(),
                category: TbenchCategory::LibIntegration,
                difficulty: TbenchDifficulty::Hard,
                prompt: "Integrate `requests` and `BeautifulSoup` to write a function `scrape_titles(url)` in `scraper.py` that returns a list of all `<h1>` texts from a webpage. Tests mock the HTTP call.".into(),
                setup_files: vec![
                    ("test_scraper.py".into(), "from unittest.mock import patch\nfrom scraper import scrape_titles\ndef test_scrape():\n    html = '<html><h1>Title 1</h1><h1>Title 2</h1></html>'\n    with patch('scraper.requests.get') as m:\n        m.return_value.text = html\n        assert scrape_titles('http://example.com') == ['Title 1', 'Title 2']\n".into()),
                ],
                verify_command: "python -m pytest test_scraper.py -q".into(),
                expected_pass_rate: 0.50,
            },
            TbenchTask {
                id: "tb-008".into(),
                category: TbenchCategory::CodeUnderstanding,
                difficulty: TbenchDifficulty::Easy,
                prompt: "Add a docstring to every public function in `utils.js` describing its parameters, return value, and one example. Functions: `formatDate`, `parseCSV`, `deepClone`.".into(),
                setup_files: vec![
                    ("utils.js".into(), "function formatDate(d) { return d.toISOString().split('T')[0]; }\nfunction parseCSV(s) { return s.split('\\n').map(l=>l.split(',')); }\nfunction deepClone(o) { return JSON.parse(JSON.stringify(o)); }\n".into()),
                    ("test_utils.js".into(), "const {formatDate,parseCSV,deepClone}=require('./utils');\nconst d=new Date('2026-01-01');\nconsole.assert(formatDate(d)==='2026-01-01');\n".into()),
                ],
                verify_command: "node test_utils.js && node -e \"const u=require('./utils'); if(!u.formatDate.toString().includes('/**')) process.exit(1)\"".into(),
                expected_pass_rate: 0.80,
            },
            TbenchTask {
                id: "tb-009".into(),
                category: TbenchCategory::CliDebug,
                difficulty: TbenchDifficulty::Medium,
                prompt: "The script `cli.sh` fails with 'command not found: jq'. Fix it to handle the case where `jq` is not installed by falling back to Python's `json.tool`. Tests verify both paths.".into(),
                setup_files: vec![
                    ("cli.sh".into(), "#!/bin/bash\necho '{\"key\":\"value\"}' | jq .\n".into()),
                    ("test_cli.sh".into(), "#!/bin/bash\nbash cli.sh | grep -q '\"key\"' && echo PASS || echo FAIL\n".into()),
                ],
                verify_command: "bash test_cli.sh | grep -q PASS".into(),
                expected_pass_rate: 0.65,
            },
            TbenchTask {
                id: "tb-010".into(),
                category: TbenchCategory::DataPipeline,
                difficulty: TbenchDifficulty::Hard,
                prompt: "Implement a streaming CSV-to-JSON converter in `converter.py` that handles files >1GB without loading them into memory. Output one JSON object per line (JSONL). Tests use a 10MB file.".into(),
                setup_files: vec![
                    ("gen_test_csv.py".into(), "import csv,io\nf=io.StringIO()\nw=csv.writer(f)\nw.writerow(['name','age','city'])\nfor i in range(100000):\n    w.writerow([f'user{i}',i%100,f'city{i%50}'])\nwith open('test.csv','w') as out:\n    out.write(f.getvalue())\n".into()),
                    ("test_converter.py".into(), "import subprocess,json,os\nos.system('python gen_test_csv.py')\nresult=subprocess.run(['python','converter.py','test.csv'],capture_output=True,text=True)\nlines=result.stdout.strip().split('\\n')\nassert len(lines)==100000\nassert json.loads(lines[0])['name']=='user0'\n".into()),
                ],
                verify_command: "python -m pytest test_converter.py -q".into(),
                expected_pass_rate: 0.35,
            },
        ]
    }

    /// Returns tasks filtered by difficulty.
    pub fn tasks_by_difficulty(difficulty: TbenchDifficulty) -> Vec<TbenchTask> {
        Self::all_tasks()
            .into_iter()
            .filter(|t| t.difficulty == difficulty)
            .collect()
    }

    /// Returns the benchmark suite identifier for stamping.
    pub fn suite_id() -> &'static str {
        "cr-internal-tbench-shaped@2026-06"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_all_difficulties() {
        let tasks = TbenchCatalog::all_tasks();
        assert!(!tasks.is_empty());
        assert!(tasks.iter().any(|t| t.difficulty == TbenchDifficulty::Hard));
        assert!(tasks
            .iter()
            .any(|t| t.difficulty == TbenchDifficulty::Medium));
        assert!(tasks.iter().any(|t| t.difficulty == TbenchDifficulty::Easy));
    }

    #[test]
    fn suite_id_is_stamped() {
        assert_eq!(
            TbenchCatalog::suite_id(),
            "cr-internal-tbench-shaped@2026-06"
        );
    }

    #[test]
    fn tasks_by_difficulty_filters_correctly() {
        let hard = TbenchCatalog::tasks_by_difficulty(TbenchDifficulty::Hard);
        assert!(!hard.is_empty());
        assert!(hard.iter().all(|t| t.difficulty == TbenchDifficulty::Hard));

        let easy = TbenchCatalog::tasks_by_difficulty(TbenchDifficulty::Easy);
        assert!(!easy.is_empty());
        assert!(easy.iter().all(|t| t.difficulty == TbenchDifficulty::Easy));
    }

    #[test]
    fn all_tasks_have_required_fields() {
        for task in TbenchCatalog::all_tasks() {
            assert!(!task.id.is_empty(), "task must have id");
            assert!(!task.prompt.is_empty(), "task must have prompt");
            assert!(
                !task.verify_command.is_empty(),
                "task must have verify_command"
            );
            assert!(task.expected_pass_rate >= 0.0 && task.expected_pass_rate <= 1.0);
        }
    }
}
