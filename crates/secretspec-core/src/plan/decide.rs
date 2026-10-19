// SPDX-License-Identifier: Apache-2.0

//! Per-slot keep / regenerate / re-derive decisions.
//!
//! This is where the operator's central promise is kept: a stored value is
//! replaced only when its slot's fingerprint says the spec asks for something
//! else, when the value is missing, or when a full rotation is due. Lost or
//! damaged fingerprint metadata never causes a regeneration; the present value
//! is kept and the expected fingerprint is stamped again.

use std::collections::{BTreeMap, BTreeSet};

use rand_core::CryptoRngCore;
use secretspec_api::constants::BCRYPT_COST;
use secretspec_api::resolve::{
    Generator, ResolvedBasicAuth, ResolvedField, ResolvedKind, ResolvedSpec, ResolvedSsh,
};

use crate::fingerprint::{
    Fingerprint, FingerprintMap, Slot, canonical_htpasswd, expected_fingerprints, fingerprint_of,
};
use crate::generate::{
    GenerateError, generate_bytes, generate_chars, generate_ssh, htpasswd_line, htpasswd_verifies,
    private_key_matches, public_key_line,
};
use crate::plan::inventory::Inventory;
use crate::plan::keys::Role;
use crate::value::SecretValue;

/// What happened to one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotOutcome {
    /// The stored value is kept unchanged.
    Kept,
    /// Generated because there was no Secret yet.
    Generated,
    /// Replaced by a newly generated value.
    Regenerated,
    /// Recomputed from kept secret material: the SSH public line was
    /// rewritten, or the htpasswd line was recomputed for the same password.
    Rederived,
}

/// The generated values and fingerprints the managed Secret must carry.
#[derive(Debug)]
pub struct Decision {
    /// Every generated key (not the literals) under its spec key name.
    pub data: BTreeMap<String, SecretValue>,
    /// Expected fingerprints plus the htpasswd fingerprint; unknown entries
    /// of the stored annotation are carried over.
    pub fingerprints: FingerprintMap,
    /// The outcome of every slot in the spec.
    pub outcomes: BTreeMap<Slot, SlotOutcome>,
    /// The slots whose present value was kept although their stored
    /// fingerprint was missing or invalid; the expected fingerprint was
    /// stamped again.
    pub reset_slots: BTreeSet<Slot>,
}

impl Decision {
    /// Some present value was kept although its fingerprint was missing or
    /// invalid.
    #[must_use]
    pub fn fingerprints_reset(&self) -> bool {
        !self.reset_slots.is_empty()
    }
}

/// Decides every slot of `spec` against the existing Secret `inv`.
///
/// `force_all` (a rotation is due) regenerates every slot; so does
/// `inv == None` (there is no Secret yet). The rules are normative; see the
/// design document, section 4.
///
/// # Errors
/// A generator failed. The error never contains secret material.
pub fn decide<R: CryptoRngCore + ?Sized>(
    spec: &ResolvedSpec,
    inv: Option<&Inventory>,
    force_all: bool,
    rng: &mut R,
) -> Result<Decision, GenerateError> {
    let mut decider = Decider {
        inv,
        force_all,
        expected: expected_fingerprints(spec),
        decision: Decision {
            data: BTreeMap::new(),
            fingerprints: FingerprintMap::default(),
            outcomes: BTreeMap::new(),
            reset_slots: BTreeSet::new(),
        },
    };
    match &spec.kind {
        ResolvedKind::Fields(fields) => {
            for field in fields {
                decider.field(field, rng)?;
            }
        }
        ResolvedKind::SshKeyPair(ssh) => decider.ssh(ssh, rng)?,
        ResolvedKind::BasicAuth(basic) => decider.basic_auth(basic, rng)?,
    }
    Ok(decider.finish())
}

/// The verdict of rules 2-6 for one slot.
enum Verdict<'a> {
    /// Keep the stored value; `reset` when its fingerprint was missing.
    Keep { value: &'a [u8], reset: bool },
    /// Generate a new value, reported with this outcome.
    Generate(SlotOutcome),
}

struct Decider<'a> {
    inv: Option<&'a Inventory>,
    force_all: bool,
    /// Expected fingerprints; becomes the output `slots` (rule 9: anything
    /// not in the spec is dropped).
    expected: BTreeMap<Slot, Fingerprint>,
    decision: Decision,
}

impl<'a> Decider<'a> {
    /// The stored fingerprint of `slot`, if present and valid (rule 6: an
    /// absent annotation, a missing entry and an invalid entry all read as
    /// `None`; invalid entries are never in `map.slots`).
    fn stored_fingerprint(&self, slot: &Slot) -> Option<&'a Fingerprint> {
        self.inv?.fingerprints.as_ref()?.map.slots.get(slot)
    }

    /// Rule 1 for role-based slots: the key recorded for `role` when it holds
    /// a value, else the spec's key name.
    fn role_value(&self, role: Role, spec_key: &str) -> Option<&'a [u8]> {
        let inv = self.inv?;
        inv.keys
            .0
            .get(&role)
            .and_then(|key| inv.data.get(key))
            .or_else(|| inv.data.get(spec_key))
            .map(SecretValue::expose)
    }

    /// Rules 2-6.
    fn verdict(&self, slot: &Slot, value: Option<&'a [u8]>) -> Verdict<'a> {
        if self.inv.is_none() {
            return Verdict::Generate(SlotOutcome::Generated);
        }
        if self.force_all {
            return Verdict::Generate(SlotOutcome::Regenerated);
        }
        let Some(value) = value else {
            return Verdict::Generate(SlotOutcome::Regenerated);
        };
        match self.stored_fingerprint(slot) {
            Some(stored) if self.expected.get(slot) == Some(stored) => Verdict::Keep {
                value,
                reset: false,
            },
            Some(_) => Verdict::Generate(SlotOutcome::Regenerated),
            None => Verdict::Keep { value, reset: true },
        }
    }

    fn put(&mut self, key: &str, value: SecretValue) {
        self.decision.data.insert(key.to_owned(), value);
    }

    fn record(&mut self, slot: Slot, outcome: SlotOutcome, reset: bool) {
        if reset {
            self.decision.reset_slots.insert(slot.clone());
        }
        self.decision.outcomes.insert(slot, outcome);
    }

    fn field<R: CryptoRngCore + ?Sized>(
        &mut self,
        field: &ResolvedField,
        rng: &mut R,
    ) -> Result<(), GenerateError> {
        let slot = Slot::Field(field.name.clone());
        let stored = self
            .inv
            .and_then(|inv| inv.data.get(&field.name))
            .map(SecretValue::expose);
        let (value, outcome, reset) = match self.verdict(&slot, stored) {
            Verdict::Keep { value, reset } => {
                (SecretValue::new(value.to_vec()), SlotOutcome::Kept, reset)
            }
            Verdict::Generate(outcome) => {
                let value = match &field.generator {
                    Generator::Chars(p) => generate_chars(p, rng)?,
                    Generator::Bytes(p) => generate_bytes(p, rng)?,
                };
                (value, outcome, false)
            }
        };
        self.put(&field.name, value);
        self.record(slot, outcome, reset);
        Ok(())
    }

    /// Rule 7.
    fn ssh<R: CryptoRngCore + ?Sized>(
        &mut self,
        ssh: &ResolvedSsh,
        rng: &mut R,
    ) -> Result<(), GenerateError> {
        let stored_private = self.role_value(Role::SshPrivateKey, &ssh.private_key_key);
        let stored_public = self.role_value(Role::SshPublicKey, &ssh.public_key_key);
        let new_outcome = match self.verdict(&Slot::SshKeyPair, stored_private) {
            Verdict::Keep { value, reset } => {
                // A kept private key must still be usable for this algorithm;
                // otherwise the pair is regenerated.
                let public = if private_key_matches(value, ssh.algorithm) {
                    public_key_line(value, &ssh.comment).ok()
                } else {
                    None
                };
                if let Some(public) = public {
                    let outcome = if stored_public == Some(public.as_bytes()) {
                        SlotOutcome::Kept
                    } else {
                        SlotOutcome::Rederived
                    };
                    self.put(&ssh.private_key_key, SecretValue::new(value.to_vec()));
                    self.put(&ssh.public_key_key, SecretValue::new(public.into_bytes()));
                    self.record(Slot::SshKeyPair, outcome, reset);
                    return Ok(());
                }
                SlotOutcome::Regenerated
            }
            Verdict::Generate(outcome) => outcome,
        };
        // `&mut R` is a sized `CryptoRngCore` for ssh-key's generic parameters.
        let mut rng = rng;
        let pair = generate_ssh(ssh.algorithm, &ssh.comment, &mut rng)?;
        let public = public_key_line(pair.private_key.expose(), &ssh.comment)?;
        self.put(&ssh.private_key_key, pair.private_key);
        self.put(&ssh.public_key_key, SecretValue::new(public.into_bytes()));
        self.record(Slot::SshKeyPair, new_outcome, false);
        Ok(())
    }

    /// Rule 8.
    fn basic_auth<R: CryptoRngCore + ?Sized>(
        &mut self,
        basic: &ResolvedBasicAuth,
        rng: &mut R,
    ) -> Result<(), GenerateError> {
        let username = basic.username.as_str();
        let stored_password = self.role_value(Role::BasicAuthPassword, &basic.password_key);
        let (password, password_outcome, password_reset) = match self
            .verdict(&Slot::BasicAuthPassword, stored_password)
        {
            Verdict::Keep { value, reset } => {
                (SecretValue::new(value.to_vec()), SlotOutcome::Kept, reset)
            }
            Verdict::Generate(outcome) => (generate_chars(&basic.password, rng)?, outcome, false),
        };

        let stored_line = self.role_value(Role::BasicAuthHtpasswd, &basic.htpasswd_key);
        let kept_line = match (password_outcome, stored_line) {
            (SlotOutcome::Kept, Some(line)) => {
                let line = SecretValue::new(line.to_vec());
                let fingerprint = htpasswd_fingerprint(username, &password, &line);
                match self.stored_fingerprint(&Slot::BasicAuthHtpasswd) {
                    Some(stored) if *stored == fingerprint => Some((line, fingerprint, false)),
                    None if htpasswd_verifies(line.expose(), username, &password) => {
                        Some((line, fingerprint, true))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let (line, fingerprint, line_outcome, line_reset) =
            if let Some((line, fingerprint, reset)) = kept_line {
                (line, fingerprint, SlotOutcome::Kept, reset)
            } else {
                let line = htpasswd_line(username, &password, rng)?;
                let fingerprint = htpasswd_fingerprint(username, &password, &line);
                let outcome = match password_outcome {
                    SlotOutcome::Kept => SlotOutcome::Rederived,
                    other => other,
                };
                (line, fingerprint, outcome, false)
            };

        self.put(
            &basic.username_key,
            SecretValue::new(username.as_bytes().to_vec()),
        );
        self.put(&basic.password_key, password);
        self.put(&basic.htpasswd_key, line);
        self.expected.insert(Slot::BasicAuthHtpasswd, fingerprint);
        self.record(Slot::BasicAuthPassword, password_outcome, password_reset);
        self.record(Slot::BasicAuthHtpasswd, line_outcome, line_reset);
        Ok(())
    }

    /// Rule 9: stamp the expected fingerprints and carry unknown entries.
    fn finish(self) -> Decision {
        let mut decision = self.decision;
        decision.fingerprints = FingerprintMap {
            slots: self.expected,
            unknown: self
                .inv
                .and_then(|inv| inv.fingerprints.as_ref())
                .map(|parsed| parsed.map.unknown.clone())
                .unwrap_or_default(),
        };
        decision
    }
}

/// The htpasswd fingerprint of the line actually written.
fn htpasswd_fingerprint(username: &str, password: &SecretValue, line: &SecretValue) -> Fingerprint {
    fingerprint_of(&canonical_htpasswd(username, password, line, BCRYPT_COST))
}
