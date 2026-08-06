use maestro_adapter::{
    ADAPTER_CONTRACT_VERSION, BoundedJsonLineDecoder, MAXIMUM_JSONL_FRAME_BYTES,
};
use serde_json::Value;

const MANIFEST: &str = include_str!("../../../fixtures/codex/app-server/0.146.0/manifest.json");
const CLIENT: &str = include_str!("../../../fixtures/codex/app-server/0.146.0/stable-client.jsonl");
const SERVER: &str =
    include_str!("../../../fixtures/codex/app-server/0.146.0/stable-server.sanitized.jsonl");

fn decode_fixture(input: &str) -> Vec<Value> {
    let mut decoder =
        BoundedJsonLineDecoder::new(MAXIMUM_JSONL_FRAME_BYTES).expect("bounded fixture decoder");
    let (frames, terminal_error) = decoder.push(input.as_bytes()).into_parts();
    assert_eq!(terminal_error, None, "valid JSONL fixture");
    decoder.finish().expect("fixture ends on a frame boundary");
    frames
}

#[test]
fn manifest_is_versioned_sanitized_and_non_consuming() {
    let manifest: Value = serde_json::from_str(MANIFEST).expect("valid fixture manifest");

    assert_eq!(
        manifest["adapterContractVersion"],
        Value::from(ADAPTER_CONTRACT_VERSION)
    );
    assert_eq!(manifest["cliVersion"], "0.146.0");
    assert_eq!(manifest["sanitized"], true);
    assert_eq!(manifest["providerTurnStarted"], false);
    assert_eq!(manifest["experimentalApiEnabled"], false);
    assert_eq!(manifest["maestroMaximumFrameBytes"], 1_048_576);
    assert!(
        manifest["vendorAcceptedFrameBytesAtLeast"]
            .as_u64()
            .is_some_and(|bytes| bytes > 1_048_576)
    );
}

#[test]
fn stable_transcript_preserves_correlation_and_out_of_order_responses() {
    let client = decode_fixture(CLIENT);
    let server = decode_fixture(SERVER);

    assert_eq!(client.len(), 5);
    assert_eq!(server.len(), 5);
    let server_order = server
        .iter()
        .map(|frame| {
            frame
                .get("id")
                .and_then(Value::as_u64)
                .map_or_else(|| "notification".to_owned(), |id| id.to_string())
        })
        .collect::<Vec<_>>();
    assert_eq!(server_order, ["1", "notification", "3", "2", "4"]);

    let client_ids = client
        .iter()
        .filter_map(|frame| frame.get("id").and_then(Value::as_u64))
        .collect::<Vec<_>>();
    let mut response_ids = server
        .iter()
        .filter_map(|frame| frame.get("id").and_then(Value::as_u64))
        .collect::<Vec<_>>();
    response_ids.sort_unstable();
    assert_eq!(client_ids, response_ids);
}

#[test]
fn checked_in_evidence_contains_no_real_local_metadata() {
    for forbidden in [
        "/Users/",
        "/home/",
        "C:\\Users\\",
        "rollout-",
        "originUrl",
        "git@",
        "access_token",
        "refresh_token",
        "Authorization:",
        "Bearer ",
        "sk-",
        "AIza",
        "xoxb-",
        "xoxp-",
        "ghp_",
        "github_pat_",
        "AKIA",
        "ASIA",
        "eyJhbGciOi",
        "-----BEGIN",
        "client_secret",
        "clientSecret",
        "@",
        ".com",
        ".net",
        ".org",
    ] {
        assert!(!MANIFEST.contains(forbidden));
        assert!(!CLIENT.contains(forbidden));
        assert!(!SERVER.contains(forbidden));
    }
}
