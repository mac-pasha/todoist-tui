//! Retry policy with exponential backoff, jitter and an injectable clock
//! (T-206, EPIC-03).
//!
//! HTTP-free and generic over the operation: [`retry`] re-runs any async
//! `FnMut() -> Future<Output = Result<T, ApiError>>` closure according to a
//! [`RetryPolicy`], sleeping between attempts through an injected
//! [`Sleeper`] seam. [`TokioSleeper`] is the production implementation;
//! tests use [`RecordingSleeper`], which captures requested durations
//! without ever actually sleeping, so the unit test suite stays fast.
//! Jitter is likewise injected (a `FnMut() -> f64` seam, see
//! [`RetryPolicy::jitter`]) rather than pulled from a `rand` dependency —
//! the crate does not depend on `rand` and this task must not add one.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::ApiError;

/// Configurable exponential-backoff-with-jitter retry policy.
///
/// `delay_for_attempt(n)` (1-indexed: `n == 1` is the delay before the
/// *second* attempt, i.e. the first retry) grows as
/// `base_delay * 2^(n-1)`, clamped at `max_delay`, then perturbed by up to
/// `jitter_fraction` of that capped value via the injected jitter source.
/// With the jitter source fixed at `0.0` the delay is exactly
/// `base_delay * 2^(n-1)` capped at `max_delay`.
pub struct RetryPolicy {
    /// Maximum number of attempts (including the first), bounded by
    /// default to avoid unbounded retry storms.
    pub max_attempts: u32,
    /// Delay before the first retry (attempt 1).
    pub base_delay: Duration,
    /// Upper bound every computed delay is clamped to.
    pub max_delay: Duration,
    /// Fraction (0.0..=1.0) of the capped delay that jitter may add or
    /// subtract. `0.0` disables jitter entirely.
    pub jitter_fraction: f64,
    /// Jitter source: called once per delay computation, expected to
    /// return a value in `0.0..1.0`. Defaults to a small nanotime-based
    /// scramble (see [`default_jitter_source`]) so production callers get
    /// spread-out delays without depending on `rand`. Tests inject a
    /// deterministic closure for reproducible delays.
    jitter: Box<dyn FnMut() -> f64 + Send>,
}

impl RetryPolicy {
    /// Default bounded policy: 4 attempts, 500ms base delay, 30s cap, 20%
    /// jitter, using [`default_jitter_source`].
    pub fn new() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            jitter_fraction: 0.2,
            jitter: Box::new(default_jitter_source),
        }
    }

    /// Replaces the jitter source. Pass `|| 0.0` to disable jitter
    /// entirely (deterministic `base * 2^(n-1)` capped delays), or a
    /// recording/deterministic closure in tests.
    pub fn with_jitter(mut self, jitter: impl FnMut() -> f64 + Send + 'static) -> Self {
        self.jitter = Box::new(jitter);
        self
    }

    /// Computes the delay before attempt `n` (1-indexed: `n == 1` is the
    /// first retry, after the first attempt failed).
    ///
    /// Grows exponentially as `base_delay * 2^(n-1)`, clamped at
    /// `max_delay`, then perturbed by the jitter source: the returned
    /// value is `capped +/- jitter_fraction * capped * (jitter() - 0.5) * 2`,
    /// itself re-clamped into `[0, max_delay]` so jitter can never push the
    /// delay past the configured ceiling.
    pub fn delay_for_attempt(&mut self, n: u32) -> Duration {
        let exponent = n.saturating_sub(1).min(32);
        let factor = 2u64.saturating_pow(exponent);
        let factor = u32::try_from(factor).unwrap_or(u32::MAX);
        let computed = self.base_delay.saturating_mul(factor);
        let capped = computed.min(self.max_delay);

        if self.jitter_fraction <= 0.0 {
            return capped;
        }

        let sample = (self.jitter)().clamp(0.0, 1.0);
        // Map sample in [0, 1) to an offset in [-jitter_fraction, +jitter_fraction].
        let signed = (sample - 0.5) * 2.0 * self.jitter_fraction;
        let capped_secs = capped.as_secs_f64();
        let jittered_secs = (capped_secs + capped_secs * signed).max(0.0);
        let jittered = Duration::from_secs_f64(jittered_secs);
        jittered.min(self.max_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Small nanotime-based scramble used as the default jitter source so
/// production callers get spread-out delays without a `rand` dependency.
/// Returns a value in `0.0..1.0`.
pub fn default_jitter_source() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

/// Abstracts waiting between retry attempts so tests never actually sleep.
pub trait Sleeper: Send + Sync {
    /// Waits for (at least, in the production implementation) `duration`.
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

/// Production [`Sleeper`] backed by `tokio::time::sleep`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Test double that records every requested sleep duration instead of
/// sleeping, so the unit test suite stays well under a second.
#[derive(Debug, Default, Clone)]
pub struct RecordingSleeper {
    durations: Arc<Mutex<Vec<Duration>>>,
}

impl RecordingSleeper {
    /// Builds a fresh recorder with no recorded sleeps.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a copy of every duration recorded so far, in call order.
    pub fn recorded(&self) -> Vec<Duration> {
        self.durations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Sleeper for RecordingSleeper {
    async fn sleep(&self, duration: Duration) {
        self.durations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(duration);
    }
}

/// Re-runs `op` while the returned [`ApiError::is_retryable`] is true,
/// waiting between attempts via `clock` for the duration computed by
/// `policy`.
///
/// Stops and returns `Ok` as soon as `op` succeeds. A non-retryable error
/// (e.g. `ApiError::Unauthorized`) is returned immediately with zero
/// recorded sleeps. Once `policy.max_attempts` attempts have been made and
/// the last one still failed retryably, returns
/// `ApiError::RetriesExhausted { attempts }`. When the failing error is
/// `ApiError::RateLimited { retry_after: Some(d) }`, the wait before the
/// next attempt is at least `d`, overriding the computed backoff.
pub async fn retry<T, C, F, Fut>(
    mut policy: RetryPolicy,
    clock: &C,
    mut op: F,
) -> Result<T, ApiError>
where
    C: Sleeper + ?Sized,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ApiError>>,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if !err.is_retryable() {
                    return Err(err);
                }
                if attempt >= max_attempts {
                    return Err(ApiError::RetriesExhausted { attempts: attempt });
                }

                let mut delay = policy.delay_for_attempt(attempt);
                if let ApiError::RateLimited {
                    retry_after: Some(hint),
                } = &err
                {
                    delay = delay.max(*hint);
                }
                clock.sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: a policy with jitter disabled (jitter source fixed at 0.0) so
    /// delays are exactly `base * 2^(n-1)` capped at `max_delay`.
    fn no_jitter_policy(base: Duration, max: Duration) -> RetryPolicy {
        let mut p = RetryPolicy::new();
        p.base_delay = base;
        p.max_delay = max;
        p.jitter_fraction = 0.0;
        p.with_jitter(|| 0.0)
    }

    // --- RetryPolicy defaults ------------------------------------------------

    #[test]
    fn defaults_are_bounded() {
        let p = RetryPolicy::new();
        assert_eq!(p.max_attempts, 4, "default max_attempts should be 4");
        assert!(p.max_attempts <= 4, "default max_attempts must be bounded");
        assert!(p.base_delay > Duration::ZERO);
        assert!(p.max_delay >= p.base_delay, "cap must be >= base");
        assert!((0.0..=1.0).contains(&p.jitter_fraction));
    }

    // --- delay_for_attempt: exponential growth + clamp -----------------------

    #[test]
    fn delay_grows_exponentially_and_clamps_for_attempts_1_to_6() {
        // base = 100ms, cap = 350ms so growth is visible then clamped:
        // n=1 -> 100, n=2 -> 200, n=3 -> 350 (cap of 400), n=4..6 stay 350.
        let mut p = no_jitter_policy(Duration::from_millis(100), Duration::from_millis(350));
        let expected = [
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(350),
            Duration::from_millis(350),
            Duration::from_millis(350),
            Duration::from_millis(350),
        ];
        for (i, &want) in expected.iter().enumerate() {
            let got = p.delay_for_attempt(u32::try_from(i).unwrap() + 1);
            assert_eq!(
                got,
                want,
                "attempt {} should be {:?}, got {:?}",
                i + 1,
                want,
                got
            );
        }
    }

    #[test]
    fn delay_is_exact_without_cap() {
        // base = 1s, cap = 1h: no clamping in the first 6 attempts.
        let mut p = no_jitter_policy(Duration::from_secs(1), Duration::from_secs(3600));
        for n in 1..=6u32 {
            let want = Duration::from_secs(1u64 << (n - 1));
            assert_eq!(p.delay_for_attempt(n), want, "attempt {n}");
        }
    }

    #[test]
    fn jitter_disabled_with_zero_fraction_gives_exact_delay() {
        // Even with a nonzero jitter source, fraction 0.0 disables perturbation.
        let mut p = RetryPolicy::new();
        p.base_delay = Duration::from_millis(250);
        p.max_delay = Duration::from_secs(30);
        p.jitter_fraction = 0.0;
        let mut p = p.with_jitter(|| 0.99); // would add jitter if fraction > 0
        for n in 1..=6u32 {
            let want = Duration::from_millis(250u64 << (n - 1));
            assert_eq!(p.delay_for_attempt(n), want, "attempt {n}");
        }
    }

    // --- jitter determinism through the seam ---------------------------------

    #[test]
    fn fixed_jitter_closure_is_deterministic() {
        // jitter source fixed at 0.5 -> same delay every call, and bounded.
        let mut a = RetryPolicy::new();
        a.jitter_fraction = 0.5;
        let mut a = a.with_jitter(|| 0.5);
        let mut b = RetryPolicy::new();
        b.jitter_fraction = 0.5;
        let mut b = b.with_jitter(|| 0.5);

        for n in 1..=6u32 {
            assert_eq!(
                a.delay_for_attempt(n),
                b.delay_for_attempt(n),
                "attempt {n} must be reproducible"
            );
            assert!(
                a.delay_for_attempt(n) <= a.max_delay,
                "jitter must stay under cap"
            );
        }
    }

    #[test]
    fn jitter_source_is_called_once_per_delay() {
        let calls = Arc::new(Mutex::new(0u32));
        let p = RetryPolicy::new();
        let calls2 = calls.clone();
        let mut p = p.with_jitter(move || {
            *calls2.lock().unwrap() += 1;
            0.0
        });
        for _ in 0..4 {
            p.delay_for_attempt(1);
        }
        assert_eq!(
            *calls.lock().unwrap(),
            4,
            "jitter source called once per delay"
        );
    }

    // --- RecordingSleeper -----------------------------------------------------

    #[tokio::test]
    async fn recording_sleeper_records_exact_durations_and_never_blocks() {
        let clock = RecordingSleeper::new();
        let d1 = Duration::from_millis(123);
        let d2 = Duration::from_secs(7);
        clock.sleep(d1).await;
        clock.sleep(d2).await;
        let rec = clock.recorded();
        assert_eq!(rec, vec![d1, d2], "recorded durations must match exactly");
    }

    #[tokio::test]
    async fn recording_sleeper_starts_empty() {
        let clock = RecordingSleeper::new();
        assert_eq!(clock.recorded(), [] as [std::time::Duration; 0]);
    }

    // --- retry(): non-retryable errors stop immediately -----------------------

    #[tokio::test]
    async fn retry_stops_immediately_on_non_retryable_error() {
        let clock = RecordingSleeper::new();
        let mut attempts = 0u32;
        let result: Result<(), ApiError> = retry(
            no_jitter_policy(Duration::from_millis(10), Duration::from_secs(1)),
            &clock,
            || {
                attempts += 1;
                async { Err(ApiError::Unauthorized) }
            },
        )
        .await;

        match result {
            Err(ApiError::Unauthorized) => {}
            other => panic!("expected Unauthorized, got {other:?}"),
        }
        assert_eq!(attempts, 1, "non-retryable error must not be retried");
        assert!(clock.recorded().is_empty(), "no sleeps may be recorded");
    }

    // --- retry(): success mid-loop --------------------------------------------

    #[tokio::test]
    async fn retry_succeeds_after_transient_failures() {
        let clock = RecordingSleeper::new();
        let mut calls = 0u32;
        let result: Result<i32, ApiError> = retry(
            no_jitter_policy(Duration::from_millis(100), Duration::from_secs(10)),
            &clock,
            || {
                calls += 1;
                let n = calls;
                async move {
                    if n < 3 {
                        Err(ApiError::Server { status: 500 })
                    } else {
                        Ok(42)
                    }
                }
            },
        )
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 3, "should succeed on the third attempt");
        // Two failed attempts -> two sleeps of exactly base * 2^(0) and base * 2^(1).
        assert_eq!(
            clock.recorded(),
            vec![Duration::from_millis(100), Duration::from_millis(200)],
            "delays between attempts must match the policy"
        );
    }

    // --- retry(): exhaustion ---------------------------------------------------

    #[tokio::test]
    async fn retry_exhausts_after_max_attempts() {
        let clock = RecordingSleeper::new();
        let policy = no_jitter_policy(Duration::from_millis(10), Duration::from_secs(1));
        let mut calls = 0u32;
        let result: Result<(), ApiError> = retry(policy, &clock, || {
            calls += 1;
            async { Err(ApiError::Server { status: 503 }) }
        })
        .await;

        let expected_attempts = 4; // RetryPolicy::new() default
        match result {
            Err(ApiError::RetriesExhausted { attempts }) => {
                assert_eq!(attempts, expected_attempts);
            }
            other => panic!("expected RetriesExhausted, got {other:?}"),
        }
        assert_eq!(calls, expected_attempts, "exactly max_attempts calls");
        // Sleeps happen between attempts only: max_attempts - 1.
        assert_eq!(
            clock.recorded().len(),
            expected_attempts as usize - 1,
            "one sleep between each pair of attempts"
        );
    }

    #[test]
    fn retries_exhausted_is_not_retryable() {
        assert!(
            !ApiError::RetriesExhausted { attempts: 4 }.is_retryable(),
            "an exhausted retry budget must not itself be retried"
        );
    }

    // --- retry(): rate-limit hint overrides backoff ----------------------------

    #[tokio::test]
    async fn rate_limited_retry_after_overrides_computed_backoff() {
        let hint = Duration::from_secs(2);
        let clock = RecordingSleeper::new();
        let mut calls = 0u32;
        let result: Result<(), ApiError> = retry(
            // base of 10ms is far smaller than the 2s server hint, so the
            // hint must win and the recorded sleep must be >= 2s.
            no_jitter_policy(Duration::from_millis(10), Duration::from_secs(10)),
            &clock,
            || {
                calls += 1;
                let n = calls;
                async move {
                    if n == 1 {
                        Err(ApiError::RateLimited {
                            retry_after: Some(hint),
                        })
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;

        assert!(result.is_ok(), "should succeed on the second attempt");
        let rec = clock.recorded();
        assert_eq!(rec.len(), 1, "exactly one sleep between the two attempts");
        assert!(
            rec[0] >= hint,
            "recorded sleep {:?} must be >= the 2s Retry-After hint",
            rec[0]
        );
    }

    #[tokio::test]
    async fn rate_limited_without_hint_falls_back_to_backoff() {
        let clock = RecordingSleeper::new();
        let mut calls = 0u32;
        let _ = retry(
            no_jitter_policy(Duration::from_millis(100), Duration::from_secs(10)),
            &clock,
            || {
                calls += 1;
                let n = calls;
                async move {
                    if n == 1 {
                        Err(ApiError::RateLimited { retry_after: None })
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;
        // No hint -> plain computed backoff: base * 2^0 = 100ms.
        assert_eq!(clock.recorded(), vec![Duration::from_millis(100)]);
    }
}
