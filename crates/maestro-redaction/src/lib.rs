//! Deterministic, fail-closed secret redaction for Maestro.
//!
//! The text scanner uses no regular expressions. It performs a fixed number of
//! bounded passes and records byte ranges before rendering one sanitized copy.
//! Structured JSON redaction additionally replaces values under sensitive keys
//! and applies text redaction to every non-sensitive string leaf.

#![forbid(unsafe_code)]

use std::{borrow::Cow, ops::Range};

use serde_json::Value;

/// Stable marker used whenever secret material has been removed.
pub const REDACTED: &str = "[REDACTED]";

/// Maximum JSON container depth inspected before the remaining subtree is
/// replaced wholesale. This bounds stack use for programmatically-built input.
pub const MAX_JSON_DEPTH: usize = 128;

const SENSITIVE_KEYS: &[&str] = &[
    "apikey",
    "apitoken",
    "auth",
    "authorization",
    "authtoken",
    "clientsecret",
    "credential",
    "credentials",
    "databasepassword",
    "dbpassword",
    "idtoken",
    "password",
    "passphrase",
    "passwd",
    "privatekey",
    "proxyauthorization",
    "pwd",
    "refreshtoken",
    "secret",
    "secretaccesskey",
    "sessiontoken",
    "signingkey",
    "token",
];

const SENSITIVE_SUFFIXES: &[&str] = &[
    "apikey",
    "apitoken",
    "authtoken",
    "clientsecret",
    "credential",
    "credentials",
    "idtoken",
    "password",
    "passphrase",
    "passwd",
    "privatekey",
    "refreshtoken",
    "secret",
    "secretaccesskey",
    "sessiontoken",
    "signingkey",
    "token",
];

const QUERY_ONLY_KEYS: &[&str] = &["key", "sig", "signature", "xamzcredential", "xamzsignature"];

/// Redacts secrets from arbitrary human-readable or protocol-derived text.
///
/// A borrowed value is returned when no secret is found. The scanner preserves
/// keys, schemes, delimiters, URLs, and other non-secret context.
#[must_use]
pub fn redact_text(input: &str) -> Cow<'_, str> {
    let ranges = secret_ranges(input);
    if ranges.is_empty() {
        Cow::Borrowed(input)
    } else {
        Cow::Owned(render_redacted(input, ranges))
    }
}

/// Returns a sanitized clone of a structured JSON value.
///
/// Sensitive object keys replace their entire non-null value. Other string
/// leaves are passed through [`redact_text`], including strings inside arrays.
#[must_use]
pub fn redact_json(value: &Value) -> Value {
    let mut redacted = value.clone();
    redact_json_in_place(&mut redacted);
    redacted
}

/// Redacts a structured JSON value in place and returns the number of values or
/// string leaves changed.
///
/// `null` under a sensitive key remains `null`, preserving the distinction
/// between absent material and removed secret material.
pub fn redact_json_in_place(value: &mut Value) -> usize {
    redact_json_value(value, 0)
}

fn redact_json_value(value: &mut Value, depth: usize) -> usize {
    if depth >= MAX_JSON_DEPTH {
        if matches!(value, Value::Array(values) if values.is_empty())
            || matches!(value, Value::Object(values) if values.is_empty())
        {
            return 0;
        }
        if matches!(value, Value::Array(_) | Value::Object(_)) {
            *value = Value::String(REDACTED.to_owned());
            return 1;
        }
    }

    match value {
        Value::Object(entries) => entries
            .iter_mut()
            .map(|(key, child)| {
                if is_sensitive_key(key) {
                    redact_named_value(child)
                } else {
                    redact_json_value(child, depth + 1)
                }
            })
            .sum(),
        Value::Array(values) => values
            .iter_mut()
            .map(|child| redact_json_value(child, depth + 1))
            .sum(),
        Value::String(text) => match redact_text(text) {
            Cow::Borrowed(_) => 0,
            Cow::Owned(redacted) => {
                *text = redacted;
                1
            }
        },
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

fn redact_named_value(value: &mut Value) -> usize {
    if value.is_null() || value.as_str() == Some(REDACTED) {
        return 0;
    }
    *value = Value::String(REDACTED.to_owned());
    1
}

fn secret_ranges(input: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    collect_assignment_ranges(input, &mut ranges);
    collect_scheme_ranges(input, &mut ranges);
    collect_jwt_ranges(input, &mut ranges);
    collect_prefixed_credential_ranges(input, &mut ranges);
    normalize_ranges(input, ranges)
}

fn collect_assignment_ranges(input: &str, ranges: &mut Vec<Range<usize>>) {
    let bytes = input.as_bytes();
    for (separator, byte) in bytes.iter().copied().enumerate() {
        if !matches!(byte, b'=' | b':') {
            continue;
        }
        let Some(key) = key_before_separator(input, separator) else {
            continue;
        };
        let normalized = normalize_key(&input[key.start..key.end]);
        let query_key = key.start > 0 && matches!(bytes[key.start - 1], b'?' | b'&' | b';');
        if !(is_sensitive_normalized_key(&normalized)
            || query_key && QUERY_ONLY_KEYS.contains(&normalized.as_str()))
        {
            continue;
        }

        let Some(value) = value_after_separator(input, separator) else {
            continue;
        };
        if matches!(normalized.as_str(), "authorization" | "proxyauthorization") {
            ranges.push(authorization_secret_range(input, value));
        } else {
            ranges.push(value);
        }
    }
}

fn collect_scheme_ranges(input: &str, ranges: &mut Vec<Range<usize>>) {
    for (scheme, kind) in [("bearer", Scheme::Bearer), ("basic", Scheme::Basic)] {
        let mut cursor = 0;
        while cursor + scheme.len() <= input.len() {
            let Some(relative) = find_ascii_case_insensitive(&input.as_bytes()[cursor..], scheme)
            else {
                break;
            };
            let start = cursor + relative;
            let after_scheme = start + scheme.len();
            if !is_word_boundary(input.as_bytes().get(start.wrapping_sub(1)).copied())
                || !input
                    .as_bytes()
                    .get(after_scheme)
                    .is_some_and(|byte| is_horizontal_whitespace(*byte))
            {
                cursor = after_scheme;
                continue;
            }
            let Some(candidate) =
                value_starting_at(input, skip_horizontal_whitespace(input, after_scheme))
            else {
                cursor = after_scheme;
                continue;
            };
            let value = &input[candidate.clone()];
            let looks_credential = match kind {
                Scheme::Bearer => looks_like_bearer(value),
                Scheme::Basic => looks_like_basic(value),
            };
            if looks_credential {
                ranges.push(candidate.clone());
            }
            cursor = candidate.end.max(after_scheme);
        }
    }
}

fn collect_jwt_ranges(input: &str, ranges: &mut Vec<Range<usize>>) {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if cursor + 3 <= bytes.len()
            && &bytes[cursor..cursor + 3] == b"eyJ"
            && is_token_boundary(bytes.get(cursor.wrapping_sub(1)).copied())
        {
            let mut end = cursor + 3;
            while end < bytes.len() && (is_base64_url(bytes[end]) || bytes[end] == b'.') {
                end += 1;
            }
            if is_jwt_like(&input[cursor..end]) && is_token_boundary(bytes.get(end).copied()) {
                ranges.push(cursor..end);
                cursor = end;
                continue;
            }
        }
        cursor += 1;
    }
}

fn collect_prefixed_credential_ranges(input: &str, ranges: &mut Vec<Range<usize>>) {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !is_prefixed_token_byte(bytes[cursor])
            || !is_prefixed_token_boundary(bytes.get(cursor.wrapping_sub(1)).copied())
        {
            cursor += 1;
            continue;
        }
        let mut end = cursor + 1;
        while end < bytes.len() && is_prefixed_token_byte(bytes[end]) {
            end += 1;
        }
        let candidate = &input[cursor..end];
        if looks_like_prefixed_credential(candidate) {
            ranges.push(cursor..end);
        }
        cursor = end;
    }
}

#[derive(Debug, Clone, Copy)]
enum Scheme {
    Bearer,
    Basic,
}

#[derive(Debug, Clone, Copy)]
struct KeyRange {
    start: usize,
    end: usize,
}

fn key_before_separator(input: &str, separator: usize) -> Option<KeyRange> {
    let bytes = input.as_bytes();
    let mut end = separator;
    while end > 0 && is_horizontal_whitespace(bytes[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }

    if matches!(bytes[end - 1], b'\'' | b'"') {
        let quote = bytes[end - 1];
        let closing = end - 1;
        let opening = previous_unescaped_quote(bytes, closing, quote)?;
        return (opening + 1 < closing).then_some(KeyRange {
            start: opening + 1,
            end: closing,
        });
    }

    let mut start = end;
    while start > 0 && is_key_byte(bytes[start - 1]) {
        start -= 1;
    }
    (start < end).then_some(KeyRange { start, end })
}

fn previous_unescaped_quote(bytes: &[u8], before: usize, quote: u8) -> Option<usize> {
    let mut cursor = before;
    while cursor > 0 {
        cursor -= 1;
        if bytes[cursor] == quote && !is_escaped(bytes, cursor) {
            return Some(cursor);
        }
    }
    None
}

fn value_after_separator(input: &str, separator: usize) -> Option<Range<usize>> {
    value_starting_at(input, skip_horizontal_whitespace(input, separator + 1))
}

fn value_starting_at(input: &str, start: usize) -> Option<Range<usize>> {
    let bytes = input.as_bytes();
    if start >= bytes.len() || input[start..].starts_with(REDACTED) {
        return None;
    }
    if matches!(bytes[start], b'\'' | b'"') {
        let quote = bytes[start];
        let value_start = start + 1;
        let mut end = value_start;
        while end < bytes.len() {
            if bytes[end] == quote && !is_escaped(bytes, end) {
                break;
            }
            end += 1;
        }
        return (value_start < end).then_some(value_start..end);
    }

    let mut end = start;
    while end < bytes.len() && !is_unquoted_value_delimiter(bytes[end]) {
        end += 1;
    }
    (start < end).then_some(start..end)
}

fn authorization_secret_range(input: &str, value: Range<usize>) -> Range<usize> {
    let bytes = input.as_bytes();
    let value_bytes = &bytes[value.clone()];
    for scheme in ["bearer", "basic"] {
        if value_bytes.eq_ignore_ascii_case(scheme.as_bytes()) {
            let start = skip_horizontal_whitespace(input, value.end);
            return value_starting_at(input, start).unwrap_or(value);
        }
        if value_bytes.len() <= scheme.len()
            || !value_bytes[..scheme.len()].eq_ignore_ascii_case(scheme.as_bytes())
            || !is_horizontal_whitespace(value_bytes[scheme.len()])
        {
            continue;
        }
        let start = skip_horizontal_whitespace(input, value.start + scheme.len());
        return value_starting_at(input, start).unwrap_or(value);
    }

    if input
        .as_bytes()
        .get(value.start.wrapping_sub(1))
        .is_some_and(|byte| matches!(byte, b'\'' | b'"'))
    {
        return value;
    }
    let end = input.as_bytes()[value.end..]
        .iter()
        .position(|byte| matches!(byte, b'\r' | b'\n'))
        .map_or(input.len(), |relative| value.end + relative);
    value.start..end
}

fn skip_horizontal_whitespace(input: &str, mut cursor: usize) -> usize {
    while input
        .as_bytes()
        .get(cursor)
        .is_some_and(|byte| is_horizontal_whitespace(*byte))
    {
        cursor += 1;
    }
    cursor
}

fn normalize_ranges(input: &str, mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.retain(|range| {
        range.start < range.end
            && input.get(range.clone()).is_some()
            && input.get(range.clone()) != Some(REDACTED)
    });
    ranges.sort_unstable_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn render_redacted(input: &str, ranges: Vec<Range<usize>>) -> String {
    let removed_bytes = ranges
        .iter()
        .map(|range| range.end - range.start)
        .sum::<usize>();
    let marker_bytes = ranges.len().saturating_mul(REDACTED.len());
    let mut output = String::with_capacity(
        input
            .len()
            .saturating_sub(removed_bytes)
            .saturating_add(marker_bytes),
    );
    let mut cursor = 0;
    for range in ranges {
        output.push_str(&input[cursor..range.start]);
        output.push_str(REDACTED);
        cursor = range.end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn normalize_key(key: &str) -> String {
    key.bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| char::from(byte.to_ascii_lowercase()))
        .collect()
}

fn is_sensitive_key(key: &str) -> bool {
    is_sensitive_normalized_key(&normalize_key(key))
}

fn is_sensitive_normalized_key(key: &str) -> bool {
    SENSITIVE_KEYS.contains(&key)
        || SENSITIVE_SUFFIXES
            .iter()
            .any(|suffix| key.ends_with(suffix))
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

fn is_jwt_like(candidate: &str) -> bool {
    let mut segments = candidate.split('.');
    let Some(header) = segments.next() else {
        return false;
    };
    let Some(payload) = segments.next() else {
        return false;
    };
    let Some(signature) = segments.next() else {
        return false;
    };
    segments.next().is_none()
        && header.starts_with("eyJ")
        && header.len() >= 8
        && payload.len() >= 8
        && signature.len() >= 8
        && [header, payload, signature]
            .iter()
            .all(|segment| segment.bytes().all(is_base64_url))
}

fn looks_like_bearer(candidate: &str) -> bool {
    candidate.len() >= 16
        || (candidate.len() >= 8
            && candidate
                .bytes()
                .any(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.' | b'=')))
}

fn looks_like_basic(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    bytes.len() >= 8
        && bytes.len().is_multiple_of(4)
        && bytes.iter().copied().all(is_base64_standard)
        && ((bytes.iter().any(u8::is_ascii_lowercase) && bytes.iter().any(u8::is_ascii_uppercase))
            || bytes
                .iter()
                .any(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'/' | b'=')))
}

fn looks_like_prefixed_credential(candidate: &str) -> bool {
    let github = ["ghp_", "gho_", "ghu_", "ghs_", "github_pat_"];
    (candidate.starts_with("sk-") && candidate.len() >= 20)
        || (matches!(candidate.get(..8), Some("sk_live_" | "sk_test_")) && candidate.len() >= 24)
        || (candidate.starts_with("rk_live_") && candidate.len() >= 24)
        || (github.iter().any(|prefix| candidate.starts_with(prefix)) && candidate.len() >= 20)
        || (candidate.starts_with("glpat-") && candidate.len() >= 20)
        || (candidate.starts_with("AIza") && candidate.len() >= 30)
        || (candidate.starts_with("xox")
            && candidate.as_bytes().get(3).is_some_and(|byte| {
                matches!(byte.to_ascii_lowercase(), b'a' | b'b' | b'p' | b'r' | b's')
            })
            && candidate.as_bytes().get(4) == Some(&b'-')
            && candidate.len() >= 20)
        || (matches!(candidate.get(..4), Some("AKIA" | "ASIA"))
            && candidate.len() == 20
            && candidate
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()))
}

fn is_escaped(bytes: &[u8], position: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = position;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn is_unquoted_value_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b',' | b';' | b'&' | b'#' | b'\'' | b'"' | b')' | b']' | b'}'
        )
}

fn is_horizontal_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

fn is_word_boundary(byte: Option<u8>) -> bool {
    byte.is_none_or(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-'))
}

fn is_token_boundary(byte: Option<u8>) -> bool {
    byte.is_none_or(|byte| !is_base64_url(byte) && byte != b'.')
}

fn is_prefixed_token_boundary(byte: Option<u8>) -> bool {
    byte.is_none_or(|byte| !is_prefixed_token_byte(byte))
}

fn is_base64_url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn is_base64_standard(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

fn is_prefixed_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use serde_json::json;

    use super::{MAX_JSON_DEPTH, REDACTED, redact_json_in_place, redact_text};

    #[test]
    fn unchanged_text_is_borrowed() {
        let input = "cargo test completed successfully";
        assert!(matches!(redact_text(input), Cow::Borrowed(value) if value == input));
    }

    #[test]
    fn excessive_json_depth_fails_closed_without_unbounded_recursion() {
        let mut value = json!({ "api_key": "deep-secret" });
        for _ in 0..(MAX_JSON_DEPTH + 32) {
            value = json!([value]);
        }

        assert_eq!(redact_json_in_place(&mut value), 1);
        let mut cursor = &value;
        for _ in 0..MAX_JSON_DEPTH {
            cursor = &cursor[0];
        }
        assert_eq!(cursor, REDACTED);
    }
}
