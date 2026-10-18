// SPDX-License-Identifier: Apache-2.0

//! Scheduled rotation: when a full regeneration is due and the next anchor.

use std::time::Duration;

use jiff::Timestamp;

use crate::plan::inventory::Anchor;

/// The outcome of evaluating the rotation schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotationPlan {
    /// Whether every generated value must be regenerated now.
    pub due: bool,
    /// The anchor to record on the Secret (`generated-at`).
    pub anchor: Timestamp,
    /// When the next rotation falls due; `None` without an interval.
    pub next: Option<Timestamp>,
    /// The recorded anchor was unreadable and was replaced.
    pub anchor_reset: bool,
}

/// `at + interval`, or `None` when the sum is out of range.
fn plus(at: Timestamp, interval: Duration) -> Option<Timestamp> {
    at.checked_add(interval).ok()
}

/// Evaluates the schedule at `now`. Missed intervals coalesce: a Secret that
/// is several intervals overdue rotates once and the next rotation is one
/// interval after `now`. Arithmetic overflow yields `next = None` and not due.
#[must_use]
pub fn rotation(interval: Option<Duration>, anchor: &Anchor, now: Timestamp) -> RotationPlan {
    let reset = |anchor_reset: bool| RotationPlan {
        due: false,
        anchor: now,
        next: interval.and_then(|i| plus(now, i)),
        anchor_reset,
    };
    let Some(interval) = interval else {
        return RotationPlan {
            due: false,
            anchor: match anchor {
                Anchor::At(at) => *at,
                Anchor::Absent | Anchor::Invalid => now,
            },
            next: None,
            anchor_reset: matches!(anchor, Anchor::Invalid),
        };
    };
    match anchor {
        Anchor::Absent => reset(false),
        Anchor::Invalid => reset(true),
        Anchor::At(at) => {
            let next = plus(*at, interval);
            let due = *at <= now && next.is_some_and(|next| now >= next);
            if due {
                RotationPlan {
                    due: true,
                    anchor: now,
                    next: plus(now, interval),
                    anchor_reset: false,
                }
            } else {
                RotationPlan {
                    due: false,
                    anchor: *at,
                    next,
                    anchor_reset: false,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests report through panics.
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    const DAY: u64 = 86_400;

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    fn days(n: u64) -> Duration {
        Duration::from_secs(n * DAY)
    }

    fn now() -> Timestamp {
        ts("2026-10-19T12:00:00Z")
    }

    fn after(base: Timestamp, d: Duration) -> Timestamp {
        base.checked_add(d).expect("in range")
    }

    fn before(base: Timestamp, d: Duration) -> Timestamp {
        base.checked_sub(d).expect("in range")
    }

    #[test]
    fn no_interval_never_due() {
        let at = before(now(), days(500));
        let plan = rotation(None, &Anchor::At(at), now());
        assert_eq!(
            plan,
            RotationPlan {
                due: false,
                anchor: at,
                next: None,
                anchor_reset: false
            }
        );
    }

    #[test]
    fn no_interval_absent_anchor_is_now() {
        let plan = rotation(None, &Anchor::Absent, now());
        assert_eq!(
            plan,
            RotationPlan {
                due: false,
                anchor: now(),
                next: None,
                anchor_reset: false
            }
        );
    }

    #[test]
    fn absent_anchor_starts_the_clock() {
        let plan = rotation(Some(days(90)), &Anchor::Absent, now());
        assert_eq!(
            plan,
            RotationPlan {
                due: false,
                anchor: now(),
                next: Some(after(now(), days(90))),
                anchor_reset: false
            }
        );
    }

    #[test]
    fn invalid_anchor_resets_without_rotating() {
        let plan = rotation(Some(days(90)), &Anchor::Invalid, now());
        assert_eq!(
            plan,
            RotationPlan {
                due: false,
                anchor: now(),
                next: Some(after(now(), days(90))),
                anchor_reset: true
            }
        );
    }

    #[test]
    fn recent_anchor_is_not_due() {
        let at = before(now(), days(10));
        let plan = rotation(Some(days(90)), &Anchor::At(at), now());
        assert_eq!(
            plan,
            RotationPlan {
                due: false,
                anchor: at,
                next: Some(after(at, days(90))),
                anchor_reset: false
            }
        );
    }

    #[test]
    fn due_exactly_at_the_boundary() {
        let at = before(now(), days(90));
        let plan = rotation(Some(days(90)), &Anchor::At(at), now());
        assert!(plan.due);
        assert_eq!(plan.anchor, now());
        assert_eq!(plan.next, Some(after(now(), days(90))));
    }

    #[test]
    fn missed_intervals_coalesce_into_one_rotation() {
        let at = before(now(), days(300));
        let plan = rotation(Some(days(90)), &Anchor::At(at), now());
        assert_eq!(
            plan,
            RotationPlan {
                due: true,
                anchor: now(),
                next: Some(after(now(), days(90))),
                anchor_reset: false
            }
        );
    }

    #[test]
    fn future_anchor_is_not_due() {
        let at = after(now(), days(5));
        let plan = rotation(Some(days(90)), &Anchor::At(at), now());
        assert_eq!(
            plan,
            RotationPlan {
                due: false,
                anchor: at,
                next: Some(after(at, days(90))),
                anchor_reset: false
            }
        );
    }

    #[test]
    fn overflow_is_not_due_and_has_no_next() {
        let plan = rotation(Some(Duration::MAX), &Anchor::At(now()), now());
        assert!(!plan.due);
        assert_eq!(plan.anchor, now());
        assert_eq!(plan.next, None);
        let absent = rotation(Some(Duration::MAX), &Anchor::Absent, now());
        assert!(!absent.due);
        assert_eq!(absent.next, None);
    }
}
