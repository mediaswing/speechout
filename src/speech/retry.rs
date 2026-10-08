//! Deciding whether a failed cloud request is worth trying again, and how long
//! to wait first. This is kept free of networking so it can be tested, and it
//! is the same for every provider.

use std::fmt;
use std::time::Duration;

/// Tries at most this many times in all, so a job never gets stuck.
pub const MAX_ATTEMPTS: u32 = 4;

/// A provider asking us to wait longer than this is treated as a failure, so
/// the user is told instead of left waiting in silence.
pub const MAX_WAIT: Duration = Duration::from_secs(60);

/// The shortest wait, even if a provider sends "Retry-After: 0", so retries
/// never hammer a service that has just refused a request.
const MIN_WAIT: Duration = Duration::from_secs(1);

/// Waits used when the provider does not say how long to wait.
const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Too many requests (HTTP 429), but the account still has credit.
    RateLimited { retry_after: Option<Duration> },
    /// The service is overloaded or briefly broken (HTTP 408, 500, 502, 503, 504).
    Unavailable { retry_after: Option<Duration> },
    /// The request never reached the service (no connection, DNS failure or a
    /// connection timeout), so trying again cannot cost anything.
    Unreachable,
    /// The connection failed after the request was sent. The service may have
    /// done the work and charged for it, so this is retried only once.
    Interrupted,
    /// Retrying will not help: a rejected key, no credit, bad input and so on.
    Permanent,
}

/// A cloud service failure. `message` is shown to the user, so it must never
/// contain an API key.
#[derive(Debug)]
pub struct ServiceError {
    pub kind: Kind,
    pub message: String,
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ServiceError {}

/// The kind of failure behind an error, or `Permanent` for anything that did
/// not come from a cloud service (a system voice, say).
pub fn kind_of(error: &anyhow::Error) -> Kind {
    error.downcast_ref::<ServiceError>().map_or(Kind::Permanent, |e| e.kind)
}

/// Sorts an HTTP error status into a kind. `body` is the start of the error
/// response, used to tell an empty account apart from a busy one, because
/// some services use the same status for both.
pub fn classify(status: u16, body: &str, retry_after: Option<Duration>) -> Kind {
    if is_out_of_credit(body) {
        return Kind::Permanent;
    }
    match status {
        429 => Kind::RateLimited { retry_after },
        408 | 500 | 502 | 503 | 504 => Kind::Unavailable { retry_after },
        _ => Kind::Permanent,
    }
}

/// OpenAI reports an empty account as HTTP 429 with `insufficient_quota`;
/// ElevenLabs uses `quota_exceeded` (normally with HTTP 401).
pub fn is_out_of_credit(body: &str) -> bool {
    body.contains("insufficient_quota") || body.contains("quota_exceeded")
}

/// Reads a Retry-After header given in seconds. The HTTP date form is rare
/// for these services and falls back to the normal backoff.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// How long to wait before the next try, or `None` to give up. `failures` is
/// the number of tries that have failed so far, starting at 1.
pub fn next_delay(kind: Kind, failures: u32) -> Option<Duration> {
    if failures >= MAX_ATTEMPTS {
        return None;
    }
    let delay = match kind {
        Kind::RateLimited { retry_after } | Kind::Unavailable { retry_after } => {
            retry_after.unwrap_or_else(|| backoff(failures)).max(MIN_WAIT)
        }
        Kind::Unreachable => backoff(failures),
        Kind::Interrupted if failures == 1 => backoff(failures),
        Kind::Interrupted | Kind::Permanent => return None,
    };
    (delay <= MAX_WAIT).then_some(delay)
}

/// 2, 4, 8… seconds, up to a cap.
fn backoff(failures: u32) -> Duration {
    BACKOFF_BASE.saturating_mul(1 << failures.saturating_sub(1).min(16)).min(BACKOFF_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECS: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn sorts_statuses() {
        assert_eq!(classify(429, "", None), Kind::RateLimited { retry_after: None });
        for status in [408, 500, 502, 503, 504] {
            assert_eq!(classify(status, "", Some(SECS(3))), Kind::Unavailable { retry_after: Some(SECS(3)) });
        }
        for status in [400, 401, 402, 403, 404, 422, 501] {
            assert_eq!(classify(status, "", None), Kind::Permanent, "HTTP {status}");
        }
    }

    #[test]
    fn running_out_of_credit_is_never_retried() {
        let openai = r#"{"error":{"type":"insufficient_quota","code":"insufficient_quota"}}"#;
        assert_eq!(classify(429, openai, None), Kind::Permanent);
        let elevenlabs = r#"{"detail":{"status":"quota_exceeded"}}"#;
        assert_eq!(classify(401, elevenlabs, None), Kind::Permanent);
        assert_eq!(classify(429, elevenlabs, None), Kind::Permanent);
        // ElevenLabs' busy and concurrency responses are worth retrying.
        let busy = r#"{"detail":{"status":"system_busy"}}"#;
        assert_eq!(classify(429, busy, None), Kind::RateLimited { retry_after: None });
        let concurrent = r#"{"detail":{"status":"too_many_concurrent_requests"}}"#;
        assert_eq!(classify(429, concurrent, None), Kind::RateLimited { retry_after: None });
    }

    #[test]
    fn reads_retry_after_in_seconds() {
        assert_eq!(parse_retry_after("7"), Some(SECS(7)));
        assert_eq!(parse_retry_after(" 0 "), Some(SECS(0)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[test]
    fn backs_off_exponentially_up_to_a_cap() {
        let kind = Kind::RateLimited { retry_after: None };
        assert_eq!(next_delay(kind, 1), Some(SECS(2)));
        assert_eq!(next_delay(kind, 2), Some(SECS(4)));
        assert_eq!(next_delay(kind, 3), Some(SECS(8)));
        assert_eq!(backoff(10), BACKOFF_CAP);
        assert_eq!(backoff(u32::MAX), BACKOFF_CAP);
    }

    #[test]
    fn honours_retry_after() {
        let kind = Kind::Unavailable { retry_after: Some(SECS(5)) };
        assert_eq!(next_delay(kind, 1), Some(SECS(5)));
        assert_eq!(next_delay(kind, 3), Some(SECS(5)));
        let now = Kind::RateLimited { retry_after: Some(SECS(0)) };
        assert_eq!(next_delay(now, 1), Some(MIN_WAIT));
    }

    #[test]
    fn gives_up_on_a_very_long_wait() {
        let kind = Kind::RateLimited { retry_after: Some(MAX_WAIT + SECS(1)) };
        assert_eq!(next_delay(kind, 1), None);
        let kind = Kind::RateLimited { retry_after: Some(MAX_WAIT) };
        assert_eq!(next_delay(kind, 1), Some(MAX_WAIT));
    }

    #[test]
    fn retry_limit_is_respected() {
        for kind in [
            Kind::RateLimited { retry_after: None },
            Kind::Unavailable { retry_after: Some(SECS(1)) },
            Kind::Unreachable,
        ] {
            let tries = (1..).take_while(|&n| next_delay(kind, n).is_some()).count() as u32 + 1;
            assert_eq!(tries, MAX_ATTEMPTS, "{kind:?}");
        }
    }

    #[test]
    fn interrupted_requests_are_retried_once_and_permanent_never() {
        assert_eq!(next_delay(Kind::Interrupted, 1), Some(SECS(2)));
        assert_eq!(next_delay(Kind::Interrupted, 2), None);
        assert_eq!(next_delay(Kind::Permanent, 1), None);
    }

    #[test]
    fn finds_the_kind_through_context() {
        let e = anyhow::Error::from(std::io::Error::other("reset"))
            .context(ServiceError { kind: Kind::Unreachable, message: "could not reach it".into() });
        assert_eq!(kind_of(&e), Kind::Unreachable);
        assert_eq!(e.to_string(), "could not reach it");
        assert_eq!(kind_of(&anyhow::anyhow!("system voice failed")), Kind::Permanent);
    }
}
