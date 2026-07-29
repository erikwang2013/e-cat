use super::{Codec, CodecError};

pub struct ProtoCodec;

impl Codec for ProtoCodec {
    fn encode<T: serde::Serialize>(&self, _val: &T) -> Result<Vec<u8>, CodecError> {
        Err(CodecError::Encode(
            "proto codec requires prost::Message trait".into(),
        ))
    }

    fn decode<T: serde::de::DeserializeOwned>(&self, _data: &[u8]) -> Result<T, CodecError> {
        Err(CodecError::Decode(
            "proto codec requires prost::Message trait".into(),
        ))
    }

    fn content_type(&self) -> &str {
        "application/protobuf"
    }
}
