// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! HDFS path rules: validation and per-segment percent-encoding.
//!
//! HDFS has no buckets; `bucket` is a single namespace segment and `key` is a
//! relative sequence of segments. Rejecting dot segments here keeps `get` /
//! `put` / `delete` from escaping `/{bucket}` after percent-decoding on the
//! server.

use ecat_errors::{Error, ErrorCode};

fn invalid(message: String) -> Error {
    Error::new(ErrorCode::InvalidArgument, "hadoop", message)
}

/// Percent-encode a single URL path segment (RFC 3986): every byte except
/// unreserved characters (`A-Z a-z 0-9 - _ . ~`) becomes `%XX`.
pub(crate) fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `bucket` is a single namespace segment.
pub(crate) fn validate_bucket(bucket: &str) -> Result<(), Error> {
    if bucket.is_empty()
        || bucket == "."
        || bucket == ".."
        || bucket.contains('/')
        || bucket.contains('\\')
    {
        return Err(invalid(format!(
            "invalid hadoop bucket {bucket:?}: must be one non-empty path segment"
        )));
    }
    Ok(())
}

/// `key` is a relative sequence of non-empty, non-dot path segments.
pub(crate) fn validate_key(key: &str) -> Result<(), Error> {
    if key.is_empty() || key.starts_with('/') || key.contains('\\') {
        return Err(invalid(format!("invalid hadoop key {key:?}")));
    }
    for seg in key.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(invalid(format!(
                "invalid hadoop key {key:?}: empty or dot path segment"
            )));
        }
    }
    Ok(())
}

/// Prefixes are only used for filtering, but they still must not look like a
/// traversal attempt. A trailing slash is legitimate (`logs/`).
pub(crate) fn validate_prefix(prefix: &str) -> Result<(), Error> {
    if prefix.starts_with('/') || prefix.contains('\\') {
        return Err(invalid(format!("invalid hadoop prefix {prefix:?}")));
    }
    for seg in prefix.split('/') {
        if seg == "." || seg == ".." {
            return Err(invalid(format!(
                "invalid hadoop prefix {prefix:?}: dot path segment"
            )));
        }
    }
    Ok(())
}

/// A `pathSuffix` coming back from the server must stay a single child name;
/// otherwise a malicious/broken listing could walk outside `/{bucket}`.
pub(crate) fn valid_child_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

/// Percent-encode every segment of an already validated relative key.
pub(crate) fn encode_key_path(key: &str) -> String {
    key.split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_bucket_and_key_edges() {
        for bad in ["", ".", "..", "a/b", "a\\b"] {
            assert!(validate_bucket(bad).is_err(), "bucket {bad:?}");
        }
        for bad in ["", "/a", "a/", "a//b", ".", "..", "a/../b", "a\\b"] {
            assert!(validate_key(bad).is_err(), "key {bad:?}");
        }
        for good in ["a", "a/b", "a b/c#d", "你好/世界"] {
            assert!(validate_key(good).is_ok(), "key {good:?}");
        }
        assert!(validate_prefix("").is_ok());
        assert!(validate_prefix("a/").is_ok());
        assert!(validate_prefix("../a").is_err());
        assert!(validate_prefix("/a").is_err());
    }

    #[test]
    fn encodes_each_segment_once() {
        assert_eq!(percent_encode_segment("a b"), "a%20b");
        assert_eq!(encode_key_path("a b/c%2Fd"), "a%20b/c%252Fd");
        assert!(!encode_key_path("a/b").contains("%2520"));
    }
}
