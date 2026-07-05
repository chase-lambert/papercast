//! PaperCast custom-transport wire protocol: framing + message types, with no
//! I/O and no async. Everything encodes to a `Vec<u8>` and decodes from a
//! `&[u8]`, so this crate cross-compiles for the Android NDK (the receiver core
//! links it via JNI) exactly as cleanly as it builds for the host sender. The
//! async socket I/O lives in the host sender and the receiver core, never here.
//!
//! # Frame envelope
//!
//! Every message on the wire is `[u32 BE payload_len][u8 type][payload]`, where
//! `payload_len` counts only the payload bytes — not the type byte. Types below
//! 128 flow server→client; types ≥ 128 flow client→server.
//!
//! # Flow control
//!
//! The transport is pull-based: the server sends an [`Message::Update`] only
//! when a client [`Message::Ready`] is pending, and keeps only the newest frame
//! otherwise, so a slow EPD never builds a latency queue. That policy lives in
//! the host sender; this crate just defines the messages it uses.

use thiserror::Error;

/// Protocol version carried in the two hello messages.
pub const PROTO_VERSION: u8 = 1;

/// Upper bound on a single frame's payload, rejected by [`decode`] before it
/// allocates. Guards against a malformed/hostile length prefix; a legitimate
/// full-frame update compresses far below this.
pub const MAX_PAYLOAD: u32 = 64 * 1024 * 1024;

/// Wire message-type tags.
mod tag {
    pub const SERVER_HELLO: u8 = 1;
    pub const UPDATE: u8 = 2;
    pub const MODE_CHANGED: u8 = 3;
    pub const CLIENT_HELLO: u8 = 128;
    pub const READY: u8 = 129;
}

/// The zstd level used for per-rect Gray8 compression (spec: level 1 — fast,
/// the pixels are already dithered so entropy is high and higher levels buy
/// little).
const ZSTD_LEVEL: i32 = 1;

// --- message types ---

/// Server's opening message: the framebuffer geometry the client should expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerHello {
    pub proto_version: u8,
    pub width: u16,
    pub height: u16,
    pub levels: u8,
}

/// How the receiver should drive the EPD for an update. The receiver maps these
/// to concrete Onyx waveforms; the host only expresses intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshHint {
    /// Receiver decides (wire value 0).
    Auto,
    /// Fast, low-quality waveform (e.g. A2) — motion, writing (wire value 1).
    Fast,
    /// Full-quality waveform (e.g. GC16) — clean redraw / ghost clear (value 2).
    Quality,
}

impl RefreshHint {
    #[must_use]
    pub fn to_u8(self) -> u8 {
        match self {
            RefreshHint::Auto => 0,
            RefreshHint::Fast => 1,
            RefreshHint::Quality => 2,
        }
    }

    pub fn from_u8(v: u8) -> Result<Self, ProtoError> {
        match v {
            0 => Ok(RefreshHint::Auto),
            1 => Ok(RefreshHint::Fast),
            2 => Ok(RefreshHint::Quality),
            _ => Err(ProtoError::BadRefreshHint(v)),
        }
    }
}

/// A changed rectangle in framebuffer coordinates. `u16` because the RFB-era
/// framebuffer is already capped to `u16::MAX` per side upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// One rect plus its tightly-packed, row-major Gray8 pixels (`width * height`
/// bytes). Compression is a wire detail handled by [`encode`]/[`decode`]; in
/// memory these bytes are always raw Gray8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRect {
    pub rect: Rect,
    pub gray8: Vec<u8>,
}

/// A framebuffer update: a refresh intent plus the changed rects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub refresh_hint: RefreshHint,
    pub rects: Vec<UpdateRect>,
}

/// One decoded protocol message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    ServerHello(ServerHello),
    Update(Update),
    /// The active display mode changed; carries the new mode name.
    ModeChanged(String),
    ClientHello {
        proto_version: u8,
    },
    /// Client is ready for the next update (pull-based flow control).
    Ready,
}

/// A framing or decode error. Note that a *short* buffer is not an error —
/// [`decode`] returns `Ok(None)` for "need more bytes".
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("payload length {0} exceeds maximum {MAX_PAYLOAD}")]
    TooLarge(u32),
    #[error("invalid refresh hint {0}")]
    BadRefreshHint(u8),
    #[error("malformed {msg}: {detail}")]
    Malformed {
        msg: &'static str,
        detail: &'static str,
    },
    #[error("zstd decompression failed for a rect")]
    Decompress,
    #[error("mode name is not valid UTF-8")]
    Utf8,
}

// --- encoding ---

/// Serialize one message into a complete length-prefixed frame.
#[must_use]
pub fn encode(msg: &Message) -> Vec<u8> {
    let (msg_type, payload) = match msg {
        Message::ServerHello(h) => {
            let mut p = Vec::with_capacity(6);
            p.push(h.proto_version);
            p.extend_from_slice(&h.width.to_be_bytes());
            p.extend_from_slice(&h.height.to_be_bytes());
            p.push(h.levels);
            (tag::SERVER_HELLO, p)
        }
        Message::Update(u) => (tag::UPDATE, encode_update(u)),
        Message::ModeChanged(name) => {
            // name_len is a u8; mode names are short. If one somehow exceeds 255
            // bytes, truncate defensively — but to a char boundary, so we never
            // emit a split UTF-8 sequence the decoder would reject.
            let n = if name.len() <= u8::MAX as usize {
                name.len()
            } else {
                (0..=u8::MAX as usize).rev().find(|&i| name.is_char_boundary(i)).unwrap_or(0)
            };
            let mut p = Vec::with_capacity(1 + n);
            p.push(n as u8);
            p.extend_from_slice(&name.as_bytes()[..n]);
            (tag::MODE_CHANGED, p)
        }
        Message::ClientHello { proto_version } => (tag::CLIENT_HELLO, vec![*proto_version]),
        Message::Ready => (tag::READY, Vec::new()),
    };
    frame(msg_type, &payload)
}

fn encode_update(u: &Update) -> Vec<u8> {
    let n_rects = u.rects.len().min(u16::MAX as usize);
    let mut p = Vec::new();
    p.push(u.refresh_hint.to_u8());
    p.extend_from_slice(&(n_rects as u16).to_be_bytes());
    for r in u.rects.iter().take(n_rects) {
        // zstd level-1 compression of an in-memory slice does not fail; a
        // failure here would be a bug in our arguments, not runtime data.
        let comp = zstd::bulk::compress(&r.gray8, ZSTD_LEVEL)
            .expect("zstd level-1 compression of an in-memory buffer cannot fail");
        p.extend_from_slice(&r.rect.x.to_be_bytes());
        p.extend_from_slice(&r.rect.y.to_be_bytes());
        p.extend_from_slice(&r.rect.width.to_be_bytes());
        p.extend_from_slice(&r.rect.height.to_be_bytes());
        p.extend_from_slice(&(comp.len() as u32).to_be_bytes());
        p.extend_from_slice(&comp);
    }
    p
}

fn frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.push(msg_type);
    out.extend_from_slice(payload);
    out
}

// --- decoding ---

/// Try to parse one frame from the front of `buf`.
///
/// - `Ok(Some((msg, consumed)))` — parsed a message; `consumed` bytes should be
///   drained from `buf` before the next call.
/// - `Ok(None)` — `buf` does not yet hold a complete frame; read more and retry.
/// - `Err(_)` — the bytes are malformed.
pub fn decode(buf: &[u8]) -> Result<Option<(Message, usize)>, ProtoError> {
    const HEADER: usize = 5; // u32 len + u8 type
    if buf.len() < HEADER {
        return Ok(None);
    }
    let payload_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if payload_len > MAX_PAYLOAD {
        return Err(ProtoError::TooLarge(payload_len));
    }
    let total = HEADER + payload_len as usize;
    if buf.len() < total {
        return Ok(None);
    }
    let msg_type = buf[4];
    let payload = &buf[HEADER..total];
    let msg = decode_payload(msg_type, payload)?;
    Ok(Some((msg, total)))
}

fn decode_payload(msg_type: u8, payload: &[u8]) -> Result<Message, ProtoError> {
    match msg_type {
        tag::SERVER_HELLO => {
            let mut r = Reader::new(payload, "ServerHello");
            let hello = ServerHello {
                proto_version: r.u8()?,
                width: r.u16()?,
                height: r.u16()?,
                levels: r.u8()?,
            };
            r.finish()?;
            Ok(Message::ServerHello(hello))
        }
        tag::UPDATE => decode_update(payload),
        tag::MODE_CHANGED => {
            let mut r = Reader::new(payload, "ModeChanged");
            let n = r.u8()? as usize;
            let bytes = r.take(n)?;
            r.finish()?;
            let name = std::str::from_utf8(bytes).map_err(|_| ProtoError::Utf8)?;
            Ok(Message::ModeChanged(name.to_string()))
        }
        tag::CLIENT_HELLO => {
            let mut r = Reader::new(payload, "ClientHello");
            let proto_version = r.u8()?;
            r.finish()?;
            Ok(Message::ClientHello { proto_version })
        }
        tag::READY => {
            Reader::new(payload, "Ready").finish()?;
            Ok(Message::Ready)
        }
        other => Err(ProtoError::UnknownType(other)),
    }
}

fn decode_update(payload: &[u8]) -> Result<Message, ProtoError> {
    let mut r = Reader::new(payload, "Update");
    let refresh_hint = RefreshHint::from_u8(r.u8()?)?;
    let n_rects = r.u16()? as usize;
    let mut rects = Vec::with_capacity(n_rects.min(1024));
    for _ in 0..n_rects {
        let rect = Rect {
            x: r.u16()?,
            y: r.u16()?,
            width: r.u16()?,
            height: r.u16()?,
        };
        let comp_len = r.u32()? as usize;
        let comp = r.take(comp_len)?;
        let expected = rect.width as usize * rect.height as usize;
        if expected > MAX_PAYLOAD as usize {
            return Err(ProtoError::Malformed {
                msg: "Update",
                detail: "rect pixel count exceeds maximum",
            });
        }
        let gray8 = zstd::bulk::decompress(comp, expected).map_err(|_| ProtoError::Decompress)?;
        if gray8.len() != expected {
            return Err(ProtoError::Malformed {
                msg: "Update",
                detail: "decompressed rect size does not match width*height",
            });
        }
        rects.push(UpdateRect { rect, gray8 });
    }
    r.finish()?;
    Ok(Message::Update(Update { refresh_hint, rects }))
}

/// A cursor over a fully-received payload. Because [`decode`] only calls this
/// once the whole frame is present, running short here means the payload's
/// declared length disagrees with its contents — a malformed frame, not a
/// need-more-bytes.
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
    msg: &'static str,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8], msg: &'static str) -> Self {
        Self { b, pos: 0, msg }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtoError> {
        let end = self.pos.checked_add(n).ok_or(ProtoError::Malformed {
            msg: self.msg,
            detail: "length overflow",
        })?;
        if end > self.b.len() {
            return Err(ProtoError::Malformed { msg: self.msg, detail: "truncated payload" });
        }
        let out = &self.b[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ProtoError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, ProtoError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Assert the payload was fully consumed — trailing bytes mean the frame's
    /// length prefix disagrees with the message body.
    fn finish(&self) -> Result<(), ProtoError> {
        if self.pos != self.b.len() {
            return Err(ProtoError::Malformed { msg: self.msg, detail: "trailing payload bytes" });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: &Message) {
        let bytes = encode(msg);
        let (decoded, consumed) = decode(&bytes)
            .expect("decode ok")
            .expect("a full frame");
        assert_eq!(&decoded, msg, "round-trip mismatch");
        assert_eq!(consumed, bytes.len(), "consumed the whole frame");
    }

    #[test]
    fn roundtrip_server_hello() {
        roundtrip(&Message::ServerHello(ServerHello {
            proto_version: PROTO_VERSION,
            width: 3200,
            height: 2400,
            levels: 16,
        }));
    }

    #[test]
    fn roundtrip_client_hello_and_ready() {
        roundtrip(&Message::ClientHello { proto_version: PROTO_VERSION });
        roundtrip(&Message::Ready);
    }

    #[test]
    fn roundtrip_mode_changed() {
        roundtrip(&Message::ModeChanged("writing".to_string()));
        roundtrip(&Message::ModeChanged(String::new()));
    }

    #[test]
    fn roundtrip_update_all_hints() {
        for hint in [RefreshHint::Auto, RefreshHint::Fast, RefreshHint::Quality] {
            // Mix a highly-compressible rect with a noisy one so compression and
            // the length bookkeeping are both exercised.
            let flat = vec![160u8; 64 * 64];
            let noisy: Vec<u8> = (0..40u32 * 30).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
            let msg = Message::Update(Update {
                refresh_hint: hint,
                rects: vec![
                    UpdateRect { rect: Rect { x: 0, y: 0, width: 64, height: 64 }, gray8: flat },
                    UpdateRect { rect: Rect { x: 100, y: 200, width: 40, height: 30 }, gray8: noisy },
                ],
            });
            roundtrip(&msg);
        }
    }

    #[test]
    fn roundtrip_empty_update() {
        roundtrip(&Message::Update(Update { refresh_hint: RefreshHint::Auto, rects: vec![] }));
    }

    #[test]
    fn decode_needs_more_bytes() {
        let bytes = encode(&Message::ServerHello(ServerHello {
            proto_version: 1,
            width: 10,
            height: 20,
            levels: 16,
        }));
        // Every strict prefix is incomplete → Ok(None), never an error.
        for k in 0..bytes.len() {
            assert_eq!(decode(&bytes[..k]), Ok(None), "prefix len {k} should need more");
        }
        assert!(matches!(decode(&bytes), Ok(Some(_))));
    }

    #[test]
    fn decode_is_streaming() {
        // Two frames concatenated: decode drains exactly one at a time.
        let a = encode(&Message::Ready);
        let b = encode(&Message::ClientHello { proto_version: 1 });
        let mut buf = a.clone();
        buf.extend_from_slice(&b);

        let (m1, c1) = decode(&buf).unwrap().unwrap();
        assert_eq!(m1, Message::Ready);
        assert_eq!(c1, a.len());
        let (m2, c2) = decode(&buf[c1..]).unwrap().unwrap();
        assert_eq!(m2, Message::ClientHello { proto_version: 1 });
        assert_eq!(c2, b.len());
        assert_eq!(decode(&buf[c1 + c2..]), Ok(None));
    }

    #[test]
    fn decode_rejects_unknown_type() {
        // payload_len 0, type 200.
        let buf = [0, 0, 0, 0, 200];
        assert_eq!(decode(&buf), Err(ProtoError::UnknownType(200)));
    }

    #[test]
    fn decode_rejects_oversize_length() {
        let too_big = MAX_PAYLOAD + 1;
        let mut buf = too_big.to_be_bytes().to_vec();
        buf.push(tag::UPDATE);
        assert_eq!(decode(&buf), Err(ProtoError::TooLarge(too_big)));
    }

    #[test]
    fn decode_rejects_inconsistent_payload_length() {
        // Declare a 2-byte payload for a ServerHello, which needs 6. The frame
        // is "complete" by its own length, but the body runs short → Malformed.
        let mut buf = 2u32.to_be_bytes().to_vec();
        buf.push(tag::SERVER_HELLO);
        buf.extend_from_slice(&[1, 2]);
        assert!(matches!(decode(&buf), Err(ProtoError::Malformed { .. })));
    }

    #[test]
    fn decode_rejects_bad_refresh_hint() {
        // Update payload: hint=9 (invalid), n_rects=0.
        let payload = [9u8, 0, 0];
        let mut buf = (payload.len() as u32).to_be_bytes().to_vec();
        buf.push(tag::UPDATE);
        buf.extend_from_slice(&payload);
        assert_eq!(decode(&buf), Err(ProtoError::BadRefreshHint(9)));
    }

    #[test]
    fn decode_rejects_trailing_payload_bytes() {
        // Ready with a non-empty payload is malformed.
        let mut buf = 1u32.to_be_bytes().to_vec();
        buf.push(tag::READY);
        buf.push(0xff);
        assert!(matches!(decode(&buf), Err(ProtoError::Malformed { .. })));
    }
}
