// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay airplay/RtspMessage.kt; strengthened length and header validation.
use std::collections::BTreeMap;
use thiserror::Error;

pub const MAX_HEADER: usize = 16 * 1024;
pub const MAX_BODY: usize = 2 * 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("RTSP message exceeds configured size limit")]
    TooLarge,
    #[error("invalid RTSP request line or header")]
    InvalidHeader,
    #[error("invalid or duplicate Content-Length")]
    InvalidLength,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub protocol: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_HEADER + MAX_BODY {
            return Err(Error::TooLarge);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub fn buffered_len(&self) -> usize {
        self.bytes.len()
    }
    pub fn take_buffer(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
    pub fn next_request(&mut self) -> Result<Option<Request>, Error> {
        let Some(end) = self.bytes.windows(4).position(|b| b == b"\r\n\r\n") else {
            return if self.bytes.len() > MAX_HEADER {
                Err(Error::TooLarge)
            } else {
                Ok(None)
            };
        };
        if end > MAX_HEADER {
            return Err(Error::TooLarge);
        }
        let text = std::str::from_utf8(&self.bytes[..end]).map_err(|_| Error::InvalidHeader)?;
        let mut lines = text.split("\r\n");
        let mut first = lines.next().ok_or(Error::InvalidHeader)?.splitn(3, ' ');
        let method = first
            .next()
            .filter(|x| {
                !x.is_empty()
                    && (x.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
                        || *x == "RTSP/1.0"
                        || *x == "HTTP/1.1")
            })
            .ok_or(Error::InvalidHeader)?;
        let path = first
            .next()
            .filter(|x| !x.is_empty() && !x.chars().any(char::is_control))
            .ok_or(Error::InvalidHeader)?;
        let is_response = method == "RTSP/1.0" || method == "HTTP/1.1";
        let protocol = first
            .next()
            .filter(|p| {
                (is_response && !p.is_empty() && !p.chars().any(char::is_control))
                    || *p == "RTSP/1.0"
                    || *p == "HTTP/1.1"
            })
            .ok_or(Error::InvalidHeader)?;
        if is_response && (path.len() != 3 || !path.bytes().all(|b| b.is_ascii_digit())) {
            return Err(Error::InvalidHeader);
        }
        if first.next().is_some() {
            return Err(Error::InvalidHeader);
        }
        let mut headers = BTreeMap::new();
        for line in lines {
            let (key, value) = line.split_once(':').ok_or(Error::InvalidHeader)?;
            if key.is_empty()
                || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || value.chars().any(|c| c.is_control() && c != '\t')
            {
                return Err(Error::InvalidHeader);
            }
            let key = key.to_ascii_lowercase();
            if headers
                .insert(key.clone(), value.trim().to_owned())
                .is_some()
            {
                return Err(if key == "content-length" {
                    Error::InvalidLength
                } else {
                    Error::InvalidHeader
                });
            }
        }
        // Chunked framing is not part of this protocol. Do not ambiguously interpret it.
        if headers.contains_key("transfer-encoding") {
            return Err(Error::InvalidHeader);
        }
        let length = match headers.get("content-length") {
            Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                n.parse::<usize>().map_err(|_| Error::InvalidLength)?
            }
            Some(_) => return Err(Error::InvalidLength),
            None => 0,
        };
        if length > MAX_BODY {
            return Err(Error::TooLarge);
        }
        let total = end + 4 + length;
        if self.bytes.len() < total {
            return Ok(None);
        }
        let request = Request {
            method: method.into(),
            path: path.into(),
            protocol: protocol.into(),
            headers,
            body: self.bytes[end + 4..total].to_vec(),
        };
        self.bytes.drain(..total);
        Ok(Some(request))
    }
}

pub fn response(
    request: &Request,
    status: u16,
    content_type: Option<&str>,
    body: &[u8],
) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        453 => "Not Enough Bandwidth",
        455 => "Method Not Valid in This State",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let mut text = format!(
        "{} {status} {reason}\r\nContent-Length: {}\r\nServer: RustCarPlay/0.1\r\n",
        request.protocol,
        body.len()
    );
    if let Some(cseq) = request.headers.get("cseq") {
        text.push_str(&format!("CSeq: {cseq}\r\n"));
    }
    if let Some(kind) = content_type {
        text.push_str(&format!("Content-Type: {kind}\r\n"));
    }
    text.push_str("\r\n");
    let mut result = text.into_bytes();
    result.extend_from_slice(body);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_fragmented_and_pipelined_binary_bodies() {
        let wire = b"POST /pair-setup RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 3\r\n\r\n\0\xffxGET /info RTSP/1.0\r\nCSeq: 2\r\n\r\n";
        let mut decoder = Decoder::default();
        let mut messages = Vec::new();
        for b in wire {
            decoder.push(&[*b]).unwrap();
            while let Some(r) = decoder.next_request().unwrap() {
                messages.push(r);
            }
        }
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].body, b"\0\xffx");
        assert!(
            String::from_utf8(response(&messages[1], 200, None, &[]))
                .unwrap()
                .contains("CSeq: 2\r\n")
        );
    }
    #[test]
    fn rejects_ambiguous_and_unbounded_frames() {
        for head in [
            "Content-Length: -1",
            "Content-Length: 3\r\ncontent-length: 3",
            "Content-Length: 999999999",
            "Transfer-Encoding: chunked",
        ] {
            let mut d = Decoder::default();
            d.push(format!("POST / RTSP/1.0\r\n{head}\r\n\r\n").as_bytes())
                .unwrap();
            assert!(d.next_request().is_err(), "{head}");
        }
    }
    #[test]
    fn event_responses_keep_full_reason_phrase() {
        let mut d = Decoder::default();
        d.push(b"RTSP/1.0 455 Method Not Valid in This State\r\nCSeq: 1\r\n\r\n")
            .unwrap();
        let response = d.next_request().unwrap().unwrap();
        assert_eq!(response.path, "455");
        assert_eq!(response.protocol, "Method Not Valid in This State");
    }
}
