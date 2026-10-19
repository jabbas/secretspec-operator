// SPDX-License-Identifier: Apache-2.0

//! The pure reconcile planner: decides what to write without touching the cluster.
//!
//! [`plan`] reads the `SecretSpec` and the Secret that currently exists and
//! returns everything the controller has to do: the Secret to apply (if any),
//! the status to write, the events to emit and when to look again.

pub mod decide;
pub mod inventory;
pub mod keys;
pub mod ownership;
pub mod rotation;
pub mod status;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use jiff::Timestamp;
use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference, Time};
use rand_core::CryptoRngCore;
use secretspec_api::constants::{
    ANNOTATION_FINGERPRINTS, ANNOTATION_GENERATED_AT, ANNOTATION_KEYS, ANNOTATION_SPEC_UID,
    EVENT_ADOPTED, EVENT_ANCHOR_RESET, EVENT_CONFLICT, EVENT_FINGERPRINTS_RESET, EVENT_GENERATED,
    EVENT_GENERATION_FAILED, EVENT_INVALID_SPEC, EVENT_LOW_ENTROPY, EVENT_PASSWORD_TRUNCATED,
    EVENT_REGENERATED, EVENT_ROTATED, LABEL_MANAGED_BY, LABEL_SPEC_NAME, MANAGED_BY_VALUE,
    REASON_CONFLICT, REASON_GENERATED, REASON_GENERATION_FAILED, REASON_INVALID_SPEC,
    REASON_WAITING_FOR_GC,
};
use secretspec_api::resolve::{ResolvedKind, ResolvedSpec, resolve};
use secretspec_api::v1alpha1::{SecretKind, SecretSpec, SecretSpecStatus};

use crate::fingerprint::Slot;
use crate::generate::GenerateError;
use crate::plan::decide::{Decision, SlotOutcome, decide};
use crate::plan::inventory::{Anchor, inventory};
use crate::plan::keys::KeyMap;
use crate::plan::ownership::{Ownership, owner_references, ownership};
use crate::plan::rotation::{RotationPlan, rotation};
use crate::plan::status::{Readiness, Times};

/// How long to wait before looking again while a previous owner's Secret is
/// being garbage-collected.
pub const WAIT_FOR_GC_REQUEUE: Duration = Duration::from_secs(5);

/// How long to wait before retrying after a generation failure.
pub const GENERATION_RETRY: Duration = Duration::from_secs(30);

/// The shortest rotation requeue, so an overdue timer never spins.
const MIN_REQUEUE: Duration = Duration::from_secs(1);

/// Generated values with less entropy than this get a `LowEntropy` warning.
const LOW_ENTROPY_BITS: f64 = 128.0;

/// bcrypt hashes at most this many bytes of the password.
const BCRYPT_MAX_PASSWORD_BYTES: usize = 72;

/// The Secret type assumed when the field is empty.
const DEFAULT_SECRET_TYPE: &str = "Opaque";

/// Everything one reconcile has to do.
#[derive(Debug)]
pub struct Plan {
    /// What to do with the Secret.
    pub action: SecretAction,
    /// The status to write.
    pub status: SecretSpecStatus,
    /// The events to emit, at most one per reason.
    pub events: Vec<PlannedEvent>,
    /// When to reconcile again (rotation timer or garbage-collection wait).
    pub requeue_after: Option<Duration>,
}

/// What to do with the Secret.
pub enum SecretAction {
    /// Server-side apply this Secret. It sets only the fields the operator
    /// owns.
    Apply(Box<Secret>),
    /// The existing Secret already matches; skip the API call.
    UpToDate,
    /// Write nothing: conflict, invalid spec, or waiting for garbage
    /// collection.
    Nothing,
}

/// `Debug` never shows Secret data, only its key names.
impl fmt::Debug for SecretAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Apply(secret) => {
                let keys: Vec<&String> = secret.data.iter().flat_map(BTreeMap::keys).collect();
                f.debug_struct("Apply")
                    .field("metadata", &secret.metadata)
                    .field("type", &secret.type_)
                    .field("data_keys", &keys)
                    .finish_non_exhaustive()
            }
            Self::UpToDate => f.write_str("UpToDate"),
            Self::Nothing => f.write_str("Nothing"),
        }
    }
}

/// An event for the controller to emit on the `SecretSpec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedEvent {
    /// `Warning` when true, `Normal` otherwise.
    pub warning: bool,
    /// One of the `EVENT_*` constants.
    pub reason: &'static str,
    /// Human-readable detail; names keys, never values.
    pub message: String,
}

impl PlannedEvent {
    fn normal(reason: &'static str, message: String) -> Self {
        Self {
            warning: false,
            reason,
            message,
        }
    }

    fn warning(reason: &'static str, message: String) -> Self {
        Self {
            warning: true,
            reason,
            message,
        }
    }
}

/// Plans one reconcile of `spec` against the Secret that currently exists
/// under its name. Pure: no I/O; randomness only from `rng`.
///
/// `now` is truncated to whole seconds; the `generated-at` annotation and the
/// status times use that instant.
pub fn plan<R: CryptoRngCore + ?Sized>(
    spec: &SecretSpec,
    existing: Option<&Secret>,
    now: Timestamp,
    rng: &mut R,
) -> Plan {
    let now = whole_seconds(now);
    let name = spec.metadata.name.as_deref().unwrap_or_default();
    let namespace = spec.metadata.namespace.as_deref().unwrap_or_default();

    let resolved = match resolve(&spec.spec, name, namespace) {
        Ok(resolved) => resolved,
        Err(invalid) => return halted(spec, None, invalid_spec(invalid.to_string()), now),
    };
    let kind = Some(resolved.secret_kind());

    let owner = ownership(existing, spec);
    match &owner {
        Ownership::Conflict(message) => {
            return halted(spec, kind, conflict(message.clone()), now);
        }
        Ownership::WaitForGc => {
            let readiness = Readiness {
                ready: false,
                reason: REASON_WAITING_FOR_GC,
                message: format!(
                    "Secret {name} is still owned by a deleted SecretSpec; waiting for garbage collection"
                ),
            };
            let mut plan = halted(
                spec,
                kind,
                Halt {
                    readiness,
                    event: None,
                },
                now,
            );
            plan.requeue_after = Some(WAIT_FOR_GC_REQUEUE);
            return plan;
        }
        Ownership::Absent | Ownership::Owned | Ownership::Adoptable => {}
    }

    let inv = existing.map(inventory);
    if let Some(inv) = &inv
        && inv.secret_type != secret_type(&resolved)
    {
        let message = format!(
            "Secret {name} has type {}; delete it to recreate it with type {}",
            inv.secret_type,
            secret_type(&resolved)
        );
        return halted(spec, kind, conflict(message), now);
    }

    let anchor = inv.as_ref().map_or(Anchor::Absent, |inv| inv.generated_at);
    // Rotation is scheduled from the moment the operator first observes it:
    // without a recorded next rotation time, a past-due anchor does not rotate.
    let scheduled = spec
        .status
        .as_ref()
        .is_some_and(|status| status.next_rotation_time.is_some());
    let schedule = rotation(resolved.rotation, &anchor, scheduled, now);
    let decision = match decide(&resolved, inv.as_ref(), schedule.due, rng) {
        Ok(decision) => decision,
        Err(error) => return halt_for_generate_error(&error, spec, resolved.secret_kind(), now),
    };

    let desired = desired_secret(spec, &resolved, &decision, &schedule);
    // `up_to_date` only sees what the operator applies, so a label or
    // annotation removed from the spec would never leave the Secret. A new
    // generation re-applies once: server-side apply then drops the fields our
    // field manager no longer sets. The status records the generation, so the
    // next call compares again.
    let action = match existing {
        Some(existing) if up_to_date(existing, &desired) && generation_observed(spec) => {
            SecretAction::UpToDate
        }
        _ => SecretAction::Apply(Box::new(desired)),
    };

    let regenerated = regenerated_keys(&resolved, &decision);
    let events = events(&EventInput {
        name,
        resolved: &resolved,
        decision: &decision,
        existed: inv.is_some(),
        owner: &owner,
        schedule: &schedule,
        regenerated: &regenerated,
    });

    let previous = Times::previous(spec);
    let times = Times {
        last_generated: if regenerated.is_empty() {
            previous.last_generated
        } else {
            Some(Time(now))
        },
        last_rotation: if schedule.due {
            Some(Time(now))
        } else {
            previous.last_rotation
        },
        next_rotation: schedule.next.map(Time),
    };
    let readiness = Readiness {
        ready: true,
        reason: REASON_GENERATED,
        message: format!("Secret {name} is up to date"),
    };

    Plan {
        action,
        status: status::status(spec, kind, readiness, times, now),
        events,
        requeue_after: schedule.next.map(|next| until(now, next)),
    }
}

/// Why the planner writes nothing.
struct Halt {
    readiness: Readiness,
    event: Option<PlannedEvent>,
}

fn conflict(message: String) -> Halt {
    Halt {
        readiness: Readiness {
            ready: false,
            reason: REASON_CONFLICT,
            message: message.clone(),
        },
        event: Some(PlannedEvent::warning(EVENT_CONFLICT, message)),
    }
}

fn invalid_spec(message: String) -> Halt {
    Halt {
        readiness: Readiness {
            ready: false,
            reason: REASON_INVALID_SPEC,
            message: message.clone(),
        },
        event: Some(PlannedEvent::warning(EVENT_INVALID_SPEC, message)),
    }
}

/// The plan for a failed generation: write nothing, report Ready False
/// `GenerationFailed` with a Warning event and look again in
/// [`GENERATION_RETRY`]. The failure is internal (SSH library, bcrypt), not a
/// problem of the spec, so it is retryable. The previous status times are
/// kept. `err`'s `Display` carries no values.
#[must_use]
pub fn halt_for_generate_error(
    err: &GenerateError,
    spec: &SecretSpec,
    kind: SecretKind,
    now: Timestamp,
) -> Plan {
    let message = format!("value generation failed: {err}");
    let mut plan = halted(
        spec,
        Some(kind),
        Halt {
            readiness: Readiness {
                ready: false,
                reason: REASON_GENERATION_FAILED,
                message: message.clone(),
            },
            event: Some(PlannedEvent::warning(EVENT_GENERATION_FAILED, message)),
        },
        whole_seconds(now),
    );
    plan.requeue_after = Some(GENERATION_RETRY);
    plan
}

/// A plan that writes nothing; the previous status times are kept.
fn halted(spec: &SecretSpec, kind: Option<SecretKind>, halt: Halt, now: Timestamp) -> Plan {
    Plan {
        action: SecretAction::Nothing,
        status: status::status(spec, kind, halt.readiness, Times::previous(spec), now),
        events: halt.event.into_iter().collect(),
        requeue_after: None,
    }
}

fn whole_seconds(at: Timestamp) -> Timestamp {
    // `as_second` rounds toward zero, which differs from flooring only before
    // 1970; a timestamp from `as_second` is always in range.
    Timestamp::from_second(at.as_second()).unwrap_or(at)
}

/// `next - now`, at least [`MIN_REQUEUE`].
fn until(now: Timestamp, next: Timestamp) -> Duration {
    Duration::try_from(next.duration_since(now))
        .unwrap_or(MIN_REQUEUE)
        .max(MIN_REQUEUE)
}

/// The spec's Secret type; empty reads as `Opaque`.
fn secret_type(resolved: &ResolvedSpec) -> &str {
    if resolved.secret_type.is_empty() {
        DEFAULT_SECRET_TYPE
    } else {
        &resolved.secret_type
    }
}

/// The server-side-apply intent: only the fields the operator owns.
fn desired_secret(
    spec: &SecretSpec,
    resolved: &ResolvedSpec,
    decision: &Decision,
    schedule: &RotationPlan,
) -> Secret {
    let name = spec.metadata.name.clone().unwrap_or_default();

    let mut labels = resolved.labels.clone();
    labels.insert(LABEL_MANAGED_BY.to_owned(), MANAGED_BY_VALUE.to_owned());
    labels.insert(LABEL_SPEC_NAME.to_owned(), name.clone());

    let mut annotations = resolved.annotations.clone();
    annotations.insert(
        ANNOTATION_FINGERPRINTS.to_owned(),
        decision.fingerprints.to_annotation(),
    );
    if !matches!(resolved.kind, ResolvedKind::Fields(_)) {
        annotations.insert(
            ANNOTATION_KEYS.to_owned(),
            KeyMap::for_spec(resolved).to_annotation(),
        );
    }
    annotations.insert(
        ANNOTATION_GENERATED_AT.to_owned(),
        whole_seconds(schedule.anchor).to_string(),
    );
    if let Some(uid) = &spec.metadata.uid {
        annotations.insert(ANNOTATION_SPEC_UID.to_owned(), uid.clone());
    }

    let mut data: BTreeMap<String, ByteString> = resolved
        .data
        .iter()
        .map(|(key, value)| (key.clone(), ByteString(value.as_bytes().to_vec())))
        .collect();
    data.extend(
        decision
            .data
            .iter()
            .map(|(key, value)| (key.clone(), ByteString(value.expose().to_vec()))),
    );

    Secret {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: spec.metadata.namespace.clone(),
            labels: Some(labels),
            annotations: Some(annotations),
            owner_references: owner_references(spec, resolved.deletion_policy),
            ..ObjectMeta::default()
        },
        type_: Some(secret_type(resolved).to_owned()),
        data: Some(data),
        ..Secret::default()
    }
}

/// Whether `existing` already carries everything `desired` applies: the same
/// data map, the same value for every operator-applied label and annotation,
/// the same controller reference (presence and UID) and the same type.
/// Labels, annotations and owner references added by others do not count.
fn up_to_date(existing: &Secret, desired: &Secret) -> bool {
    fn data(secret: &Secret) -> BTreeMap<&str, &[u8]> {
        secret
            .data
            .iter()
            .flatten()
            .map(|(key, value)| (key.as_str(), value.0.as_slice()))
            .collect()
    }
    fn contains(
        existing: Option<&BTreeMap<String, String>>,
        desired: Option<&BTreeMap<String, String>>,
    ) -> bool {
        desired
            .into_iter()
            .flatten()
            .all(|(key, value)| existing.and_then(|map| map.get(key)) == Some(value))
    }
    fn controller(secret: &Secret) -> Option<&str> {
        secret
            .metadata
            .owner_references
            .iter()
            .flatten()
            .find(|reference: &&OwnerReference| reference.controller == Some(true))
            .map(|reference| reference.uid.as_str())
    }
    fn type_of(secret: &Secret) -> &str {
        match secret.type_.as_deref() {
            None | Some("") => DEFAULT_SECRET_TYPE,
            Some(other) => other,
        }
    }

    data(existing) == data(desired)
        && contains(
            existing.metadata.labels.as_ref(),
            desired.metadata.labels.as_ref(),
        )
        && contains(
            existing.metadata.annotations.as_ref(),
            desired.metadata.annotations.as_ref(),
        )
        && controller(existing) == controller(desired)
        && type_of(existing) == type_of(desired)
}

/// The Secret keys that hold the value of `slot`.
fn slot_keys<'a>(resolved: &'a ResolvedSpec, slot: &'a Slot) -> Vec<&'a str> {
    match (slot, &resolved.kind) {
        (Slot::Field(name), _) => vec![name.as_str()],
        (Slot::SshKeyPair, ResolvedKind::SshKeyPair(ssh)) => {
            vec![ssh.private_key_key.as_str(), ssh.public_key_key.as_str()]
        }
        (Slot::BasicAuthPassword, ResolvedKind::BasicAuth(basic)) => {
            vec![basic.password_key.as_str()]
        }
        (Slot::BasicAuthHtpasswd, ResolvedKind::BasicAuth(basic)) => {
            vec![basic.htpasswd_key.as_str()]
        }
        _ => Vec::new(),
    }
}

fn is_new_value(outcome: SlotOutcome) -> bool {
    matches!(outcome, SlotOutcome::Generated | SlotOutcome::Regenerated)
}

/// The keys whose value was generated or regenerated, sorted.
fn regenerated_keys<'a>(resolved: &'a ResolvedSpec, decision: &'a Decision) -> BTreeSet<&'a str> {
    decision
        .outcomes
        .iter()
        .filter(|(_, outcome)| is_new_value(**outcome))
        .flat_map(|(slot, _)| slot_keys(resolved, slot))
        .collect()
}

/// Whether the current status already reflects `metadata.generation`. A
/// missing status has observed nothing.
fn generation_observed(spec: &SecretSpec) -> bool {
    spec.status
        .as_ref()
        .is_some_and(|status| status.observed_generation == spec.metadata.generation)
}

fn join(keys: &BTreeSet<&str>) -> String {
    keys.iter().copied().collect::<Vec<_>>().join(", ")
}

struct EventInput<'a> {
    name: &'a str,
    resolved: &'a ResolvedSpec,
    decision: &'a Decision,
    /// A Secret existed before this plan.
    existed: bool,
    owner: &'a Ownership,
    schedule: &'a RotationPlan,
    regenerated: &'a BTreeSet<&'a str>,
}

fn events(input: &EventInput<'_>) -> Vec<PlannedEvent> {
    let &EventInput {
        name,
        resolved,
        decision,
        existed,
        owner,
        schedule,
        regenerated,
    } = input;
    let mut events = Vec::new();

    if !existed {
        events.push(PlannedEvent::normal(
            EVENT_GENERATED,
            format!("generated keys: {}", join(regenerated)),
        ));
    } else if schedule.due {
        events.push(PlannedEvent::normal(
            EVENT_ROTATED,
            format!("rotated keys: {}", join(regenerated)),
        ));
    } else if !regenerated.is_empty() {
        events.push(PlannedEvent::normal(
            EVENT_REGENERATED,
            format!("regenerated keys: {}", join(regenerated)),
        ));
    }

    if matches!(owner, Ownership::Adoptable) {
        events.push(PlannedEvent::normal(
            EVENT_ADOPTED,
            format!("adopted existing Secret {name}"),
        ));
    }

    if decision.fingerprints_reset() {
        let keys: BTreeSet<&str> = decision
            .reset_slots
            .iter()
            .flat_map(|slot| slot_keys(resolved, slot))
            .collect();
        events.push(PlannedEvent::warning(
            EVENT_FINGERPRINTS_RESET,
            format!(
                "kept existing values for keys: {} (fingerprint metadata was missing or invalid)",
                join(&keys)
            ),
        ));
    }

    if schedule.anchor_reset {
        events.push(PlannedEvent::warning(
            EVENT_ANCHOR_RESET,
            format!(
                "annotation {ANNOTATION_GENERATED_AT} was not a valid RFC 3339 time; the rotation anchor was reset to {}",
                whole_seconds(schedule.anchor)
            ),
        ));
    }

    let low: BTreeSet<&str> = resolved
        .entropy_bits()
        .iter()
        .filter(|(_, bits)| *bits < LOW_ENTROPY_BITS)
        // Only values written by this plan; borrow the name from `regenerated`.
        .filter_map(|(key, _)| regenerated.get(key.as_str()).copied())
        .collect();
    if !low.is_empty() {
        events.push(PlannedEvent::warning(
            EVENT_LOW_ENTROPY,
            format!("keys with less than 128 bits of entropy: {}", join(&low)),
        ));
    }

    if let ResolvedKind::BasicAuth(basic) = &resolved.kind {
        let password = &basic.password;
        let bytes = password.prefix.len() + usize::from(password.length) + password.suffix.len();
        let new = decision
            .outcomes
            .get(&Slot::BasicAuthPassword)
            .is_some_and(|outcome| is_new_value(*outcome));
        if new && bytes > BCRYPT_MAX_PASSWORD_BYTES {
            events.push(PlannedEvent::warning(
                EVENT_PASSWORD_TRUNCATED,
                format!(
                    "password for key {} is longer than 72 bytes; the htpasswd hash covers only the first 72 bytes",
                    basic.password_key
                ),
            ));
        }
    }

    events
}
