use super::*;

#[path = "api_session_contract_components_01.rs"]
mod api_session_contract_components_01;
#[path = "api_session_contract_components_02.rs"]
mod api_session_contract_components_02;

pub(crate) fn session_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    api_session_contract_components_01::insert_session_components_01(&mut schemas);
    api_session_contract_components_02::insert_session_components_02(&mut schemas);
    schemas
}
