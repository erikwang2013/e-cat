mod json;
mod proto;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Json,
    Protobuf,
}

pub trait Codec: Send + Sync {
    fn encode<T: serde::Serialize>(&self, val: &T) -> Result<Vec<u8>, CodecError>;
    fn decode<T: serde::de::DeserializeOwned>(&self, data: &[u8]) -> Result<T, CodecError>;
    fn content_type(&self) -> &str;
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("encode error: {0}")]
    Encode(String),
    #[error("decode error: {0}")]
    Decode(String),
}

pub enum CodecBox {
    Json(json::JsonCodec),
    Proto(proto::ProtoCodec),
}

impl Codec for CodecBox {
    fn encode<T: serde::Serialize>(&self, val: &T) -> Result<Vec<u8>, CodecError> {
        match self {
            Self::Json(c) => c.encode(val),
            Self::Proto(c) => c.encode(val),
        }
    }

    fn decode<T: serde::de::DeserializeOwned>(&self, data: &[u8]) -> Result<T, CodecError> {
        match self {
            Self::Json(c) => c.decode(data),
            Self::Proto(c) => c.decode(data),
        }
    }

    fn content_type(&self) -> &str {
        match self {
            Self::Json(c) => c.content_type(),
            Self::Proto(c) => c.content_type(),
        }
    }
}

pub fn codec_for(encoding: Encoding) -> CodecBox {
    match encoding {
        Encoding::Json => CodecBox::Json(json::JsonCodec),
        Encoding::Protobuf => CodecBox::Proto(proto::ProtoCodec),
    }
}

pub fn codec_from_content_type(ct: &str) -> CodecBox {
    match ct {
        "application/json" => CodecBox::Json(json::JsonCodec),
        "application/protobuf" | "application/x-protobuf" => CodecBox::Proto(proto::ProtoCodec),
        _ => CodecBox::Json(json::JsonCodec),
    }
}
