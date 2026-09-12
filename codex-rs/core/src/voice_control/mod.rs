//! Internal foundations for server-side voice control.
//!
//! This module is intentionally not part of the public crate API. In particular, provenance and
//! authority values are trusted Core bookkeeping, not client-supplied permission claims.

mod admission;
mod lease;
mod provenance;

pub(crate) use admission::AdmissionController;
pub(crate) use admission::AdmissionRequest;
pub(crate) use admission::AdmissionTicket;
pub(crate) use admission::BeginLeaseAcquireError;
pub(crate) use admission::BeginStoppingError;
pub(crate) use admission::CommitmentState;
pub(crate) use admission::EffectBoundary;
pub(crate) use admission::FencingError;
pub(crate) use admission::IdleEpoch;
pub(crate) use admission::NotSubmittedReason;
pub(crate) use admission::SubmissionResolution;
pub(crate) use lease::LeaseGeneration;
pub(crate) use lease::VoiceLease;
pub(crate) use lease::VoiceLeaseId;
pub(crate) use lease::VoiceLeaseState;
pub(crate) use provenance::InputClass;
pub(crate) use provenance::InputEffect;
pub(crate) use provenance::InputOrigin;
pub(crate) use provenance::InputProvenance;
pub(crate) use provenance::SubmissionAuthority;
