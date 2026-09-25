//! Runs against a real `tdjson` when `FLOX_TDJSON` points at one; skips otherwise.

use std::path::PathBuf;
use std::time::Duration;

use flox_td::client::{version, TdClient, TdParams};
use flox_td::ffi::TdJson;
use serde_json::Value;

fn tdjson() -> Option<PathBuf> {
    match std::env::var_os("FLOX_TDJSON") {
        Some(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => {
            eprintln!("FLOX_TDJSON is not set; skipping the live TDLib test");
            None
        }
    }
}

/// Waits for the next `updateAuthorizationState` and returns its state's `@type`.
// A test helper: a missing update fails the test.
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn next_state(rx: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<Value>>) -> String {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let u = rx.recv().await.unwrap();
            if u["@type"] == "updateAuthorizationState" {
                return u["authorization_state"]["@type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
            }
        }
    })
    .await
    .expect("no updateAuthorizationState within 20 s")
}

/// One test for the whole lifecycle: `td_receive` must only ever be called from one
/// thread, so a test binary may hold a single client.
#[tokio::test]
async fn live_client_starts_restarts_and_closes() {
    let Some(path) = tdjson() else { return };
    let lib = TdJson::load(&path).unwrap();
    assert_eq!(version(&lib).as_deref(), Some("1.8.67"));

    let dir = tempfile::tempdir().unwrap();
    let params = |api_id: i32, name: &str| TdParams {
        api_id,
        api_hash: "00000000000000000000000000000000".into(),
        db_dir: dir.path().join(name).join("db"),
        files_dir: dir.path().join(name).join("files"),
        device_model: "Test".into(),
        app_version: "1.0.0".into(),
    };
    let (client, mut rx) = TdClient::start_subscribed(lib, params(1, "a")).unwrap();

    let first = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let u = rx.recv().await.unwrap();
            if u["@type"] == "updateAuthorizationState" {
                return u;
            }
        }
    })
    .await
    .expect("no updateAuthorizationState within 5 s");
    assert_eq!(
        first["authorization_state"]["@type"].as_str(),
        Some("authorizationStateWaitTdlibParameters")
    );
    assert!(first.get("@client_id").is_none());

    // New credentials: the same handle closes the instance and starts another.
    let first_id = client.client_id();
    tokio::time::timeout(Duration::from_secs(30), client.restart_with(params(2, "b")))
        .await
        .expect("restart timed out")
        .unwrap();
    assert_ne!(client.client_id(), first_id);
    assert_eq!(client.params().api_id, 2);
    let mut seen = Vec::new();
    while seen.last().map(String::as_str) != Some("authorizationStateWaitTdlibParameters") {
        seen.push(next_state(&mut rx).await);
    }
    assert!(
        seen.iter().any(|s| s == "authorizationStateClosed"),
        "{seen:?}"
    );

    tokio::time::timeout(Duration::from_secs(20), client.close())
        .await
        .expect("close timed out")
        .unwrap();

    // Closing again is a no-op once the instance is closed.
    client.close().await.unwrap();
    let states: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|u| (*u).clone())
        .filter(|u| u["@type"] == "updateAuthorizationState")
        .collect();
    assert_eq!(
        states
            .last()
            .and_then(|u| u["authorization_state"]["@type"].as_str()),
        Some("authorizationStateClosed")
    );
}
