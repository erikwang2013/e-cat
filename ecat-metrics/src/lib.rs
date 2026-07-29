// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use prometheus::{Encoder, Registry, TextEncoder};
use std::sync::OnceLock;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

pub fn metrics_text() -> String {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    encoder.encode(&registry().gather(), &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_singleton() {
        let r1 = registry() as *const Registry;
        let r2 = registry() as *const Registry;
        assert_eq!(r1, r2);
    }

    #[test]
    fn metrics_text_does_not_panic() {
        let text = metrics_text();
        // empty registry produces empty or minimal output — just check it's valid UTF-8
        let _ = text;
    }
}
