use ecat_metadata::Metadata;
use http::{HeaderMap, Method, Uri};
use std::collections::HashMap;

pub struct Request<T = ()> {
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub metadata: Metadata,
    pub body: T,
    pub params: HashMap<String, String>,
}

impl<T> Request<T> {
    pub fn new(body: T) -> Self {
        Self {
            method: Method::GET,
            uri: Uri::default(),
            headers: HeaderMap::new(),
            metadata: Metadata::new(),
            body,
            params: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_new_creates_with_defaults() {
        let req: Request = Request::new(());
        assert_eq!(req.method, Method::GET);
        assert!(req.params.is_empty());
    }

    #[test]
    fn request_with_body() {
        let req = Request::new(42u32);
        assert_eq!(req.body, 42);
    }

    #[test]
    fn request_with_string_body() {
        let req = Request::new("hello".to_string());
        assert_eq!(req.body, "hello");
    }
}
