//! Make custom app definition: one action module per engine method, posting to the app's HTTP
//! bridge. The shape follows the Make Apps SDK concepts (connection, base, modules with `api`
//! and `parameters`); it has not been imported into Make from here, so the first import is the
//! acceptance run for this renderer.

use crate::{
    camel_method, json_string, rpc_path, Contract, GenError, GeneratedFile, Operation, ParamType,
};

fn make_type(ty: ParamType) -> &'static str {
    match ty {
        ParamType::String | ParamType::Json => "text",
        ParamType::Number => "number",
        ParamType::Boolean => "boolean",
    }
}

fn module_source(op: &Operation) -> String {
    let mut params = String::new();
    let mut body = String::new();
    for (i, p) in op.params.iter().enumerate() {
        if i > 0 {
            params.push_str(",\n");
            body.push_str(",\n");
        }
        params.push_str(&format!(
            "        {{ \"name\": {}, \"type\": {}, \"label\": {}, \"required\": {}, \"help\": {} }}",
            json_string(&p.name),
            json_string(make_type(p.ty)),
            json_string(&p.name),
            p.required,
            json_string(&p.description),
        ));
        body.push_str(&format!(
            "          {}: {}",
            json_string(&p.name),
            json_string(&format!("{{{{parameters.{}}}}}", p.name)),
        ));
    }
    format!(
        "    {{\n      \"name\": {name},\n      \"label\": {label},\n      \"description\": {desc},\n      \"type\": \"action\",\n      \"connection\": \"connection\",\n      \"api\": {{\n        \"method\": \"POST\",\n        \"url\": {url},\n        \"body\": {{\n{body}\n        }},\n        \"response\": {{ \"output\": \"{{{{body}}}}\" }}\n      }},\n      \"parameters\": [\n{params}\n      ]\n    }}",
        name = json_string(&camel_method(&op.method)),
        label = json_string(&op.method),
        desc = json_string(&op.summary),
        url = json_string(&rpc_path(&op.method)),
        body = body,
        params = params,
    )
}

/// Render the app definition. Paths are relative to the app root.
pub fn render(contract: &Contract) -> Result<Vec<GeneratedFile>, GenError> {
    contract.validate()?;
    let modules: Vec<String> = contract.operations.iter().map(module_source).collect();
    let contents = format!(
        "{{\n  \"name\": {name},\n  \"label\": {label},\n  \"version\": {version},\n  \"connection\": {{\n    \"type\": \"apikey\",\n    \"parameters\": [\n      {{ \"name\": \"baseUrl\", \"type\": \"text\", \"label\": \"Base URL\", \"required\": true }},\n      {{ \"name\": \"token\", \"type\": \"password\", \"label\": \"Token\", \"required\": true }}\n    ]\n  }},\n  \"base\": {{\n    \"url\": \"{{{{connection.baseUrl}}}}\",\n    \"headers\": {{ \"Authorization\": \"Bearer {{{{connection.token}}}}\", \"Content-Type\": \"application/json\" }}\n  }},\n  \"modules\": [\n{modules}\n  ]\n}}\n",
        name = json_string(&contract.slug()),
        label = json_string(&contract.pascal_name()),
        version = json_string(&contract.version),
        modules = modules.join(",\n"),
    );
    Ok(vec![GeneratedFile {
        path: "make-app.json".into(),
        contents,
    }])
}
