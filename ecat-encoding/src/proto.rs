// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Protobuf codec.
//!
//! The `Codec` trait methods (`encode`/`decode`) use serde bounds and always
//! return an error. Use `ProtoCodec::encode_message()` and
//! `ProtoCodec::decode_message()` for protobuf serialization — they accept
//! `prost::Message` types when the `prost-codec` feature is enabled.
//!
//! ```ignore
//! let codec = ProtoCodec;
//! let bytes = codec.encode_message(&my_proto_msg)?;
//! let msg: MyProto = codec.decode_message(&bytes)?;
//! ```
use super::{Codec, CodecError};

#[derive(Debug)]
/// Protobuf codec. Implements `Codec` for serde-compat, but the primary
/// API is `encode_message()` / `decode_message()` with `prost::Message`.
pub struct ProtoCodec;

impl ProtoCodec {
    /// Encode a protobuf message using prost.
    #[cfg(feature = "prost-codec")]
    pub fn encode_message<T: prost::Message>(&self, val: &T) -> Result<Vec<u8>, CodecError> {
        let mut buf = Vec::with_capacity(val.encoded_len());
        val.encode(&mut buf)
            .map_err(|e| CodecError::Encode(e.to_string()))?;
        Ok(buf)
    }

    /// Decode a protobuf message using prost.
    #[cfg(feature = "prost-codec")]
    pub fn decode_message<T: prost::Message + Default>(
        &self,
        data: &[u8],
    ) -> Result<T, CodecError> {
        T::decode(data).map_err(|e| CodecError::Decode(e.to_string()))
    }

    /// Encode using prost (stub without feature).
    #[cfg(not(feature = "prost-codec"))]
    pub fn encode_message<T>(&self, _val: &T) -> Result<Vec<u8>, CodecError> {
        Err(CodecError::Encode(
            "enable 'prost-codec' feature and implement prost::Message".into(),
        ))
    }

    /// Decode using prost (stub without feature).
    #[cfg(not(feature = "prost-codec"))]
    pub fn decode_message<T: Default>(&self, _data: &[u8]) -> Result<T, CodecError> {
        Err(CodecError::Decode(
            "enable 'prost-codec' feature and implement prost::Message".into(),
        ))
    }
}

impl Codec for ProtoCodec {
    fn encode<T: serde::Serialize>(&self, _val: &T) -> Result<Vec<u8>, CodecError> {
        Err(CodecError::Encode(
            "ProtoCodec: use encode_message() with prost::Message types, or enable 'prost-codec'"
                .into(),
        ))
    }

    fn decode<T: serde::de::DeserializeOwned>(&self, _data: &[u8]) -> Result<T, CodecError> {
        Err(CodecError::Decode(
            "ProtoCodec: use decode_message() with prost::Message types, or enable 'prost-codec'"
                .into(),
        ))
    }

    fn content_type(&self) -> &str {
        "application/protobuf"
    }
}
