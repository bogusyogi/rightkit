mod tests {
    // Test-only: env/state guards intentionally held across `.await` to serialize
    // tests that mutate process env / shared state. Not a production deadlock path.
    #![allow(clippy::await_holding_lock, clippy::field_reassign_with_default)]
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::Request;
    use axum::routing::post;
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// Session rows persist fire-and-forget (`create_session_seeded_with_options`
    /// spawns the durable write), so a synchronous read right after create races
    /// the spawned task. Poll up to 1s for the row instead of a single read.
    async fn wait_for_session(
        storage: &std::sync::Arc<tokio::sync::Mutex<rightkit_agent_engine_storage::Storage>>,
        id: &str,
    ) -> rightkit_agent_engine_storage::SessionRow {
        for _ in 0..200 {
            {
                let store = storage.lock().await;
                if let Some(row) = store.get_session(id).unwrap() {
                    return row;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("session `{id}` was not persisted within 1s");
    }

    include!("api_tests_part_01.rs");
    include!("api_tests_part_02.rs");
    include!("api_tests_part_03.rs");
    include!("api_tests_part_04.rs");
    include!("api_tests_part_05.rs");
    include!("api_tests_part_06.rs");
    include!("api_tests_part_07.rs");
    include!("api_tests_part_08.rs");
    include!("api_tests_part_09.rs");
    include!("api_tests_part_10.rs");
    include!("api_tests_part_11.rs");
    include!("api_tests_part_12.rs");
    include!("api_tests_part_13.rs");
    include!("api_tests_part_14.rs");
    include!("api_tests_part_15.rs");
    include!("api_tests_part_16.rs");
    include!("api_tests_part_17.rs");
    include!("api_tests_part_18.rs");
    include!("api_tests_part_19.rs");
    include!("api_tests_part_20.rs");
    include!("api_tests_part_21.rs");
    include!("api_tests_part_22.rs");
    include!("api_tests_part_23.rs");
    include!("api_tests_part_24.rs");
    include!("api_tests_part_25.rs");
    include!("api_tests_part_26.rs");
    include!("api_tests_part_27.rs");
    include!("api_tests_part_28.rs");
    include!("api_tests_part_29.rs");
    include!("api_tests_part_30.rs");
    include!("api_tests_part_31.rs");
    include!("api_tests_part_32.rs");
    include!("api_tests_part_33.rs");
    include!("api_tests_part_34.rs");
    include!("api_tests_part_35.rs");
    include!("api_tests_part_36.rs");
    include!("api_tests_part_37.rs");
    include!("api_tests_part_38.rs");
}
