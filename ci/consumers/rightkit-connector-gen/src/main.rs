// rightkit-connector-gen 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Renders connector source text for an in-memory contract. The crate returns file contents and
// never writes them; this consumer writes nothing, and uses no network or process.
use rightkit_connector_gen::{
    make, n8n, rpc_path, zapier, Contract, GenError, GeneratedFile, Operation, Param, ParamType,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn contract() -> Contract {
    Contract {
        engine: "post-right".into(),
        version: "1.2.3".into(),
        operations: vec![
            Operation {
                method: "job.submit".into(),
                summary: "Submit a job".into(),
                params: vec![
                    Param {
                        name: "title".into(),
                        ty: ParamType::String,
                        required: true,
                        description: "Job title".into(),
                    },
                    Param {
                        name: "options".into(),
                        ty: ParamType::Json,
                        required: false,
                        description: "Extra options".into(),
                    },
                ],
            },
            Operation {
                method: "job.cancel".into(),
                summary: "Cancel a job".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: ParamType::Number,
                    required: true,
                    description: "Job id".into(),
                }],
            },
        ],
    }
}

fn file<'a>(files: &'a [GeneratedFile], path: &str) -> Option<&'a GeneratedFile> {
    files.iter().find(|f| f.path == path)
}

fn main() {
    let contract = contract();
    check("slug", contract.slug() == "post-right");
    check("pascal name", contract.pascal_name() == "PostRight");
    check("rpc path", rpc_path("job.submit") == "/rpc/job.submit");
    check("valid contract", contract.validate().is_ok());

    // n8n community node.
    let n8n_files = n8n::render(&contract).unwrap_or_else(|e| {
        eprintln!("FAIL: n8n render refused: {e}");
        std::process::exit(1);
    });
    check(
        "n8n file set",
        n8n_files.len() == 4
            && file(&n8n_files, "package.json").is_some()
            && file(&n8n_files, "tsconfig.json").is_some()
            && file(&n8n_files, "credentials/PostRightApi.credentials.ts").is_some()
            && file(&n8n_files, "nodes/PostRight/PostRight.node.ts").is_some(),
    );
    let n8n_package = file(&n8n_files, "package.json")
        .map(|f| f.contents.as_str())
        .unwrap_or("");
    check(
        "n8n package name and version",
        n8n_package.contains("\"n8n-nodes-post-right\"") && n8n_package.contains("\"1.2.3\""),
    );
    let n8n_node = file(&n8n_files, "nodes/PostRight/PostRight.node.ts")
        .map(|f| f.contents.as_str())
        .unwrap_or("");
    check(
        "n8n node posts to both methods",
        n8n_node.contains("\"/rpc/job.submit\"") && n8n_node.contains("\"/rpc/job.cancel\""),
    );

    // Zapier CLI app.
    let zapier_files = zapier::render(&contract).unwrap_or_else(|e| {
        eprintln!("FAIL: zapier render refused: {e}");
        std::process::exit(1);
    });
    let zapier_index = file(&zapier_files, "index.js")
        .map(|f| f.contents.as_str())
        .unwrap_or("");
    check(
        "zapier files",
        zapier_files.len() == 2
            && file(&zapier_files, "package.json").is_some()
            && zapier_index.contains("\"/rpc/job.submit\"")
            && zapier_index.contains("Bearer"),
    );

    // Make custom app.
    let make_files = make::render(&contract).unwrap_or_else(|e| {
        eprintln!("FAIL: make render refused: {e}");
        std::process::exit(1);
    });
    let make_app = file(&make_files, "make-app.json")
        .map(|f| f.contents.as_str())
        .unwrap_or("");
    check(
        "make app definition",
        make_files.len() == 1
            && make_app.contains("\"post-right\"")
            && make_app.contains("\"/rpc/job.cancel\""),
    );

    // Deterministic output.
    check(
        "rendering is deterministic",
        n8n::render(&contract) == Ok(n8n_files.clone())
            && zapier::render(&contract) == Ok(zapier_files.clone())
            && make::render(&contract) == Ok(make_files.clone()),
    );

    // Refusals happen before any text is produced.
    let mut no_letters = contract.clone();
    no_letters.engine = "--".into();
    check(
        "engine name refused",
        no_letters.validate() == Err(GenError::BadEngineName),
    );
    let mut bad_method = contract.clone();
    bad_method.operations[0].method = "job submit".into();
    check(
        "method name refused",
        n8n::render(&bad_method) == Err(GenError::BadMethodName("job submit".into())),
    );
    let mut duplicate = contract.clone();
    duplicate.operations[1].method = "job.submit".into();
    check(
        "duplicate method refused",
        zapier::render(&duplicate) == Err(GenError::DuplicateMethod("job.submit".into())),
    );
    let mut repeated = contract.clone();
    repeated.operations[0].params[1].name = "title".into();
    check(
        "repeated parameter refused",
        make::render(&repeated) == Err(GenError::BadParamName("title".into())),
    );

    println!("OK rightkit-connector-gen 0.1.0 (registry consumer, public API)");
}
