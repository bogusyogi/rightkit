#[cfg(test)]
mod tests_support {
    use super::*;
    pub(super) fn ref_name(schema: Value) -> String {
        schema["$ref"]
            .as_str()
            .expect("schema helper should return a component ref")
            .trim_start_matches("#/components/schemas/")
            .to_string()
    }
}
