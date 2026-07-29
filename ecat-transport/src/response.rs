use ecat_metadata::Metadata;
use http::StatusCode;

pub struct Response<T = ()> {
    pub status: StatusCode,
    pub headers: http::HeaderMap,
    pub metadata: Metadata,
    pub body: T,
}

impl<T> Response<T> {
    pub fn new(body: T) -> Self {
        Self {
            status: StatusCode::OK,
            headers: http::HeaderMap::new(),
            metadata: Metadata::new(),
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_new_creates_with_defaults() {
        let resp: Response = Response::new(());
        assert_eq!(resp.status, StatusCode::OK);
    }

    #[test]
    fn response_with_body() {
        let resp = Response::new(42u32);
        assert_eq!(resp.body, 42);
    }

    #[test]
    fn response_with_string_body() {
        let resp = Response::new("hello".to_string());
        assert_eq!(resp.body, "hello");
    }
}
