use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const ALPN: &[u8] = b"staccato/1";
pub const DEFAULT_PORT: u16 = 1744;
pub const PROTOCOL_VERSION: u32 = 1;
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    #[serde(flatten)]
    pub message: Message,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        client_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    Welcome {
        server_name: String,
        library_revision: u64,
        pair_required: bool,
        fingerprint: String,
    },
    Pair {
        code: String,
    },
    Paired {
        token: String,
    },
    GetCatalog {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_revision: Option<u64>,
    },
    Rescan,
    Catalog {
        revision: u64,
        tracks: Vec<CatalogTrack>,
        #[serde(default)]
        removed: Vec<String>,
        /// When true the client should drop remote tracks not listed in `tracks`.
        #[serde(default)]
        replace: bool,
    },
    OpenMedia {
        remote_id: String,
        offset: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<u64>,
    },
    MediaHeader {
        size: u64,
        etag: String,
        codec: String,
    },
    Ping,
    Pong,
    LibraryChanged {
        revision: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogTrack {
    pub remote_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    pub duration_ms: u64,
    pub codec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    pub file_size: u64,
    pub modified_ns: i64,
}

impl Envelope {
    pub fn new(message: Message) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            message,
        }
    }
}

pub fn encode_frame(message: &Message) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(&Envelope::new(message.clone())).context("encoding message")?;
    if json.len() > MAX_MESSAGE_BYTES {
        bail!("message exceeds {} bytes", MAX_MESSAGE_BYTES);
    }
    let mut frame = Vec::with_capacity(4 + json.len());
    frame.extend_from_slice(&(json.len() as u32).to_be_bytes());
    frame.extend_from_slice(&json);
    Ok(frame)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn decode_frame(bytes: &[u8]) -> Result<Message> {
    if bytes.len() < 4 {
        bail!("truncated frame header");
    }
    let length = u32::from_be_bytes(bytes[..4].try_into().expect("header is 4 bytes")) as usize;
    if length > MAX_MESSAGE_BYTES {
        bail!("message exceeds {} bytes", MAX_MESSAGE_BYTES);
    }
    if bytes.len() < 4 + length {
        bail!("truncated frame body");
    }
    decode_json(&bytes[4..4 + length])
}

pub fn decode_json(bytes: &[u8]) -> Result<Message> {
    let envelope: Envelope = serde_json::from_slice(bytes).context("decoding message")?;
    if envelope.v != PROTOCOL_VERSION {
        bail!("unsupported protocol version {}", envelope.v);
    }
    Ok(envelope.message)
}

pub async fn write_message(send: &mut quinn::SendStream, message: &Message) -> Result<()> {
    let frame = encode_frame(message)?;
    send.write_all(&frame).await.context("writing message")?;
    Ok(())
}

pub async fn read_message(recv: &mut quinn::RecvStream) -> Result<Message> {
    let mut header = [0u8; 4];
    recv.read_exact(&mut header)
        .await
        .context("reading message length")?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_MESSAGE_BYTES {
        bail!("message exceeds {} bytes", MAX_MESSAGE_BYTES);
    }
    let mut body = vec![0u8; length];
    recv.read_exact(&mut body)
        .await
        .context("reading message body")?;
    decode_json(&body)
}

pub fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trips_as_length_prefixed_json() {
        let message = Message::Hello {
            client_name: "staccato".into(),
            token: Some("abc".into()),
        };
        let frame = encode_frame(&message).expect("encode");
        assert_eq!(
            u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        assert_eq!(decode_frame(&frame).expect("decode"), message);
        let json = std::str::from_utf8(&frame[4..]).unwrap();
        assert!(json.contains(r#""type":"hello""#));
        assert!(json.contains(r#""v":1"#));
    }

    #[test]
    fn unknown_type_is_a_decode_error() {
        let json = br#"{"v":1,"type":"not_a_verb"}"#;
        let mut frame = Vec::from((json.len() as u32).to_be_bytes());
        frame.extend_from_slice(json);
        assert!(decode_frame(&frame).is_err());
    }

    #[test]
    fn rescan_round_trips() {
        let frame = encode_frame(&Message::Rescan).expect("encode");
        assert_eq!(decode_frame(&frame).expect("decode"), Message::Rescan);
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let json = br#"{"v":99,"type":"ping"}"#;
        assert!(decode_json(json).is_err());
    }
}
