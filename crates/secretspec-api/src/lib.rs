// SPDX-License-Identifier: Apache-2.0

//! Kubernetes API types for the operator.
//!
//! The `SecretSpec` custom resource (`secretspec.jabbas.eu/v1alpha1`) lives in
//! [`v1alpha1`]; names and defaults shared with the controller live in
//! [`constants`]. The CRD schema is generated from these types, so every
//! `///` doc comment here is user-facing documentation (`kubectl explain`).

pub mod constants;
pub mod resolve;
pub mod v1alpha1;

pub use resolve::{InvalidSpec, ResolvedSpec, resolve};
