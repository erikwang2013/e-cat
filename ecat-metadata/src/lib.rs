use std::collections::HashMap;

pub const TRACE_ID: &str = "x-ecat-trace-id";
pub const SERVICE_NAME: &str = "x-ecat-service";
pub const CLIENT_IP: &str = "x-ecat-client-ip";

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    inner: HashMap<String, String>,
}

impl Metadata {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(|v| v.as_str())
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.inner.insert(key.into(), value.into());
    }

    pub fn trace_id(&self) -> Option<&str> {
        self.get(TRACE_ID)
    }
}

// HTTP header -> Metadata
impl From<&http::HeaderMap> for Metadata {
    fn from(headers: &http::HeaderMap) -> Self {
        let mut m = Metadata::new();
        for (k, v) in headers.iter() {
            if let Ok(val) = v.to_str() {
                m.set(k.as_str(), val);
            }
        }
        m
    }
}

// gRPC metadata -> Metadata
impl From<&tonic::metadata::MetadataMap> for Metadata {
    fn from(map: &tonic::metadata::MetadataMap) -> Self {
        let mut m = Metadata::new();
        for entry in map.iter() {
            use tonic::metadata::KeyAndValueRef;
            match entry {
                KeyAndValueRef::Ascii(key, value) => {
                    if let Ok(val) = value.to_str() {
                        m.set(key.as_str(), val);
                    }
                }
                KeyAndValueRef::Binary(_, _) => {}
            }
        }
        m
    }
}

impl IntoIterator for Metadata {
    type Item = (String, String);
    type IntoIter = std::collections::hash_map::IntoIter<String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.into_iter()
    }
}
