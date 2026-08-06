use maestro_redaction::{REDACTED, redact_json, redact_json_in_place, redact_text};
use serde_json::json;

#[test]
fn authorization_headers_and_standalone_schemes_preserve_context() {
    let input = concat!(
        "Authorization: Bearer fixture-bearer-token-123456\n",
        "proxy-authorization = Basic dXNlcjpwYXNz\n",
        "Authorization: Digest username=fixture,response=digest-secret\n",
        "challenge Bearer opaque_token_1234567890 accepted\n",
        "legacy Basic dXNlcjpwYXNz accepted",
    );

    assert_eq!(
        redact_text(input),
        concat!(
            "Authorization: Bearer [REDACTED]\n",
            "proxy-authorization = Basic [REDACTED]\n",
            "Authorization: [REDACTED]\n",
            "challenge Bearer [REDACTED] accepted\n",
            "legacy Basic [REDACTED] accepted",
        )
    );
}

#[test]
fn common_assignments_keep_keys_delimiters_and_quotes() {
    let input = concat!(
        "OPENAI_API_KEY=sk-fixture_12345678901234567890 ",
        "password: 'correct horse battery staple' ",
        "clientSecret=client-fixture-secret ",
        "refresh_token=refresh-fixture-123; ",
        "--passphrase=fixture-passphrase",
    );

    assert_eq!(
        redact_text(input),
        concat!(
            "OPENAI_API_KEY=[REDACTED] ",
            "password: '[REDACTED]' ",
            "clientSecret=[REDACTED] ",
            "refresh_token=[REDACTED]; ",
            "--passphrase=[REDACTED]",
        )
    );
}

#[test]
fn jwt_and_high_confidence_seed_formats_are_redacted_without_labels() {
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmaXh0dXJlIn0.c2lnbmF0dXJlMTIzNDU2";
    let input = format!(
        "jwt={jwt} raw sk-fixture_12345678901234567890 ghp_fixture12345678901234567890 xoxb-1234567890-fixturetoken AKIAIOSFODNN7EXAMPLE"
    );
    let output = redact_text(&input);

    assert_eq!(output.matches(REDACTED).count(), 5);
    assert!(!output.contains(jwt));
    assert!(!output.contains("sk-fixture"));
    assert!(!output.contains("ghp_fixture"));
    assert!(!output.contains("xoxb-"));
    assert!(!output.contains("AKIA"));
}

#[test]
fn url_query_redaction_preserves_non_secret_parameters_and_fragment() {
    let input = concat!(
        "GET https://example.test/run?page=2&access_token=url-secret-123",
        "&mode=fast&sig=signature-fixture#result",
    );

    assert_eq!(
        redact_text(input),
        concat!(
            "GET https://example.test/run?page=2&access_token=[REDACTED]",
            "&mode=fast&sig=[REDACTED]#result",
        )
    );
}

#[test]
fn structured_json_redacts_sensitive_keys_recursively_and_text_leaves() {
    let input = json!({
        "agent": "codex",
        "headers": {
            "Authorization": "Bearer nested-fixture-token-123456",
            "Content-Type": "application/json"
        },
        "request": {
            "apiKey": "structured-secret",
            "password": 123_456,
            "optionalToken": null,
            "metadata": {
                "callback": "https://example.test/cb?token=query-secret&state=visible",
                "tokenCount": 7
            }
        },
        "events": [
            "safe event",
            "refresh_token=array-secret",
            { "client_secret": { "nested": "must-not-survive" } }
        ]
    });

    let output = redact_json(&input);
    assert_eq!(output["agent"], "codex");
    assert_eq!(output["headers"]["Authorization"], REDACTED);
    assert_eq!(output["headers"]["Content-Type"], "application/json");
    assert_eq!(output["request"]["apiKey"], REDACTED);
    assert_eq!(output["request"]["password"], REDACTED);
    assert!(output["request"]["optionalToken"].is_null());
    assert_eq!(
        output["request"]["metadata"]["callback"],
        "https://example.test/cb?token=[REDACTED]&state=visible"
    );
    assert_eq!(output["request"]["metadata"]["tokenCount"], 7);
    assert_eq!(output["events"][0], "safe event");
    assert_eq!(output["events"][1], "refresh_token=[REDACTED]");
    assert_eq!(output["events"][2]["client_secret"], REDACTED);
    assert_eq!(input["request"]["apiKey"], "structured-secret");
}

#[test]
fn in_place_api_counts_changed_leaves_and_is_idempotent() {
    let mut value = json!({
        "password": "one",
        "message": "token=two",
        "safe": "visible",
        "missing_secret": null
    });

    assert_eq!(redact_json_in_place(&mut value), 2);
    assert_eq!(redact_json_in_place(&mut value), 0);
    assert_eq!(value["password"], REDACTED);
    assert_eq!(value["message"], "token=[REDACTED]");
    assert_eq!(value["safe"], "visible");
    assert!(value["missing_secret"].is_null());
}

#[test]
fn false_positive_boundaries_remain_visible() {
    let input = concat!(
        "Basic authentication is enabled; Bearer token is documented. ",
        "tokenizer=v2 password_policy=required api_key_name=primary ",
        "secretary=Alex public_key=ssh-ed25519 client_id=desktop ",
        "version=1.2.3 https://example.test/?page=2&monkey=capuchin sk-short"
    );

    assert_eq!(redact_text(input), input);
}

#[test]
fn empty_assignments_and_schemes_do_not_consume_the_next_line() {
    let input = "api_key=\nvisible=true\nBearer\nstill visible";
    assert_eq!(redact_text(input), input);
}

#[test]
fn escaped_quoted_values_and_unicode_context_are_preserved_safely() {
    let input = "کاربر password=\"s3cr\\\"et value\" پایان api_key='کلید محرمانه'";
    assert_eq!(
        redact_text(input),
        "کاربر password=\"[REDACTED]\" پایان api_key='[REDACTED]'"
    );
}

#[test]
fn seeded_corpus_leaves_no_fixture_secret_material() {
    let secrets = [
        "fixture-auth-1234567890",
        "fixture-password-123",
        "fixture-query-secret",
        "eyJhbGciOiJIUzI1NiJ9.eyJzZWVkIjoiZml4dHVyZSJ9.c2lnbmF0dXJlMTIz",
    ];
    let input = format!(
        "Authorization: Bearer {}\npassword={}\nhttps://local/?api_key={}\n{}",
        secrets[0], secrets[1], secrets[2], secrets[3]
    );
    let output = redact_text(&input);

    for secret in secrets {
        assert!(!output.contains(secret));
    }
    assert_eq!(output.matches(REDACTED).count(), 4);
}
