// SPDX-License-Identifier: Apache-2.0

//! Lease-based leader election (`coordination.k8s.io/v1`).
//!
//! In-house because the maintained crates for kube 4 write the Lease with
//! `patch`, and the operator's RBAC (design §9) grants only `get`, `create`
//! and `update` on leases. This module uses exactly those verbs:
//!
//! - **Acquire** when the Lease is absent (`create`), has no holder, is ours,
//!   or has not changed for its `leaseDurationSeconds` as observed on *our*
//!   clock (`update` with the observed `resourceVersion`). A live lease is
//!   never stolen; a lost race is a `409 Conflict` and we keep standing by.
//! - **Renew** every [`RENEW_PERIOD`] with `update` and the `resourceVersion`
//!   of our last write. If no renewal succeeds within [`RENEW_DEADLINE`], or
//!   the Lease names another holder, leadership is lost: the work is dropped
//!   and an error is returned (the process exits non-zero and restarts).
//! - **Release** (holder cleared) when the work finishes, so a successor can
//!   take over without waiting for expiry.
//!
//! Expiry is judged from how long the Lease has stayed unchanged locally, not
//! from its `renewTime`, so clock skew between nodes does not matter.

use std::future::Future;
use std::pin::pin;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::Client;
use kube::api::{Api, ObjectMeta, PostParams};

use crate::outcome::kube_error_summary;

/// How long a lease stays valid after its last renewal.
pub const LEASE_DURATION: Duration = Duration::from_secs(15);
/// How often the leader renews.
pub const RENEW_PERIOD: Duration = Duration::from_secs(5);
/// The leader gives up when no renewal succeeded for this long; shorter than
/// [`LEASE_DURATION`], so it stops before anyone else may take over.
pub const RENEW_DEADLINE: Duration = Duration::from_secs(10);
/// How often a standing-by candidate retries.
pub const RETRY_PERIOD: Duration = Duration::from_secs(5);
/// Pause before retrying a failed renewal.
const RENEW_RETRY: Duration = Duration::from_secs(1);
// The leader must give up before anyone else may take over.
const _: () = assert!(
    RENEW_PERIOD.as_secs() < RENEW_DEADLINE.as_secs()
        && RENEW_DEADLINE.as_secs() < LEASE_DURATION.as_secs()
);

/// HTTP status of an optimistic-concurrency or create race.
const HTTP_CONFLICT: u16 = 409;

/// Waits until this instance holds the Lease `lease_ns/lease_name` as
/// `holder`, then runs `work` while keeping the Lease renewed.
///
/// Returns `work`'s result after releasing the Lease. Cancel the returned
/// future to stop standing by.
///
/// # Errors
///
/// Fails when leadership is lost while `work` runs (`work` is dropped), or
/// with `work`'s own error.
pub async fn run_with_leadership<F>(
    client: Client,
    lease_ns: &str,
    lease_name: &str,
    holder: &str,
    work: F,
) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>>,
{
    let api: Api<Lease> = Api::namespaced(client, lease_ns);
    let lease = acquire(&api, lease_name, holder).await;
    tracing::info!(lease = lease_name, holder, "acquired leadership");

    let work = pin!(work);
    let keep = pin!(keep(&api, lease_name, holder, lease));
    match futures::future::select(work, keep).await {
        futures::future::Either::Left((result, _)) => {
            release(&api, lease_name, holder).await;
            result
        }
        futures::future::Either::Right((lost, _)) => {
            tracing::error!(lease = lease_name, holder, reason = %lost, "lost leadership");
            Err(lost)
        }
    }
}

/// Stands by until the Lease is acquired; returns the written Lease.
async fn acquire(api: &Api<Lease>, name: &str, holder: &str) -> Lease {
    let mut observed = Observed::default();
    let mut last_holder: Option<String> = None;
    loop {
        match try_acquire(api, name, holder, &mut observed).await {
            Ok(Attempt::Acquired(lease)) => return *lease,
            Ok(Attempt::HeldBy(other)) => {
                if last_holder.as_deref() != Some(other.as_str()) {
                    tracing::info!(lease = name, leader = %other, "standing by");
                    last_holder = Some(other);
                }
            }
            Ok(Attempt::Raced) => tracing::debug!(lease = name, "lost a lease race"),
            Err(err) => {
                tracing::warn!(lease = name, error = %kube_error_summary(&err), "lease request failed");
            }
        }
        tokio::time::sleep(RETRY_PERIOD).await;
    }
}

/// The outcome of one acquisition attempt.
enum Attempt {
    Acquired(Box<Lease>),
    HeldBy(String),
    Raced,
}

async fn try_acquire(
    api: &Api<Lease>,
    name: &str,
    holder: &str,
    observed: &mut Observed,
) -> Result<Attempt, kube::Error> {
    let Some(current) = api.get_opt(name).await? else {
        let lease = Lease {
            metadata: ObjectMeta {
                name: Some(name.to_owned()),
                ..ObjectMeta::default()
            },
            spec: Some(claimed(None, holder, Timestamp::now())),
        };
        return conflict_as_race(api.create(&PostParams::default(), &lease).await);
    };
    let unchanged_for =
        observed.unchanged_for(current.metadata.resource_version.as_deref(), Instant::now());
    match decide(current.spec.as_ref(), holder, unchanged_for) {
        Decision::Take => {
            let lease = Lease {
                metadata: current.metadata.clone(),
                spec: Some(claimed(current.spec.as_ref(), holder, Timestamp::now())),
            };
            conflict_as_race(api.replace(name, &PostParams::default(), &lease).await)
        }
        Decision::Wait => Ok(Attempt::HeldBy(
            current
                .spec
                .and_then(|s| s.holder_identity)
                .unwrap_or_default(),
        )),
    }
}

fn conflict_as_race(result: Result<Lease, kube::Error>) -> Result<Attempt, kube::Error> {
    match result {
        Ok(lease) => Ok(Attempt::Acquired(Box::new(lease))),
        Err(kube::Error::Api(status)) if status.code == HTTP_CONFLICT => Ok(Attempt::Raced),
        Err(err) => Err(err),
    }
}

/// Renews the Lease until leadership is lost; returns why.
async fn keep(api: &Api<Lease>, name: &str, holder: &str, mut lease: Lease) -> anyhow::Error {
    let mut last_renew = Instant::now();
    let mut pause = RENEW_PERIOD;
    loop {
        tokio::time::sleep(pause).await;
        let Some(remaining) = RENEW_DEADLINE.checked_sub(last_renew.elapsed()) else {
            return anyhow::anyhow!("lease {name} not renewed within {RENEW_DEADLINE:?}");
        };
        match tokio::time::timeout(remaining, renew(api, name, holder, &lease)).await {
            Err(_) => {
                return anyhow::anyhow!("lease {name} not renewed within {RENEW_DEADLINE:?}");
            }
            Ok(Ok(Renewal::Renewed(renewed))) => {
                lease = *renewed;
                last_renew = Instant::now();
                pause = RENEW_PERIOD;
            }
            Ok(Ok(Renewal::Lost(other))) => {
                return anyhow::anyhow!("lease {name} is now held by {other:?}");
            }
            Ok(Err(err)) => {
                tracing::warn!(lease = name, error = %kube_error_summary(&err), "lease renewal failed; retrying");
                pause = RENEW_RETRY;
            }
        }
    }
}

/// The outcome of one renewal.
enum Renewal {
    Renewed(Box<Lease>),
    Lost(String),
}

async fn renew(
    api: &Api<Lease>,
    name: &str,
    holder: &str,
    ours: &Lease,
) -> Result<Renewal, kube::Error> {
    let update = |base: &Lease| Lease {
        metadata: base.metadata.clone(),
        spec: Some(claimed(base.spec.as_ref(), holder, Timestamp::now())),
    };
    match api
        .replace(name, &PostParams::default(), &update(ours))
        .await
    {
        Ok(lease) => Ok(Renewal::Renewed(Box::new(lease))),
        Err(kube::Error::Api(status)) if status.code == HTTP_CONFLICT => {
            // Someone else wrote the Lease: still ours only if it names us.
            let current = api.get(name).await?;
            match holder_of(current.spec.as_ref()) {
                Some(h) if h == holder => api
                    .replace(name, &PostParams::default(), &update(&current))
                    .await
                    .map(|lease| Renewal::Renewed(Box::new(lease))),
                other => Ok(Renewal::Lost(other.unwrap_or_default().to_owned())),
            }
        }
        Err(err) => Err(err),
    }
}

/// Clears the holder if it is still us, so a successor need not wait for
/// expiry. Best effort: failures are logged.
async fn release(api: &Api<Lease>, name: &str, holder: &str) {
    let result = async {
        let current = api.get(name).await?;
        if holder_of(current.spec.as_ref()) != Some(holder) {
            return Ok(());
        }
        let lease = Lease {
            metadata: current.metadata.clone(),
            spec: Some(released(current.spec.as_ref())),
        };
        api.replace(name, &PostParams::default(), &lease)
            .await
            .map(drop)
    }
    .await;
    match result {
        Ok(()) => tracing::info!(lease = name, holder, "released leadership"),
        Err(err) => {
            tracing::warn!(lease = name, error = %kube_error_summary(&err), "releasing the lease failed");
        }
    }
}

/// What a candidate may do with the Lease it observed.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Take,
    Wait,
}

/// Tracks how long the Lease has stayed unchanged on the local clock.
#[derive(Debug, Default)]
struct Observed {
    version: Option<String>,
    since: Option<Instant>,
}

impl Observed {
    /// Records the Lease `version` seen at `now`; returns how long it has been
    /// unchanged (zero when it just changed or was first seen).
    fn unchanged_for(&mut self, version: Option<&str>, now: Instant) -> Duration {
        match self.since {
            Some(since) if self.version.as_deref() == version => {
                now.saturating_duration_since(since)
            }
            _ => {
                self.version = version.map(str::to_owned);
                self.since = Some(now);
                Duration::ZERO
            }
        }
    }
}

/// The current holder, `None` when unset or empty.
fn holder_of(spec: Option<&LeaseSpec>) -> Option<&str> {
    spec.and_then(|s| s.holder_identity.as_deref())
        .filter(|h| !h.is_empty())
}

/// The Lease's own duration, or [`LEASE_DURATION`] when missing or invalid.
fn lease_duration(spec: Option<&LeaseSpec>) -> Duration {
    spec.and_then(|s| s.lease_duration_seconds)
        .and_then(|secs| u64::try_from(secs).ok())
        .filter(|&secs| secs > 0)
        .map_or(LEASE_DURATION, Duration::from_secs)
}

/// Whether `identity` may take the Lease: free, already ours, or unchanged
/// for at least its duration. A live lease held by another is never taken.
fn decide(spec: Option<&LeaseSpec>, identity: &str, unchanged_for: Duration) -> Decision {
    match holder_of(spec) {
        None => Decision::Take,
        Some(h) if h == identity => Decision::Take,
        Some(_) if unchanged_for >= lease_duration(spec) => Decision::Take,
        Some(_) => Decision::Wait,
    }
}

/// The Lease spec after `identity` claims or renews it at `now`.
fn claimed(current: Option<&LeaseSpec>, identity: &str, now: Timestamp) -> LeaseSpec {
    let previous = holder_of(current);
    let same_holder = previous == Some(identity);
    let transitions = current.and_then(|s| s.lease_transitions).unwrap_or(0);
    let acquire_time = if same_holder {
        current
            .and_then(|s| s.acquire_time.clone())
            .unwrap_or(MicroTime(now))
    } else {
        MicroTime(now)
    };
    LeaseSpec {
        holder_identity: Some(identity.to_owned()),
        lease_duration_seconds: Some(duration_seconds(LEASE_DURATION)),
        acquire_time: Some(acquire_time),
        renew_time: Some(MicroTime(now)),
        lease_transitions: Some(if current.is_none() || same_holder {
            transitions
        } else {
            transitions.saturating_add(1)
        }),
        ..current.cloned().unwrap_or_default()
    }
}

/// The Lease spec with the holder cleared.
fn released(current: Option<&LeaseSpec>) -> LeaseSpec {
    LeaseSpec {
        holder_identity: None,
        ..current.cloned().unwrap_or_default()
    }
}

fn duration_seconds(d: Duration) -> i32 {
    i32::try_from(d.as_secs()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn spec(holder: Option<&str>, duration: Option<i32>) -> LeaseSpec {
        LeaseSpec {
            holder_identity: holder.map(str::to_owned),
            lease_duration_seconds: duration,
            ..LeaseSpec::default()
        }
    }

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn free_or_own_lease_is_taken() {
        assert_eq!(decide(None, "me", Duration::ZERO), Decision::Take);
        assert_eq!(
            decide(Some(&spec(None, Some(15))), "me", Duration::ZERO),
            Decision::Take
        );
        assert_eq!(
            decide(Some(&spec(Some(""), Some(15))), "me", Duration::ZERO),
            Decision::Take
        );
        assert_eq!(
            decide(Some(&spec(Some("me"), Some(15))), "me", Duration::ZERO),
            Decision::Take
        );
    }

    #[test]
    fn live_lease_is_never_stolen() {
        let other = spec(Some("other"), Some(15));
        assert_eq!(decide(Some(&other), "me", Duration::ZERO), Decision::Wait);
        assert_eq!(
            decide(Some(&other), "me", Duration::from_secs(14)),
            Decision::Wait
        );
    }

    #[test]
    fn expired_lease_is_taken() {
        let other = spec(Some("other"), Some(15));
        assert_eq!(
            decide(Some(&other), "me", Duration::from_secs(15)),
            Decision::Take
        );
        let long = spec(Some("other"), Some(60));
        assert_eq!(
            decide(Some(&long), "me", Duration::from_secs(30)),
            Decision::Wait
        );
        // Missing or invalid durations fall back to the default.
        for duration in [None, Some(0), Some(-5)] {
            let s = spec(Some("other"), duration);
            assert_eq!(
                decide(Some(&s), "me", Duration::from_secs(14)),
                Decision::Wait
            );
            assert_eq!(decide(Some(&s), "me", LEASE_DURATION), Decision::Take);
        }
    }

    #[test]
    fn expiry_is_measured_on_the_local_clock() {
        let start = Instant::now();
        let mut observed = Observed::default();
        assert_eq!(observed.unchanged_for(Some("1"), start), Duration::ZERO);
        let later = start + Duration::from_secs(10);
        assert_eq!(
            observed.unchanged_for(Some("1"), later),
            Duration::from_secs(10)
        );
        // A renewal (new resourceVersion) restarts the clock.
        assert_eq!(observed.unchanged_for(Some("2"), later), Duration::ZERO);
        assert_eq!(
            observed.unchanged_for(Some("2"), later + Duration::from_secs(3)),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn claiming_a_new_lease() {
        let now = ts("2026-10-19T10:00:00Z");
        let s = claimed(None, "me", now);
        assert_eq!(s.holder_identity.as_deref(), Some("me"));
        assert_eq!(s.lease_duration_seconds, Some(15));
        assert_eq!(s.acquire_time, Some(MicroTime(now)));
        assert_eq!(s.renew_time, Some(MicroTime(now)));
        assert_eq!(s.lease_transitions, Some(0));
    }

    #[test]
    fn renewing_keeps_acquire_time_and_transitions() {
        let first = ts("2026-10-19T10:00:00Z");
        let now = ts("2026-10-19T10:00:05Z");
        let mine = LeaseSpec {
            lease_transitions: Some(3),
            ..claimed(None, "me", first)
        };
        let s = claimed(Some(&mine), "me", now);
        assert_eq!(s.acquire_time, Some(MicroTime(first)));
        assert_eq!(s.renew_time, Some(MicroTime(now)));
        assert_eq!(s.lease_transitions, Some(3));
    }

    #[test]
    fn taking_over_counts_a_transition() {
        let first = ts("2026-10-19T10:00:00Z");
        let now = ts("2026-10-19T10:01:00Z");
        let theirs = claimed(None, "other", first);
        let s = claimed(Some(&theirs), "me", now);
        assert_eq!(s.holder_identity.as_deref(), Some("me"));
        assert_eq!(s.acquire_time, Some(MicroTime(now)));
        assert_eq!(s.lease_transitions, Some(1));
        // After a release the next holder also counts as a transition.
        let s = claimed(Some(&released(Some(&theirs))), "me", now);
        assert_eq!(s.lease_transitions, Some(1));
    }

    #[test]
    fn release_clears_only_the_holder() {
        let theirs = claimed(None, "me", ts("2026-10-19T10:00:00Z"));
        let r = released(Some(&theirs));
        assert_eq!(r.holder_identity, None);
        assert_eq!(r.renew_time, theirs.renew_time);
        assert_eq!(holder_of(Some(&r)), None);
    }
}
