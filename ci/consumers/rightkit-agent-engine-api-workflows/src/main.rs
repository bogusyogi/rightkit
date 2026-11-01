// rightkit-agent-engine-api-workflows 0.1.0 consumed by exact registry pin: OpenAPI document, stable
// error codes and the generic import request. No I/O.
use rightkit_agent_engine_api_workflows::error::code;
use rightkit_agent_engine_api_workflows::{openapi_v1_schema, ImportRequest};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let doc = openapi_v1_schema();
    check(
        doc.get("openapi").is_some() && doc.get("paths").is_some(),
        "OpenAPI document has openapi and paths",
    );
    check(
        code::NOT_FOUND == "workflows_not_found",
        "error codes are stable",
    );
    let req: ImportRequest = serde_json::from_value(serde_json::json!({
        "rawArtifactRef": "a", "package": {"any": 1}
    }))
    .expect("import request");
    check(
        req.raw_artifact_ref == "a" && req.package["any"] == 1,
        "ImportRequest keeps the package opaque",
    );
}
