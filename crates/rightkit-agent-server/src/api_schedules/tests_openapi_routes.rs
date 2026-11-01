#[cfg(test)]
mod tests_openapi_routes {
    use super::tests_support::*;
    use super::*;
    #[test]
    fn schedule_openapi_helpers_cover_daemon_schedule_routes() {
        let components = schedules_openapi_components();
        let schedule_endpoints: Vec<_> = crate::api_contract::DAEMON_API_ENDPOINTS
            .iter()
            .filter(|endpoint| endpoint.path.starts_with("/schedules"))
            .collect();
        assert_eq!(schedule_endpoints.len(), 10);

        for endpoint in schedule_endpoints {
            let response_ref = ref_name(
                schedule_openapi_response_schema(endpoint.method, endpoint.path).unwrap_or_else(
                    || {
                        panic!(
                            "missing response helper for {} {}",
                            endpoint.method, endpoint.path
                        )
                    },
                ),
            );
            assert!(
                components.contains_key(&response_ref),
                "response helper for {} {} points at missing component {response_ref}",
                endpoint.method,
                endpoint.path
            );

            let request_schema = schedule_openapi_request_schema(endpoint.method, endpoint.path);
            match (endpoint.method, endpoint.path) {
                ("POST", "/schedules")
                | ("PATCH", "/schedules/{id}")
                | ("PUT", "/schedules/{id}/time-zone")
                | ("POST", "/schedules/{id}/run")
                | ("POST", "/schedules/{id}/pause")
                | ("POST", "/schedules/{id}/resume") => {
                    let request_ref = ref_name(request_schema.unwrap_or_else(|| {
                        panic!(
                            "missing request helper for {} {}",
                            endpoint.method, endpoint.path
                        )
                    }));
                    assert!(
                        components.contains_key(&request_ref),
                        "request helper for {} {} points at missing component {request_ref}",
                        endpoint.method,
                        endpoint.path
                    );
                }
                _ => assert!(
                    request_schema.is_none(),
                    "{} {} should not expose a request body schema",
                    endpoint.method,
                    endpoint.path
                ),
            }
        }
    }
}
