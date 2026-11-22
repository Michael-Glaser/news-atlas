//! Retry a *transient* failure, once.
//!
//! The decision is narrow on purpose: only [`AiError::is_transient`] is retried —
//! a llama.cpp that is still loading its model refuses a connection for a moment,
//! and a second attempt either works or is a real failure. Everything else (a bad
//! request, a context overflow, a protocol error) is returned on the first attempt.
//!
//! It does not: back off (there is no sleep), open a socket, or decide the number
//! of retries — the caller passes [`crate::http1::DEFAULT_RETRIES`].

use crate::error::AiError;

/// Retry `attempt` while the failure is transient, at most `retries` times.
///
/// Split out from the socket code so the retry *policy* is testable without
/// network: an attempt closure that always fails transiently must be called
/// exactly `retries + 1` times.
pub fn retry_once<T>(
    retries: u8,
    mut attempt: impl FnMut() -> Result<T, AiError>,
) -> Result<T, AiError> {
    let mut left = retries;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if left == 0 || !error.is_transient() {
                    return Err(error);
                }
                left -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn retry_once_retries_a_transient_failure_once() {
        let mut attempts = 0;
        let result: Result<u8, AiError> = retry_once(1, || {
            attempts += 1;
            Err(AiError::Unreachable("refused".into()))
        });
        assert!(result.is_err());
        assert_eq!(attempts, 2, "the original attempt plus exactly one retry");
    }

    #[test]
    fn retry_once_does_not_retry_a_real_failure() {
        let mut attempts = 0;
        let result: Result<u8, AiError> = retry_once(1, || {
            attempts += 1;
            Err(AiError::Protocol("bad framing".into()))
        });
        assert!(result.is_err());
        assert_eq!(attempts, 1);

        let mut attempts = 0;
        let ok = retry_once(1, || {
            attempts += 1;
            if attempts == 1 {
                Err(AiError::Unreachable("still loading".into()))
            } else {
                Ok(7u8)
            }
        });
        assert_eq!(ok.unwrap(), 7);
        assert_eq!(attempts, 2);
    }
}
