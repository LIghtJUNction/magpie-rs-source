//! Metadata-only wire boundary for the local Usage command.
use std::collections::HashSet;

use serde::Deserialize;

const PROTOCOL_VERSION: &str = "0.2.0";

/// A static, credential-free report validation error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    InvalidReport,
    TooLarge,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidReport => "invalid usage report",
            Self::TooLarge => "usage report exceeds limit",
        })
    }
}
impl std::error::Error for ParseError {}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Report {
    pub version: String,
    pub scope: String,
    pub limit: usize,
    pub incomplete_records: usize,
    pub totals: Totals,
    pub records: Vec<Record>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Totals {
    pub requests: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub reported_requests: usize,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Record {
    pub schema: u8,
    pub id: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub provider_id: String,
    pub model: String,
    pub upstream_model: String,
    pub protocol: String,
    pub upstream_protocol: String,
    pub stream: bool,
    pub outcome: String,
    pub usage: Option<Tokens>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Tokens {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

/// A missing token field stays unknown, including when other fields are known.
pub fn token_sum<'a>(
    records: impl Iterator<Item = &'a Record>,
    field: fn(&Tokens) -> Option<u64>,
) -> Option<u64> {
    let mut total = None::<u64>;
    for record in records {
        if let Some(value) = record.usage.as_ref().and_then(field) {
            total = Some(total.unwrap_or(0).checked_add(value)?);
        }
    }
    total
}

pub fn parse(bytes: &[u8], requested_limit: usize) -> Result<Report, ParseError> {
    let invalid = || ParseError::InvalidReport;
    if bytes.len() > 1024 * 1024 {
        return Err(ParseError::TooLarge);
    }
    let report: Report = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if report.version != PROTOCOL_VERSION
        || report.scope != "recent_requests"
        || report.limit != requested_limit
        || !(1..=100).contains(&report.limit)
        || report.records.len() > report.limit
        || report.totals.requests != report.records.len()
        || report.totals.reported_requests > report.records.len()
    {
        return Err(invalid());
    }
    let mut ids = HashSet::new();
    for record in &report.records {
        let short_ascii = |value: &str, max: usize| {
            !value.is_empty() && value.len() <= max && value.bytes().all(|c| c.is_ascii_graphic())
        };
        if record.schema != 1
            || !short_ascii(&record.id, 256)
            || !ids.insert(&record.id)
            || !magpie_core::catalog::valid_provider_id(&record.provider_id)
            || !magpie_core::catalog::valid_model_id(&record.upstream_model)
            || record.model.is_empty()
            || record.model.len() > 4096
            || record.model.chars().any(char::is_control)
            || !short_ascii(&record.protocol, 64)
            || !short_ascii(&record.upstream_protocol, 64)
            || !matches!(
                record.outcome.as_str(),
                "completed" | "failed" | "cancelled"
            )
        {
            return Err(invalid());
        }
    }
    let count = |outcome| {
        report
            .records
            .iter()
            .filter(|r| r.outcome == outcome)
            .count()
    };
    if report.totals.completed != count("completed")
        || report.totals.failed != count("failed")
        || report.totals.cancelled != count("cancelled")
        || report.totals.input_tokens != token_sum(report.records.iter(), |t| t.input_tokens)
        || report.totals.output_tokens != token_sum(report.records.iter(), |t| t.output_tokens)
        || report.totals.total_tokens != token_sum(report.records.iter(), |t| t.total_tokens)
    {
        return Err(invalid());
    }
    Ok(report)
}

/// Validates host-loaded JSON using the same bounded wire contract as byte input.
pub fn parse_value(
    value: &serde_json::Value,
    requested_limit: usize,
) -> Result<Report, ParseError> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > (1024 * 1024usize).saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("usage report exceeds limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|_| ParseError::TooLarge)?;
    parse(&output.0, requested_limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> serde_json::Value {
        json!({"version":"0.2.0","scope":"recent_requests","limit":100,
        "incomplete_records":0,"totals":{"requests":2,"completed":1,"failed":1,
        "cancelled":0,"reported_requests":1,"input_tokens":0,"output_tokens":2,"total_tokens":2},
        "records":[
            {"schema":1,"id":"fixture-1","started_at_ms":1,"duration_ms":3,
                "provider_id":"fixture","model":"fixture/model-a","upstream_model":"model-a",
                "protocol":"openai_responses","upstream_protocol":"openai_responses",
                "stream":false,"outcome":"completed","usage":{"input_tokens":0,"output_tokens":2,"total_tokens":2}},
            {"schema":1,"id":"fixture-2","started_at_ms":2,"duration_ms":4,
                "provider_id":"fixture","model":"fixture/model-b","upstream_model":"model-b",
                "protocol":"openai_responses","upstream_protocol":"openai_responses",
                "stream":true,"outcome":"failed","usage":null}
        ]})
    }

    #[test]
    fn unknown_usage_does_not_turn_into_zero_or_disappear_from_request_counts() {
        let report = parse(&serde_json::to_vec(&fixture()).unwrap(), 100).unwrap();
        assert_eq!(report.totals.requests, 2);
        assert!(report.records[1].usage.is_none());
        assert_eq!(
            token_sum(report.records.iter().skip(1), |t| t.input_tokens),
            None
        );
        assert_eq!(
            token_sum(report.records.iter().take(1), |t| t.input_tokens),
            Some(0)
        );
    }

    #[test]
    fn inconsistent_counts_and_duplicate_request_ids_are_rejected() {
        let mut value = fixture();
        value["totals"]["failed"] = json!(0);
        assert!(parse(&serde_json::to_vec(&value).unwrap(), 100).is_err());
        let mut value = fixture();
        value["records"][1]["id"] = value["records"][0]["id"].clone();
        assert!(parse(&serde_json::to_vec(&value).unwrap(), 100).is_err());
    }
    #[test]
    fn value_and_bytes_have_the_same_contract_and_bounds() {
        let value = fixture();
        assert_eq!(
            parse_value(&value, 100).unwrap(),
            parse(&serde_json::to_vec(&value).unwrap(), 100).unwrap()
        );
        assert_eq!(parse_value(&value, 99), Err(ParseError::InvalidReport));
        let mut too_many = value.clone();
        too_many["limit"] = json!(101);
        assert_eq!(parse_value(&too_many, 101), Err(ParseError::InvalidReport));
        assert_eq!(
            parse(&vec![b' '; 1024 * 1024 + 1], 100),
            Err(ParseError::TooLarge)
        );
        assert_eq!(
            parse_value(&json!({"oversized":"x".repeat(1024 * 1024)}), 100),
            Err(ParseError::TooLarge)
        );
    }

    #[test]
    fn token_overflow_and_unknown_fields_never_become_zero() {
        let mut report = parse_value(&fixture(), 100).unwrap();
        report.records[0].usage = Some(Tokens {
            total_tokens: Some(u64::MAX),
            ..Tokens::default()
        });
        report.records[1].usage = Some(Tokens {
            total_tokens: Some(1),
            ..Tokens::default()
        });
        assert_eq!(token_sum(report.records.iter(), |t| t.total_tokens), None);
        assert_eq!(token_sum(report.records.iter(), |t| t.input_tokens), None);
    }
}
