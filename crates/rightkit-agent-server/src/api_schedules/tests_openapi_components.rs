#[cfg(test)]
mod tests_openapi_components {
    use super::*;

    #[test]
    fn schedules_openapi_components_cover_schedule_shapes() {
        let components = schedules_openapi_components();
        for name in [
            "ScheduleDeliveryTarget",
            "ScheduleRunResourceLimits",
            "ScheduleRunEvent",
            "ScheduleEntry",
            "ScheduleRow",
            "ListSchedulesResponse",
            "ScheduleAdminAuditEvent",
            "ScheduleAdminAuditResponse",
            "ScheduleRunsResponse",
            "CreateScheduleRequest",
            "PatchScheduleRequest",
            "UpdateScheduleTimeZoneRequest",
            "ScheduleActionRequest",
            "ScheduleMutationResponse",
            "DeleteScheduleResponse",
            "RunScheduleNowResponse",
        ] {
            assert!(components.contains_key(name), "missing {name}");
        }

        assert_eq!(
            components["ScheduleEntry"]["properties"]["delivery"]["items"]["$ref"],
            "#/components/schemas/ScheduleDeliveryTarget"
        );
        assert_eq!(
            components["ListSchedulesResponse"]["properties"]["schedules"]["items"]["$ref"],
            "#/components/schemas/ScheduleRow"
        );
        assert_eq!(
            components["ScheduleAdminAuditResponse"]["properties"]["history"]["items"]["$ref"],
            "#/components/schemas/ScheduleAdminAuditEvent"
        );
        assert_eq!(
            components["RunScheduleNowResponse"]["properties"]["latestRun"]["oneOf"][0]["$ref"],
            "#/components/schemas/ScheduleRunEvent"
        );
        let run_props = components["ScheduleRunEvent"]["properties"]
            .as_object()
            .expect("run event properties");
        for field in [
            "mode",
            "toolset",
            "executionBackend",
            "resolvedExecutionBackend",
            "executionWorkspaceId",
            "executionWorkspacePath",
            "resourceLimits",
            "workdir",
            "skills",
            "deliveryKinds",
            "sourceRecipeId",
        ] {
            assert!(run_props.contains_key(field), "missing run field {field}");
        }
        assert_eq!(
            run_props["resourceLimits"]["oneOf"][0]["$ref"],
            serde_json::json!("#/components/schemas/ScheduleRunResourceLimits")
        );
        assert_eq!(
            run_props["deliveryKinds"]["items"]["type"],
            serde_json::json!("string")
        );
    }
}
