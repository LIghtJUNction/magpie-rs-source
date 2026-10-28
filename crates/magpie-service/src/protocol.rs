//! Pure, credential-free protocol conversion with explicit compatibility failures.
//!
//! Native requests/responses are passed through, including provider extensions.
//! Cross-protocol conversion accepts only the fields documented in
//! `protocol/README.md`; an unrepresentable field is an error, never a deletion.
//! HTTP, cancellation of sockets, routing, and accounting belong to the caller.

#[path = "protocol/model.rs"]
mod model;
#[path = "protocol/request.rs"]
mod request;
#[path = "protocol/response.rs"]
mod response;
#[path = "protocol/stream.rs"]
mod stream;

pub use model::{
    ConversionError, ErrorCategory, ErrorKind, FinishReason, GatewayError, Limits, Outcome,
    Protocol, ResponseContext, Usage,
};
pub use request::convert_request;
pub use response::{ConvertedResponse, convert_response, error_body};
pub use stream::{SseConverter, StreamBatch, StreamEnd};

#[cfg(test)]
#[path = "protocol/tests.rs"]
mod tests;
