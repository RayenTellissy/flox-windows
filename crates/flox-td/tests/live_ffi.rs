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

#[tokio::test]
async fn live_client_starts_and_closes() {
    let Some(path) = tdjson() else { return };
    let lib = TdJson::load(&path).unwrap();
    assert_eq!(version(&lib).as_deref(), Some("1.8.67"));

    let dir = tempfile::tempdir().unwrap();
    let params = TdParams {
        api_id: 1,
        api_hash: "00000000000000000000000000000000".into(),
        db_dir: dir.path().join("db"),
        files_dir: dir.path().join("files"),
        device_model: "Test".into(),
        app_version: "1.0.0".into(),
    };
    let (client, mut rx) = TdClient::start_subscribed(lib, params).unwrap();

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
