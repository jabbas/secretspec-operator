// SPDX-License-Identifier: Apache-2.0

//! Per-slot keep / regenerate / re-derive decisions (normative rules 1-9).

// Tests report through panics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::{BTreeMap, BTreeSet};

use secretspec_api::constants::BCRYPT_COST;
use secretspec_api::resolve::{ResolvedSpec, SshAlgorithmParams, resolve};
use secretspec_api::v1alpha1::{EcdsaCurve, SecretSpecSpec};
use secretspec_core::generate::{
    generate_ssh, htpasswd_line, htpasswd_verifies, private_key_matches, public_key_line,
};
use secretspec_core::plan::decide::{Decision, SlotOutcome, decide};
use secretspec_core::plan::inventory::{Anchor, Inventory};
use secretspec_core::plan::keys::{KeyMap, Role};
use secretspec_core::{
    Fingerprint, FingerprintMap, SecretValue, Slot, canonical_htpasswd, expected_fingerprints,
    fingerprint_of,
};
use support::seeded_rng;

const OTHER_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn spec(yaml: &str) -> ResolvedSpec {
    let spec: SecretSpecSpec = serde_norway::from_str(yaml).expect("manifest deserializes");
    resolve(&spec, "n", "ns").expect("valid spec")
}

fn first(spec: &ResolvedSpec, seed: u64) -> Decision {
    decide(spec, None, false, &mut seeded_rng(seed)).expect("generation succeeds")
}

/// The inventory of the Secret that `decision` for `spec` would have written.
fn written(spec: &ResolvedSpec, decision: &Decision) -> Inventory {
    Inventory {
        data: decision
            .data
            .iter()
            .map(|(k, v)| (k.clone(), v.expose().to_vec()))
            .collect(),
        fingerprints: Some(
            FingerprintMap::from_annotation(&decision.fingerprints.to_annotation())
                .expect("written annotation parses"),
        ),
        keys: KeyMap::for_spec(spec),
        generated_at: Anchor::Absent,
        secret_type: "Opaque".to_owned(),
    }
}

fn again(spec: &ResolvedSpec, inv: &Inventory, seed: u64) -> Decision {
    decide(spec, Some(inv), false, &mut seeded_rng(seed)).expect("generation succeeds")
}

fn value<'a>(d: &'a Decision, key: &str) -> &'a [u8] {
    d.data
        .get(key)
        .map(SecretValue::expose)
        .expect("key present")
}

fn field(name: &str) -> Slot {
    Slot::Field(name.to_owned())
}

fn all(d: &Decision, outcome: SlotOutcome) -> bool {
    d.outcomes.values().all(|o| *o == outcome)
}

fn htpasswd_fp(username: &str, password: &[u8], line: &[u8]) -> Fingerprint {
    fingerprint_of(&canonical_htpasswd(
        username,
        &SecretValue::new(password.to_vec()),
        &SecretValue::new(line.to_vec()),
        BCRYPT_COST,
    ))
}

/// Expected fingerprints plus the htpasswd fingerprint of the written line.
fn expected_with_htpasswd(
    spec: &ResolvedSpec,
    d: &Decision,
    user: &str,
) -> BTreeMap<Slot, Fingerprint> {
    let mut want = expected_fingerprints(spec);
    want.insert(
        Slot::BasicAuthHtpasswd,
        htpasswd_fp(user, value(d, "password"), value(d, "auth")),
    );
    want
}

const FIELDS: &str = "fields: [{name: a}, {name: b, bytes: 16, encoding: hex}]";
const BASIC: &str = "basicAuth: {username: admin}";

// --- Rule 1: locating current values ------------------------------------

#[test]
fn rule_1_fields_are_located_by_name() {
    let s = spec(FIELDS);
    let d1 = first(&s, 1);
    let d2 = again(&s, &written(&s, &d1), 2);
    assert_eq!(value(&d2, "a"), value(&d1, "a"));
    assert_eq!(value(&d2, "b"), value(&d1, "b"));
}

#[test]
fn rule_1_renamed_role_key_moves_value() {
    let s1 = spec(BASIC);
    let d1 = first(&s1, 1);
    let inv = written(&s1, &d1);
    let s2 = spec("basicAuth: {username: admin, keys: {password: pw, htpasswd: users}}");
    let d2 = again(&s2, &inv, 2);
    assert_eq!(value(&d2, "pw"), value(&d1, "password"));
    assert_eq!(value(&d2, "users"), value(&d1, "auth"));
    assert!(!d2.data.contains_key("password"));
    assert!(!d2.data.contains_key("auth"));
    assert_eq!(value(&d2, "username"), b"admin");
    assert!(all(&d2, SlotOutcome::Kept), "{:?}", d2.outcomes);
    assert_eq!(d2.reset_slots, BTreeSet::new());
}

#[test]
fn rule_1_htpasswd_rename_without_key_map_keeps_password_and_rederives_line() {
    let s1 = spec(BASIC);
    let d1 = first(&s1, 1);
    let mut inv = written(&s1, &d1);
    // No `keys` annotation: the line cannot be found under its new name.
    inv.keys = KeyMap::default();
    let s2 = spec("basicAuth: {username: admin, keys: {htpasswd: users}}");
    let d2 = again(&s2, &inv, 2);
    assert_eq!(d2.outcomes[&Slot::BasicAuthPassword], SlotOutcome::Kept);
    assert_eq!(
        d2.outcomes[&Slot::BasicAuthHtpasswd],
        SlotOutcome::Rederived
    );
    assert_eq!(value(&d2, "password"), value(&d1, "password"));
    assert!(!d2.data.contains_key("auth"));
    let password = SecretValue::new(value(&d2, "password").to_vec());
    assert!(htpasswd_verifies(value(&d2, "users"), "admin", &password));
    assert_eq!(d2.reset_slots, BTreeSet::new());
}

#[test]
fn rule_1_role_falls_back_to_spec_key_when_recorded_key_is_absent() {
    let s = spec("sshKeyPair: {}");
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    // The annotation points at keys that are not in the data.
    inv.keys = KeyMap(
        [
            (Role::SshPrivateKey, "gone".to_owned()),
            (Role::SshPublicKey, "gone-too".to_owned()),
        ]
        .into(),
    );
    let d2 = again(&s, &inv, 2);
    assert_eq!(value(&d2, "ssh-privatekey"), value(&d1, "ssh-privatekey"));
    assert_eq!(d2.outcomes[&Slot::SshKeyPair], SlotOutcome::Kept);
}

#[test]
fn rule_1_recorded_key_wins_over_spec_key() {
    let s = spec(BASIC);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    // Value moved to "old" by an earlier spec; a stale "password" key lingers.
    let password = inv.data.remove("password").unwrap();
    inv.data.insert("old".to_owned(), password.clone());
    inv.data.insert("password".to_owned(), b"stale".to_vec());
    inv.keys.0.insert(Role::BasicAuthPassword, "old".to_owned());
    let d2 = again(&s, &inv, 2);
    assert_eq!(value(&d2, "password"), password.as_slice());
    assert_eq!(d2.outcomes[&Slot::BasicAuthPassword], SlotOutcome::Kept);
}

// --- Rule 2: force_all ----------------------------------------------------

#[test]
fn rule_2_no_secret_generates_every_slot() {
    for yaml in [FIELDS, "sshKeyPair: {}", BASIC] {
        let s = spec(yaml);
        let d = first(&s, 1);
        assert!(all(&d, SlotOutcome::Generated), "{yaml}: {:?}", d.outcomes);
        assert_eq!(d.reset_slots, BTreeSet::new());
    }
}

#[test]
fn rule_2_force_all_regenerates_every_slot() {
    for yaml in [FIELDS, "sshKeyPair: {}", BASIC] {
        let s = spec(yaml);
        let d1 = first(&s, 1);
        let d2 = decide(&s, Some(&written(&s, &d1)), true, &mut seeded_rng(2)).unwrap();
        assert!(
            all(&d2, SlotOutcome::Regenerated),
            "{yaml}: {:?}",
            d2.outcomes
        );
        assert_eq!(d2.reset_slots, BTreeSet::new());
        for (key, v) in &d1.data {
            if key == "username" {
                continue;
            }
            assert_ne!(value(&d2, key), v.expose(), "{yaml}: {key} unchanged");
        }
    }
}

// --- Rule 3: missing value ---------------------------------------------------

#[test]
fn rule_3_missing_value_is_regenerated() {
    let s = spec(FIELDS);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.data.remove("a");
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&field("a")], SlotOutcome::Regenerated);
    assert_eq!(d2.outcomes[&field("b")], SlotOutcome::Kept);
    assert_ne!(value(&d2, "a"), value(&d1, "a"));
    assert_eq!(value(&d2, "b"), value(&d1, "b"));
}

#[test]
fn rule_3_missing_ssh_private_key_regenerates_pair() {
    let s = spec("sshKeyPair: {}");
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.data.remove("ssh-privatekey");
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&Slot::SshKeyPair], SlotOutcome::Regenerated);
    assert_ne!(value(&d2, "ssh-publickey"), value(&d1, "ssh-publickey"));
}

// --- Rule 4: matching fingerprint keeps ------------------------------------------

#[test]
fn rule_4_matching_fingerprints_keep_everything() {
    for yaml in [FIELDS, "sshKeyPair: {}", BASIC] {
        let s = spec(yaml);
        let d1 = first(&s, 1);
        let d2 = again(&s, &written(&s, &d1), 2);
        assert!(all(&d2, SlotOutcome::Kept), "{yaml}: {:?}", d2.outcomes);
        assert_eq!(d2.reset_slots, BTreeSet::new());
        assert_eq!(d2.fingerprints, d1.fingerprints);
        assert_eq!(d2.data.len(), d1.data.len());
        for (key, v) in &d1.data {
            assert_eq!(value(&d2, key), v.expose(), "{yaml}: {key} changed");
        }
    }
}

// --- Rule 5: changed fingerprint regenerates ----------------------------------

#[test]
fn rule_5_changed_parameters_regenerate_only_that_slot() {
    let s1 = spec(FIELDS);
    let d1 = first(&s1, 1);
    let s2 = spec("fields: [{name: a, length: 20}, {name: b, bytes: 16, encoding: hex}]");
    let d2 = again(&s2, &written(&s1, &d1), 2);
    assert_eq!(d2.outcomes[&field("a")], SlotOutcome::Regenerated);
    assert_eq!(d2.outcomes[&field("b")], SlotOutcome::Kept);
    assert_eq!(value(&d2, "a").len(), 20);
    assert_eq!(value(&d2, "b"), value(&d1, "b"));
    assert_eq!(d2.fingerprints.slots, expected_fingerprints(&s2));
}

#[test]
fn rule_5_foreign_fingerprint_regenerates() {
    let s = spec(FIELDS);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    let parsed = inv.fingerprints.as_mut().unwrap();
    parsed
        .map
        .slots
        .insert(field("a"), Fingerprint::parse(OTHER_FP).unwrap());
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&field("a")], SlotOutcome::Regenerated);
    assert_eq!(d2.outcomes[&field("b")], SlotOutcome::Kept);
    assert_eq!(d2.reset_slots, BTreeSet::new());
}

#[test]
fn rule_5_ssh_algorithm_change_regenerates_pair() {
    let s1 = spec("sshKeyPair: {}");
    let d1 = first(&s1, 1);
    let s2 = spec("sshKeyPair: {algorithm: ecdsa, ecdsaCurve: P-256}");
    let d2 = again(&s2, &written(&s1, &d1), 2);
    assert_eq!(d2.outcomes[&Slot::SshKeyPair], SlotOutcome::Regenerated);
    assert!(private_key_matches(
        value(&d2, "ssh-privatekey"),
        SshAlgorithmParams::Ecdsa {
            curve: EcdsaCurve::P256
        }
    ));
}

// --- Rule 6: absent or invalid fingerprint keeps and stamps ---------------

#[test]
fn rule_6_absent_annotation_keeps_and_stamps() {
    for yaml in [FIELDS, "sshKeyPair: {}", BASIC] {
        let s = spec(yaml);
        let d1 = first(&s, 1);
        let mut inv = written(&s, &d1);
        inv.fingerprints = None;
        let d2 = again(&s, &inv, 2);
        assert!(all(&d2, SlotOutcome::Kept), "{yaml}: {:?}", d2.outcomes);
        assert!(d2.fingerprints_reset(), "{yaml}");
        let every: BTreeSet<Slot> = d2.outcomes.keys().cloned().collect();
        assert_eq!(d2.reset_slots, every, "{yaml}");
        assert_eq!(d2.fingerprints, d1.fingerprints, "{yaml}");
        for (key, v) in &d1.data {
            assert_eq!(value(&d2, key), v.expose(), "{yaml}: {key} changed");
        }
    }
}

#[test]
fn rule_6_missing_slot_entry_keeps_and_stamps() {
    let s = spec(FIELDS);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.fingerprints
        .as_mut()
        .unwrap()
        .map
        .slots
        .remove(&field("a"));
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&field("a")], SlotOutcome::Kept);
    assert_eq!(value(&d2, "a"), value(&d1, "a"));
    assert_eq!(d2.reset_slots, BTreeSet::from([field("a")]));
    assert_eq!(d2.fingerprints.slots, expected_fingerprints(&s));
}

#[test]
fn rule_6_empty_annotation_object_keeps_and_stamps() {
    for yaml in [FIELDS, "sshKeyPair: {}", BASIC] {
        let s = spec(yaml);
        let d1 = first(&s, 1);
        let mut inv = written(&s, &d1);
        let parsed = FingerprintMap::from_annotation("{}").unwrap();
        assert_eq!(parsed.problems.len(), 0);
        inv.fingerprints = Some(parsed);
        let d2 = again(&s, &inv, 2);
        assert!(all(&d2, SlotOutcome::Kept), "{yaml}: {:?}", d2.outcomes);
        let every: BTreeSet<Slot> = d2.outcomes.keys().cloned().collect();
        assert_eq!(d2.reset_slots, every, "{yaml}");
        assert_eq!(d2.fingerprints, d1.fingerprints, "{yaml}");
        for (key, v) in &d1.data {
            assert_eq!(value(&d2, key), v.expose(), "{yaml}: {key} changed");
        }
    }
}

#[test]
fn rule_6_invalid_slot_entry_keeps_and_stamps() {
    let s = spec(FIELDS);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    let b = expected_fingerprints(&s)[&field("b")].clone();
    let text = format!(r#"{{"field/a":"NOT-HEX","field/b":"{b}"}}"#);
    let parsed = FingerprintMap::from_annotation(&text).unwrap();
    assert_eq!(parsed.problems.len(), 1);
    inv.fingerprints = Some(parsed);
    let d2 = again(&s, &inv, 2);
    assert!(all(&d2, SlotOutcome::Kept), "{:?}", d2.outcomes);
    assert_eq!(d2.reset_slots, BTreeSet::from([field("a")]));
    assert_eq!(d2.fingerprints.slots, expected_fingerprints(&s));
}

#[test]
fn rule_6_absent_fingerprint_with_missing_value_regenerates_without_reset() {
    let s = spec(FIELDS);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.fingerprints = None;
    inv.data.remove("a");
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&field("a")], SlotOutcome::Regenerated);
    assert_eq!(d2.outcomes[&field("b")], SlotOutcome::Kept);
    assert_eq!(
        d2.reset_slots,
        BTreeSet::from([field("b")]),
        "b was kept without a fingerprint"
    );
}

// --- Rule 7: SSH -------------------------------------------------------

#[test]
fn rule_7_comment_change_rederives_public_line() {
    let s1 = spec("sshKeyPair: {comment: old}");
    let d1 = first(&s1, 1);
    let s2 = spec("sshKeyPair: {comment: new}");
    let d2 = again(&s2, &written(&s1, &d1), 2);
    assert_eq!(d2.outcomes[&Slot::SshKeyPair], SlotOutcome::Rederived);
    assert_eq!(value(&d2, "ssh-privatekey"), value(&d1, "ssh-privatekey"));
    let want = public_key_line(value(&d1, "ssh-privatekey"), "new").unwrap();
    assert_eq!(value(&d2, "ssh-publickey"), want.as_bytes());
    assert!(want.ends_with(" new\n"));
    assert_eq!(d2.reset_slots, BTreeSet::new());
}

#[test]
fn rule_7_missing_public_key_is_rederived() {
    let s = spec("sshKeyPair: {}");
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.data.remove("ssh-publickey");
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&Slot::SshKeyPair], SlotOutcome::Rederived);
    assert_eq!(value(&d2, "ssh-publickey"), value(&d1, "ssh-publickey"));
}

#[test]
fn rule_7_tampered_public_key_is_rederived() {
    let s = spec("sshKeyPair: {}");
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.data.insert(
        "ssh-publickey".to_owned(),
        b"ssh-ed25519 AAAA bogus\n".to_vec(),
    );
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&Slot::SshKeyPair], SlotOutcome::Rederived);
    assert_eq!(value(&d2, "ssh-publickey"), value(&d1, "ssh-publickey"));
}

#[test]
fn rule_7_unparseable_private_key_regenerates_pair() {
    let s = spec("sshKeyPair: {}");
    let d1 = first(&s, 1);
    for fingerprints_lost in [false, true] {
        let mut inv = written(&s, &d1);
        inv.data
            .insert("ssh-privatekey".to_owned(), b"not a key".to_vec());
        if fingerprints_lost {
            inv.fingerprints = None;
        }
        let d2 = again(&s, &inv, 2);
        assert_eq!(d2.outcomes[&Slot::SshKeyPair], SlotOutcome::Regenerated);
        assert!(private_key_matches(
            value(&d2, "ssh-privatekey"),
            SshAlgorithmParams::Ed25519
        ));
        // Nothing was kept, so nothing was reset.
        assert_eq!(d2.reset_slots, BTreeSet::new());
    }
}

#[test]
fn rule_7_private_key_of_wrong_algorithm_regenerates_pair() {
    let s = spec("sshKeyPair: {}");
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    // A P-256 key stored under a (correct) ed25519 fingerprint.
    let foreign = generate_ssh(
        SshAlgorithmParams::Ecdsa {
            curve: EcdsaCurve::P256,
        },
        "",
        &mut seeded_rng(9),
    )
    .unwrap();
    inv.data.insert(
        "ssh-privatekey".to_owned(),
        foreign.private_key.expose().to_vec(),
    );
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&Slot::SshKeyPair], SlotOutcome::Regenerated);
    assert!(private_key_matches(
        value(&d2, "ssh-privatekey"),
        SshAlgorithmParams::Ed25519
    ));
    // The resolved default comment is `<name>@<namespace>`.
    let public = public_key_line(value(&d2, "ssh-privatekey"), "n@ns").unwrap();
    assert_eq!(value(&d2, "ssh-publickey"), public.as_bytes());
}

// --- Rule 8: basicAuth --------------------------------------------------------

#[test]
fn rule_8_new_basic_auth_line_verifies_and_is_fingerprinted() {
    let s = spec(BASIC);
    let d = first(&s, 1);
    let password = SecretValue::new(value(&d, "password").to_vec());
    assert!(htpasswd_verifies(value(&d, "auth"), "admin", &password));
    assert_eq!(
        d.fingerprints.slots,
        expected_with_htpasswd(&s, &d, "admin")
    );
    assert_eq!(value(&d, "username"), b"admin");
}

#[test]
fn rule_8_username_change_keeps_password_and_rederives_line() {
    let s1 = spec(BASIC);
    let d1 = first(&s1, 1);
    let s2 = spec("basicAuth: {username: root}");
    let d2 = again(&s2, &written(&s1, &d1), 2);
    assert_eq!(d2.outcomes[&Slot::BasicAuthPassword], SlotOutcome::Kept);
    assert_eq!(
        d2.outcomes[&Slot::BasicAuthHtpasswd],
        SlotOutcome::Rederived
    );
    assert_eq!(value(&d2, "password"), value(&d1, "password"));
    assert_eq!(value(&d2, "username"), b"root");
    let password = SecretValue::new(value(&d2, "password").to_vec());
    assert!(htpasswd_verifies(value(&d2, "auth"), "root", &password));
    assert_eq!(
        d2.fingerprints.slots,
        expected_with_htpasswd(&s2, &d2, "root")
    );
    assert_eq!(d2.reset_slots, BTreeSet::new());
}

#[test]
fn rule_8_password_change_regenerates_line() {
    let s1 = spec(BASIC);
    let d1 = first(&s1, 1);
    let s2 = spec("basicAuth: {username: admin, password: {length: 40}}");
    let d2 = again(&s2, &written(&s1, &d1), 2);
    assert_eq!(
        d2.outcomes[&Slot::BasicAuthPassword],
        SlotOutcome::Regenerated
    );
    assert_eq!(
        d2.outcomes[&Slot::BasicAuthHtpasswd],
        SlotOutcome::Regenerated
    );
    let password = SecretValue::new(value(&d2, "password").to_vec());
    assert!(htpasswd_verifies(value(&d2, "auth"), "admin", &password));
    assert_eq!(
        d2.fingerprints.slots,
        expected_with_htpasswd(&s2, &d2, "admin")
    );
}

#[test]
fn rule_8_lost_htpasswd_fingerprint_keeps_verifying_line() {
    let s = spec(BASIC);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.fingerprints
        .as_mut()
        .unwrap()
        .map
        .slots
        .remove(&Slot::BasicAuthHtpasswd);
    let d2 = again(&s, &inv, 2);
    assert!(all(&d2, SlotOutcome::Kept), "{:?}", d2.outcomes);
    assert_eq!(value(&d2, "auth"), value(&d1, "auth"));
    assert_eq!(d2.reset_slots, BTreeSet::from([Slot::BasicAuthHtpasswd]));
    assert_eq!(d2.fingerprints, d1.fingerprints);
}

#[test]
fn rule_8_lost_htpasswd_fingerprint_with_bad_line_rederives() {
    let s = spec(BASIC);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.fingerprints
        .as_mut()
        .unwrap()
        .map
        .slots
        .remove(&Slot::BasicAuthHtpasswd);
    // A valid line for another password.
    let other = htpasswd_line(
        "admin",
        &SecretValue::new(b"x".to_vec()),
        &mut seeded_rng(5),
    )
    .unwrap();
    inv.data.insert("auth".to_owned(), other.expose().to_vec());
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&Slot::BasicAuthPassword], SlotOutcome::Kept);
    assert_eq!(
        d2.outcomes[&Slot::BasicAuthHtpasswd],
        SlotOutcome::Rederived
    );
    let password = SecretValue::new(value(&d2, "password").to_vec());
    assert!(htpasswd_verifies(value(&d2, "auth"), "admin", &password));
    assert_eq!(
        d2.fingerprints.slots,
        expected_with_htpasswd(&s, &d2, "admin")
    );
}

#[test]
fn rule_8_stale_htpasswd_fingerprint_rederives_even_if_line_verifies() {
    let s = spec(BASIC);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.fingerprints.as_mut().unwrap().map.slots.insert(
        Slot::BasicAuthHtpasswd,
        Fingerprint::parse(OTHER_FP).unwrap(),
    );
    let d2 = again(&s, &inv, 2);
    assert_eq!(
        d2.outcomes[&Slot::BasicAuthHtpasswd],
        SlotOutcome::Rederived
    );
    assert_ne!(value(&d2, "auth"), value(&d1, "auth"));
    // The stale fingerprint is a mismatch, not lost metadata: the password
    // is untouched and nothing is reported as reset.
    assert_eq!(d2.outcomes[&Slot::BasicAuthPassword], SlotOutcome::Kept);
    assert_eq!(value(&d2, "password"), value(&d1, "password"));
    assert_eq!(d2.reset_slots, BTreeSet::new());
}

#[test]
fn rule_8_missing_htpasswd_line_is_rederived() {
    let s = spec(BASIC);
    let d1 = first(&s, 1);
    let mut inv = written(&s, &d1);
    inv.data.remove("auth");
    let d2 = again(&s, &inv, 2);
    assert_eq!(d2.outcomes[&Slot::BasicAuthPassword], SlotOutcome::Kept);
    assert_eq!(
        d2.outcomes[&Slot::BasicAuthHtpasswd],
        SlotOutcome::Rederived
    );
    assert_eq!(
        d2.fingerprints.slots,
        expected_with_htpasswd(&s, &d2, "admin")
    );
}

// --- Rule 9: unknown and stale fingerprints ------------------------------------

#[test]
fn rule_9_unknown_entries_carried_and_stale_slots_dropped() {
    let s1 = spec(FIELDS);
    let d1 = first(&s1, 1);
    let mut inv = written(&s1, &d1);
    inv.fingerprints
        .as_mut()
        .unwrap()
        .map
        .unknown
        .insert("future/slot".to_owned(), "opaque".to_owned());
    let s2 = spec("fields: [{name: a}]");
    let d2 = again(&s2, &inv, 2);
    assert_eq!(
        d2.fingerprints.unknown,
        BTreeMap::from([("future/slot".to_owned(), "opaque".to_owned())])
    );
    assert_eq!(d2.fingerprints.slots, expected_fingerprints(&s2));
    assert!(!d2.fingerprints.slots.contains_key(&field("b")));
    assert!(!d2.data.contains_key("b"));
    assert_eq!(d2.outcomes.keys().collect::<Vec<_>>(), vec![&field("a")]);
}

// --- Extras ------------------------------------------------------------------

#[test]
fn outcomes_cover_every_slot() {
    let cases: [(&str, Vec<Slot>); 3] = [
        (FIELDS, vec![field("a"), field("b")]),
        ("sshKeyPair: {}", vec![Slot::SshKeyPair]),
        (
            BASIC,
            vec![Slot::BasicAuthPassword, Slot::BasicAuthHtpasswd],
        ),
    ];
    for (yaml, slots) in cases {
        let d = first(&spec(yaml), 1);
        let mut got: Vec<Slot> = d.outcomes.keys().cloned().collect();
        let mut want = slots;
        got.sort();
        want.sort();
        assert_eq!(got, want, "{yaml}");
    }
}

#[test]
fn literals_are_not_part_of_the_decision() {
    let s = spec("data: {user: app}\nfields: [{name: password}]");
    let d = first(&s, 1);
    assert_eq!(d.data.keys().collect::<Vec<_>>(), vec!["password"]);
}

#[test]
fn debug_output_never_contains_values() {
    for yaml in [FIELDS, "sshKeyPair: {}", BASIC] {
        let d = first(&spec(yaml), 1);
        let debug = format!("{d:?}");
        for (key, v) in &d.data {
            if key == "username" {
                continue;
            }
            let text = String::from_utf8_lossy(v.expose());
            // The longest token: the whole value, or the key material body.
            let needle = text
                .split_whitespace()
                .max_by_key(|token| token.len())
                .unwrap();
            assert!(!debug.contains(needle), "{yaml}: {key} leaked");
        }
    }
}
