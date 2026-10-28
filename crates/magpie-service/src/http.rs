use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::GatewayLimits;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADER_FIELDS: usize = 64;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_BODY: &str = concat!(
    "{\"status\":\"ok\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\"}"
);
const MODELS_BODY: &str = "{\"object\":\"list\",\"data\":[]}";
const NOT_IMPLEMENTED_BODY: &str =
    "{\"error\":{\"code\":\"not_implemented\",\"message\":\"Gateway route is not implemented\"}}";

struct Request {
    method: Method,
    path: String,
    body_bytes: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    Get,
    Head,
    Other,
}

enum HeaderError {
    Invalid,
    TooLarge,
    Io(io::Error),
}

enum RequestError {
    BadRequest,
    ContentTooLarge,
    HeadersTooLarge,
}

pub(super) fn handle_connection(mut stream: TcpStream, limits: GatewayLimits) {
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));

    let request = {
        let mut reader = BufReader::with_capacity(4096, &mut stream);
        read_request(&mut reader, limits)
    };

    match request {
        Ok(Some(request)) => respond_to_request(&mut stream, request),
        Ok(None) => {}
        Err(RequestError::BadRequest) => write_response(
            &mut stream,
            400,
            "Bad Request",
            "{\"error\":\"bad_request\"}",
            false,
        ),
        Err(RequestError::ContentTooLarge) => write_response(
            &mut stream,
            413,
            "Content Too Large",
            "{\"error\":\"content_too_large\"}",
            false,
        ),
        Err(RequestError::HeadersTooLarge) => write_response(
            &mut stream,
            431,
            "Request Header Fields Too Large",
            "{\"error\":\"headers_too_large\"}",
            false,
        ),
    }
}

pub(super) fn write_overloaded(stream: &mut TcpStream) {
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    write_response(
        stream,
        503,
        "Service Unavailable",
        "{\"error\":\"gateway_busy\"}",
        false,
    );
}

fn read_request(
    reader: &mut impl BufRead,
    limits: GatewayLimits,
) -> Result<Option<Request>, RequestError> {
    let header = match read_header(reader) {
        Ok(Some(header)) => header,
        Ok(None) => return Ok(None),
        Err(HeaderError::Invalid) => return Err(RequestError::BadRequest),
        Err(HeaderError::TooLarge) => return Err(RequestError::HeadersTooLarge),
        Err(HeaderError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(RequestError::BadRequest);
        }
        Err(HeaderError::Io(_)) => return Err(RequestError::BadRequest),
    };

    let Some(end) = header.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
        return Err(RequestError::BadRequest);
    };
    let header = &header[..end];
    if !header.is_ascii() {
        return Err(RequestError::BadRequest);
    }
    let header = std::str::from_utf8(header).map_err(|_| RequestError::BadRequest)?;
    let mut lines = header.split("\r\n");
    let request_line = lines.next().ok_or(RequestError::BadRequest)?;
    let mut words = request_line.split_ascii_whitespace();
    let method = match words.next().ok_or(RequestError::BadRequest)? {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        _ => Method::Other,
    };
    let target = words.next().ok_or(RequestError::BadRequest)?;
    let protocol = words.next().ok_or(RequestError::BadRequest)?;
    if words.next().is_some()
        || !target.starts_with('/')
        || target.bytes().any(|byte| byte.is_ascii_control())
        || !matches!(protocol, "HTTP/1.0" | "HTTP/1.1")
    {
        return Err(RequestError::BadRequest);
    }

    let mut content_length = None;
    let mut host_count = 0;
    let mut field_count = 0;
    for line in lines {
        field_count += 1;
        if field_count > MAX_HEADER_FIELDS {
            return Err(RequestError::HeadersTooLarge);
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(RequestError::BadRequest);
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err(RequestError::BadRequest);
        }
        let value = value.trim_matches([' ', '\t']);
        if value
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(RequestError::BadRequest);
        }
        if name.eq_ignore_ascii_case("host") {
            host_count += 1;
            if value.is_empty() {
                return Err(RequestError::BadRequest);
            }
        } else if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(RequestError::BadRequest);
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| RequestError::BadRequest)?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(RequestError::BadRequest);
        }
    }
    if host_count > 1 || (protocol == "HTTP/1.1" && host_count != 1) {
        return Err(RequestError::BadRequest);
    }

    let body_bytes = content_length.unwrap_or(0);
    let per_request_limit = limits
        .request_body_bytes
        .min(limits.total_buffered_bytes / limits.active_requests.max(1));
    if body_bytes > per_request_limit {
        return Err(RequestError::ContentTooLarge);
    }

    Ok(Some(Request {
        method,
        path: target.split('?').next().unwrap_or(target).to_owned(),
        body_bytes,
    }))
}

fn read_header(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, HeaderError> {
    let mut header = Vec::with_capacity(1024);
    loop {
        let available = reader.fill_buf().map_err(HeaderError::Io)?;
        if available.is_empty() {
            return if header.is_empty() {
                Ok(None)
            } else {
                Err(HeaderError::Invalid)
            };
        }
        let old_len = header.len();
        let take = available.len().min(MAX_HEADER_BYTES - old_len);
        header.extend_from_slice(&available[..take]);
        let complete = header.windows(4).any(|bytes| bytes == b"\r\n\r\n");
        reader.consume(take);
        if complete {
            return Ok(Some(header));
        }
        if header.len() == MAX_HEADER_BYTES {
            return Err(HeaderError::TooLarge);
        }
    }
}

fn respond_to_request(stream: &mut TcpStream, request: Request) {
    let head_only = request.method == Method::Head;
    if request.method == Method::Other {
        write_response(
            stream,
            405,
            "Method Not Allowed",
            "{\"error\":\"method_not_allowed\"}",
            false,
        );
    } else if request.path == "/health" {
        if request.body_bytes > 0 {
            write_response(
                stream,
                400,
                "Bad Request",
                "{\"error\":\"body_not_allowed\"}",
                head_only,
            );
        } else {
            write_response(stream, 200, "OK", HEALTH_BODY, head_only);
        }
    } else if request.path == "/v1/models" {
        if request.body_bytes > 0 {
            write_response(
                stream,
                400,
                "Bad Request",
                "{\"error\":\"body_not_allowed\"}",
                head_only,
            );
        } else {
            write_response(stream, 200, "OK", MODELS_BODY, head_only);
        }
    } else if request.path.starts_with("/v1/") || request.path.starts_with("/v1beta/") {
        write_response(
            stream,
            501,
            "Not Implemented",
            NOT_IMPLEMENTED_BODY,
            head_only,
        );
    } else {
        write_response(
            stream,
            404,
            "Not Found",
            "{\"error\":\"not_found\"}",
            head_only,
        );
    }
}

fn write_response(stream: &mut TcpStream, status: u16, reason: &str, body: &str, head_only: bool) {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head_only {
        let _ = stream.write_all(body.as_bytes());
    }
    let _ = stream.flush();
}
