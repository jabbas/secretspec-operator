# Fingerprint format (v1)

This document is **normative**. It defines how the operator decides that a
generated value is still valid or must be regenerated.

> **The format is frozen forever.** Changing any byte of the canonical text
> below — a field name, a separator, the case of a hex digit, the order of
> lines, a trailing newline — changes the fingerprint of every existing
> Secret. The operator would then see every stored fingerprint as stale and
> **rotate every credential in every cluster** on upgrade. A new format needs
> a new version header (`secretspec-fingerprint-v2`), an explicit migration
> that recognises both, and must never silently replace v1.

## Concepts

- A **slot** is a stable logical name for one generated value. Slots do not
  depend on Secret key names, so renaming `keys.*` moves a value to a new key
  without regenerating it.
- The **fingerprint** of a slot is the SHA-256 (lowercase hex, 64 characters)
  of the slot's *canonical text*, a function of the slot's **fully resolved**
  generation parameters (every default applied; see the stability rule below).
- The operator stores all fingerprints of a Secret as a JSON object in the
  annotation `secretspec.jabbas.eu/fingerprints`. A value is kept while its
  stored fingerprint equals the expected one and regenerated otherwise.

## Slots

| Slot | Meaning |
|---|---|
| `field/<name>` | The `fields[]` entry called `<name>`. `<name>` is a valid Secret key name: `[-._a-zA-Z0-9]+`, 1-253 characters, not `.`, not starting with `..`. |
| `ssh/keypair` | The SSH key pair (private and public key together). |
| `basic-auth/password` | The `basicAuth` password. |
| `basic-auth/htpasswd` | The `basicAuth` htpasswd line. |

`basic-auth/htpasswd` is value-dependent (it covers the generated password), so
it cannot be computed from the spec alone; the other slots can.

## Canonical text

Rules for every form:

- Lines are separated by a single `\n` (LF). **The last line also ends with
  `\n`.** There are no carriage returns, no spaces around `=`, no blank lines.
- Hexadecimal is **lowercase**, two digits per byte.
- An empty value is written as `key=` (nothing after the `=`).
- Integers are written in decimal without padding or sign.
- The line order is exactly as listed.
- The input of SHA-256 is the UTF-8 (in practice ASCII) bytes of the text.

### Characters mode (`field/<name>` and `basic-auth/password`)

```
secretspec-fingerprint-v1
slot=<slot>
mode=chars
length=<n>
alphabet=<hex of sorted unique alphabet bytes>
require=<lower>,<upper>,<digits>,<symbols>
prefix=<hex of prefix bytes>
suffix=<hex of suffix bytes>
```

- `<slot>` is `field/<name>` or `basic-auth/password`.
- `length` is the resolved number of random characters (without prefix/suffix).
- `alphabet` is the **effective** alphabet: the charset (or the custom
  alphabet), minus `exclude`, with duplicates removed and the bytes sorted
  ascending. The order or duplicates the user wrote therefore never matter.
- `require` is the resolved minimum count per class, `0` for an unset class.

### Random-bytes mode (`field/<name>`)

```
secretspec-fingerprint-v1
slot=field/<name>
mode=bytes
count=<n>
encoding=<hex|base64|base64url|base32>
prefix=<hex of prefix bytes>
suffix=<hex of suffix bytes>
```

`encoding` is the API wire string (`base64url`, never `Base64url`).

### SSH key pair (`ssh/keypair`)

```
secretspec-fingerprint-v1
slot=ssh/keypair
algorithm=ed25519
```

```
secretspec-fingerprint-v1
slot=ssh/keypair
algorithm=rsa
bits=<2048|3072|4096>
```

```
secretspec-fingerprint-v1
slot=ssh/keypair
algorithm=ecdsa
curve=<P-256|P-384|P-521>
```

The curve is the API wire string (`P-384`).

### htpasswd (`basic-auth/htpasswd`)

```
secretspec-fingerprint-v1
slot=basic-auth/htpasswd
username=<hex of username bytes>
password-sha256=<hex of sha256(password bytes)>
cost=<n>
```

`cost` is the bcrypt cost (currently `12`). The line is regenerated only when
the username, the password or the cost changes; bcrypt is salted, so it must
not be recomputed on every reconcile.

## Not fingerprinted

These inputs do **not** enter any canonical text. Changing them never
regenerates a value:

- the SSH `comment` (changing it rewrites only the public-key line);
- every `keys.*` name (renaming a key moves the value);
- the `data` literals;
- `deletionPolicy`;
- everything under `secret.*` (type, labels, annotations);
- `rotation` (it triggers regeneration on its own schedule, not through the
  fingerprint).

## Annotation format

`secretspec.jabbas.eu/fingerprints` is a compact JSON object (no whitespace)
from slot string to fingerprint, **keys sorted by their string form** (byte
order):

```json
{"field/a":"<64 hex>","field/b":"<64 hex>"}
```

A reader rejects the annotation if it is not a JSON object of strings, if a key
is not a known slot (see the table above), or if a value is not exactly 64
lowercase hex characters. An empty map is `{}`.

## Stability rule

Fingerprints are always computed from **fully resolved** parameters, never from
the raw spec. Every default that feeds a canonical text (`length` 32,
`charset` alphanumeric, empty `prefix`/`suffix`, `require` all zero, `rsaBits`
default, `ecdsaCurve` default, ...) is therefore part of the API-version
contract: **defaults and this format MUST NOT change within an apiVersion.**
The golden vectors below (and `crates/secretspec-core/tests/fingerprint_golden.rs`)
guard this. A mismatch is a release blocker; the expected values are never
updated to make a test pass.

## Golden test vectors

Anyone can verify an implementation with `printf '<text>' | shasum -a 256`
(`printf`, not `echo`; `\n` is a line feed). `ALNUM` below stands for the hex of
the alphanumeric alphabet:

```
ALNUM = 303132333435363738394142434445464748494a4b4c4d4e4f505152535455565758595a6162636465666768696a6b6c6d6e6f707172737475767778797a
```

(`ALNUM` is the hex of `0-9A-Za-z` in byte order. In the canonical text it is
written out in full.)

| # | Case | Canonical text (`\n` = LF) | SHA-256 |
|---|---|---|---|
| 1 | `field/password`, 32 alphanumeric characters | `secretspec-fingerprint-v1\nslot=field/password\nmode=chars\nlength=32\nalphabet=ALNUM\nrequire=0,0,0,0\nprefix=\nsuffix=\n` | `6819313d1058c21d17f2c11aa042b85e5b39f5b1bb9f505d9eeb90f013bb9266` |
| 2 | `field/rpc-secret`, 32 bytes hex | `secretspec-fingerprint-v1\nslot=field/rpc-secret\nmode=bytes\ncount=32\nencoding=hex\nprefix=\nsuffix=\n` | `fdb7e7a25bf08598dd36a8ab9856b9c28b812ceee2f5153192c5886af53eb291` |
| 3 | `field/access-key-id`, 24 hex characters, prefix `GK` | `secretspec-fingerprint-v1\nslot=field/access-key-id\nmode=chars\nlength=24\nalphabet=30313233343536373839616263646566\nrequire=0,0,0,0\nprefix=474b\nsuffix=\n` | `519823d7c00a3a06bae896caddd8f78a80c114b25bc3073ee00f1bb12b8bc0cd` |
| 4 | `field/token`, 48 bytes base64url, prefix `tg6_`, suffix `-x` | `secretspec-fingerprint-v1\nslot=field/token\nmode=bytes\ncount=48\nencoding=base64url\nprefix=7467365f\nsuffix=2d78\n` | `2ba5ece6a2d18b7b89dceef4bd2bce1c1caa5146db4c83992792d501cc0f0a15` |
| 5 | `basic-auth/password`, 32 alphanumeric, require 1,2,3,4 | `secretspec-fingerprint-v1\nslot=basic-auth/password\nmode=chars\nlength=32\nalphabet=ALNUM\nrequire=1,2,3,4\nprefix=\nsuffix=\n` | `7f25ae7647044e77aa69810b6e6d48b757b10f06735cfb9c60a8b7028f3ec0ab` |
| 6 | `ssh/keypair`, ed25519 | `secretspec-fingerprint-v1\nslot=ssh/keypair\nalgorithm=ed25519\n` | `b62ed2e39a6976cc6626f17f758b49eccf548c2132f73d98ca25c2aa1f89f746` |
| 7 | `ssh/keypair`, RSA 4096 | `secretspec-fingerprint-v1\nslot=ssh/keypair\nalgorithm=rsa\nbits=4096\n` | `33454c99221f4015946a46dd2760c404084c058d7a4544047aed390bfe099940` |
| 8 | `ssh/keypair`, ECDSA P-384 | `secretspec-fingerprint-v1\nslot=ssh/keypair\nalgorithm=ecdsa\ncurve=P-384\n` | `4a877ed8174512b3202da9155390324cb7228af6d6bf9158ec3598c50c563eef` |
| 9 | `basic-auth/htpasswd`, username `admin`, password `secret`, cost 12 | `secretspec-fingerprint-v1\nslot=basic-auth/htpasswd\nusername=61646d696e\npassword-sha256=2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b\ncost=12\n` | `8e302bbe8780c93eda8fcd8d173aaf3a71632e5eb26b7a91f134902cab42998e` |

In vector 9, `2bb80d53…a25b` is `printf %s secret | shasum -a 256`.

Reproduce vector 6:

```sh
printf 'secretspec-fingerprint-v1\nslot=ssh/keypair\nalgorithm=ed25519\n' | shasum -a 256
# b62ed2e39a6976cc6626f17f758b49eccf548c2132f73d98ca25c2aa1f89f746  -
```

In vectors 1 and 5 replace `ALNUM` with the full hex string above before
hashing.
