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
