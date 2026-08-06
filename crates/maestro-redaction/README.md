# maestro-redaction

`maestro-redaction` is Maestro's deterministic, fail-closed redaction layer for
text and structured JSON that may reach logs, error surfaces, event consoles,
exports, crash reports, or support bundles.

The crate recognizes:

- `Authorization` and `Proxy-Authorization` header values;
- standalone Bearer and Basic credentials with conservative boundaries;
- common API-key, token, password, passphrase, credential, and secret
  assignments;
- JWT-like values and high-confidence credential prefixes;
- secret-bearing URL query parameters; and
- sensitive JSON object keys at any supported nesting level.

It deliberately uses a bounded byte scanner rather than regular expressions.
The implementation makes a fixed number of linear passes, allocates at most in
proportion to its input, and has a JSON depth limit that replaces an excessively
deep subtree with `[REDACTED]` instead of risking unbounded recursion.

## APIs

```rust
use maestro_redaction::{redact_json, redact_text};
use serde_json::json;

assert_eq!(
    redact_text("Authorization: Bearer fixture-secret-token"),
    "Authorization: Bearer [REDACTED]",
);

let safe = redact_json(&json!({
    "request": { "apiKey": "fixture-secret", "model": "local" }
}));
assert_eq!(safe["request"]["apiKey"], "[REDACTED]");
assert_eq!(safe["request"]["model"], "local");
```

Callers should redact at the trust boundary, before persistence or
presentation. This crate complements typed secret wrappers and structured
logging; it is not permission to log raw process environments, database keys,
passphrases, or vendor credentials.

