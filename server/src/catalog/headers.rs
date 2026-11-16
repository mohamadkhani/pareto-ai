//! Custom request headers on credentials and offerings: validation,
//! normalization, and the request-time merge.
//!
//! Header *values* are credential-equivalent secrets (API-key style headers),
//! so at rest they are sealed with the same AES-256-GCM [`SecretBox`] as the
//! credential secret, with the row id as associated data (decision D17).
//! `headers_json` stores the envelope
//! `{"names": ["x-api-key"], "values": "v1:<nonce>:<ct>"}`: the values
//! ciphertext seals the JSON object of name -> value, while the *names* stay
//! plain so lists and audit rows can show them without the key. Responses
//! carry names only ([`names`]) and nothing in this module ever logs a value.
//!
//! The merge ([`merge_headers`]) is a pure function — the offering's headers
//! override the credential's on key conflict — and M3 uses it when forwarding
//! a request upstream.

use std::collections::BTreeMap;

use crate::catalog::CatalogError;
use crate::secrets::SecretBox;

/// Header names an operator may not override: they are set per request by the
/// gateway or the HTTP stack, and a stored value would corrupt the upstream
/// request (or smuggle a second message).
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
];

const MAX_HEADER_NAME: usize = 128;
const MAX_HEADER_VALUE: usize = 4096;

/// Validates and normalizes a header map from a request body: names become
/// lowercase RFC 7230 tokens, values must be strings without CR/LF or other
/// control characters (header injection), and reserved headers are refused.
pub fn normalize(
    input: &BTreeMap<String, serde_json::Value>,
) -> Result<BTreeMap<String, String>, CatalogError> {
    let mut out = BTreeMap::new();
    for (raw_name, raw_value) in input {
        let name = raw_name.trim().to_ascii_lowercase();
        if name.is_empty() || name.len() > MAX_HEADER_NAME || !is_token(&name) {
            return Err(CatalogError::Invalid(format!(
                "header name {raw_name:?} is not a valid HTTP header name"
            )));
        }
        if RESERVED_HEADERS.contains(&name.as_str()) {
            return Err(CatalogError::Invalid(format!(
                "header {name} is set by the gateway and cannot be overridden"
            )));
        }
        let value = raw_value.as_str().ok_or_else(|| {
            CatalogError::Invalid(format!("header {raw_name:?} must have a string value"))
        })?;
        if value.len() > MAX_HEADER_VALUE {
            return Err(CatalogError::Invalid(format!(
                "header {raw_name:?} value is longer than {MAX_HEADER_VALUE} bytes"
            )));
        }
        if value.chars().any(|c| c.is_control()) {
            return Err(CatalogError::Invalid(format!(
                "header {raw_name:?} value contains control characters"
            )));
        }
        out.insert(name, value.to_string());
    }
    Ok(out)
}

/// Seals a normalized header map for `headers_json` under `row_id` as
/// associated data (D17), returning the `{"names", "values"}` envelope.
/// Fails only when the [`SecretBox`] cannot seal.
pub fn seal(
    map: &BTreeMap<String, String>,
    secrets: &SecretBox,
    row_id: &str,
) -> Result<String, CatalogError> {
    let values = secrets.encrypt(&values_json(map), row_id)?;
    let envelope = serde_json::json!({
        "names": map.keys().collect::<Vec<_>>(),
        "values": values,
    });
    Ok(envelope.to_string())
}

/// The sorted header names of stored `headers_json` — the only part a response
/// may carry, readable without the key (D17). Corrupt storage degrades to an
/// empty list instead of leaking a parse error or raw content into the
/// response.
pub fn names(raw: Option<&str>) -> Vec<String> {
    raw.map(parse_envelope)
        .and_then(Result::ok)
        .map(|(names, _values)| names)
        .unwrap_or_default()
}

/// Opens sealed `headers_json` written under `row_id` (the AEAD associated
/// data) back into a normalized map. `None` opens as an empty map.
///
/// Fails on corrupt storage, on a ciphertext that was moved to another row
/// or another key, and on decrypted content the validation rules would not
/// have let in at write time (corrupt rows).
pub fn open(
    raw: Option<&str>,
    secrets: &SecretBox,
    row_id: &str,
) -> Result<BTreeMap<String, String>, CatalogError> {
    let Some(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    let (_names, values) = parse_envelope(raw)?;
    let plaintext = secrets.decrypt(&values, row_id)?;
    parse_values(&plaintext)
}

/// Merges a credential's sealed headers with an offering's sealed headers
/// (the offering wins on key conflict). Each side is
/// `(headers_json, row id)`; the row id is the AEAD associated data (D17).
/// Either side may be absent.
///
/// Fails when either side is corrupt or does not decrypt. Keys are normalized
/// to lowercase, because HTTP header names are case-insensitive and
/// `Authorization` and `authorization` must conflict instead of both being
/// sent.
pub fn merge_headers(
    secrets: &SecretBox,
    credential: Option<(&str, &str)>,
    offering: Option<(&str, &str)>,
) -> Result<BTreeMap<String, String>, CatalogError> {
    let mut merged = match credential {
        Some((raw, row_id)) => open(Some(raw), secrets, row_id)?,
        None => BTreeMap::new(),
    };
    if let Some((raw, row_id)) = offering {
        for (name, value) in open(Some(raw), secrets, row_id)? {
            merged.insert(name, value);
        }
    }
    Ok(merged)
}

/// Parses the `{"names", "values"}` envelope, rejecting anything else as
/// corrupt storage. The names array is a plain projection for display; the
/// sealed values string is the authoritative half.
fn parse_envelope(raw: &str) -> Result<(Vec<String>, String), CatalogError> {
    let corrupt = || CatalogError::Invalid("stored headers are corrupt".to_string());
    let envelope: serde_json::Value = serde_json::from_str(raw).map_err(|_| corrupt())?;
    let names = envelope
        .get("names")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(corrupt)?
        .iter()
        .map(|name| name.as_str().map(str::to_owned).ok_or_else(corrupt))
        .collect::<Result<Vec<_>, _>>()?;
    let values = envelope
        .get("values")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(corrupt)?
        .to_string();
    Ok((names, values))
}

/// Parses decrypted header values into a normalized map, rejecting anything
/// the validation rules would not have let in at write time (corrupt rows).
fn parse_values(raw: &str) -> Result<BTreeMap<String, String>, CatalogError> {
    let map: BTreeMap<String, serde_json::Value> = serde_json::from_str(raw)
        .map_err(|_| CatalogError::Invalid("stored headers are corrupt".to_string()))?;
    normalize(&map)
}

/// Serializes a normalized header map for the sealed values payload.
fn values_json(map: &BTreeMap<String, String>) -> String {
    serde_json::Value::Object(
        map.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    )
    .to_string()
}

/// RFC 7230 `tchar`: what a header name may consist of.
fn is_token(name: &str) -> bool {
    name.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MasterKey;

    fn secrets() -> SecretBox {
        SecretBox::new(MasterKey::from_bytes([7u8; 32]))
    }

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect()
    }

    fn sealed(pairs: &[(&str, &str)], row_id: &str) -> String {
        let normalized = normalize(&headers(pairs)).expect("normalize");
        seal(&normalized, &secrets(), row_id).expect("seal")
    }

    #[test]
    fn merge_lets_the_offering_win_on_conflict() {
        let credential = sealed(&[("x-tenant", "team-a"), ("x-region", "eu")], "cred-1");
        let offering = sealed(&[("x-region", "us"), ("x-route", "fast")], "off-1");

        let merged = merge_headers(
            &secrets(),
            Some((&credential, "cred-1")),
            Some((&offering, "off-1")),
        )
        .expect("merge");
        assert_eq!(merged.get("x-tenant").map(String::as_str), Some("team-a"));
        assert_eq!(
            merged.get("x-region").map(String::as_str),
            Some("us"),
            "the offering overrides the credential"
        );
        assert_eq!(merged.get("x-route").map(String::as_str), Some("fast"));
    }

    #[test]
    fn merge_treats_header_names_case_insensitively() {
        // `Authorization` from the offering must replace `authorization` from
        // the credential instead of producing two headers.
        let credential = sealed(&[("Authorization", "old")], "cred-1");
        let offering = sealed(&[("authorization", "new")], "off-1");
        let merged = merge_headers(
            &secrets(),
            Some((&credential, "cred-1")),
            Some((&offering, "off-1")),
        )
        .expect("merge");
        assert_eq!(merged.len(), 1);
        assert_eq!(merged.get("authorization").map(String::as_str), Some("new"));
    }

    #[test]
    fn merge_accepts_absent_sides() {
        let merged = merge_headers(&secrets(), None, None).expect("empty merge");
        assert!(merged.is_empty());

        let only_credential = sealed(&[("a", "1")], "cred-1");
        let merged =
            merge_headers(&secrets(), Some((&only_credential, "cred-1")), None).expect("merge");
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn rejects_non_object_or_non_string_header_storage() {
        // Corrupt envelopes: not the {"names", "values"} shape at all.
        for bad in [
            r#"[]"#,
            r#""string""#,
            r#"{"a": 1}"#,
            r#"{"a": null}"#,
            "not json",
            r#"{"names": "x-api-key", "values": "v1:a:b"}"#,
            r#"{"names": ["x-api-key"]}"#,
            r#"{"names": [], "values": 5}"#,
        ] {
            assert!(
                merge_headers(&secrets(), Some((bad, "cred-1")), None).is_err(),
                "{bad} must not parse as headers"
            );
        }

        // A well-formed envelope whose sealed payload is not a name->value
        // object is corrupt too.
        let payload_corrupt = {
            let values = secrets().encrypt(r#"{"a": 1}"#, "cred-1").expect("seal");
            serde_json::json!({ "names": ["a"], "values": values }).to_string()
        };
        assert!(
            merge_headers(&secrets(), Some((&payload_corrupt, "cred-1")), None).is_err(),
            "non-string values must not decrypt into headers"
        );
    }

    #[test]
    fn sealed_values_decrypt_only_under_their_row_id() {
        // The row id is the AEAD associated data (D17): a ciphertext moved to
        // another row fails to open instead of leaking.
        let envelope = sealed(&[("x-api-key", "value")], "cred-1");
        assert!(open(Some(&envelope), &secrets(), "cred-2").is_err());
        assert!(open(Some(&envelope), &secrets(), "cred-1").is_ok());

        let foreign_key = SecretBox::new(MasterKey::from_bytes([8u8; 32]));
        assert!(open(Some(&envelope), &foreign_key, "cred-1").is_err());
    }

    #[test]
    fn rejects_header_injection_and_invalid_names() {
        // CR/LF in a value would smuggle a second header into the upstream
        // request once M3 forwards these.
        let injection = headers(&[("x-note", "fine\r\nx-evil: 1")]);
        assert!(normalize(&injection).is_err());

        let bad_name = headers(&[("x bad name", "v")]);
        assert!(normalize(&bad_name).is_err());

        let empty_name = headers(&[("", "v")]);
        assert!(normalize(&empty_name).is_err());

        let reserved = headers(&[("Host", "evil.example")]);
        assert!(normalize(&reserved).is_err());

        let non_string = BTreeMap::from([("x-count".to_string(), serde_json::json!(3))]);
        assert!(normalize(&non_string).is_err());

        let long_value = headers(&[("x-long", &"a".repeat(MAX_HEADER_VALUE + 1))]);
        assert!(normalize(&long_value).is_err());
    }

    #[test]
    fn names_returns_sorted_names_and_nothing_else() {
        let envelope = sealed(
            &[("x-zeta", "secret-value"), ("x-alpha", "other-secret")],
            "cred-1",
        );
        assert_eq!(names(Some(&envelope)), vec!["x-alpha", "x-zeta"]);
        assert_eq!(names(None), Vec::<String>::new());
        assert_eq!(names(Some("corrupt")), Vec::<String>::new());
        assert!(
            !envelope.contains("secret-value") && !envelope.contains("other-secret"),
            "the envelope carries names in the clear, never values"
        );
    }

    #[test]
    fn seal_open_round_trip_preserves_normalized_headers() {
        let normalized = normalize(&headers(&[("X-Original", "value")])).expect("normalize");
        let envelope = seal(&normalized, &secrets(), "cred-1").expect("seal");
        assert_eq!(
            open(Some(&envelope), &secrets(), "cred-1").expect("open"),
            normalized
        );
    }
}
