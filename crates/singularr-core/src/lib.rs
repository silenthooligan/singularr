//! Singularr's pure logic. No IO, no async, no clock: everything here is a function of its inputs,
//! so the server, the tests and (later) the browser all get the same answer.
//!
//! - [`release`]: turn a release name into typed fields, with the evidence for each.
//! - [`policy`]: the rules model, scoring ([`policy::evaluate`]) and decisions ([`policy::decide`]).
//! - [`naming`]: read a library's file names back into fields, and write the edition
//!   part of a name for each library.
//! - [`edition`]: the closed list of editions (`rules/editions.toml`) and which edition a name
//!   belongs to.
//! - [`profile`]: quality profiles, lists and advanced rules as a person edits them, compiled
//!   into a policy, and a policy file imported into them.
//! - [`convention`]: naming conventions: templates of tokens, the presets, the names they give.
#![forbid(unsafe_code)]

pub mod convention;
pub mod edition;
pub mod naming;
pub mod policy;
pub mod profile;
pub mod release;

pub use edition::{Edition, EditionList, EditionRead};
pub use policy::{
    Action, Condition, Decision, EditionDecision, EvalContext, Lane, MediaInfo, Policy,
    PolicyError, Protection, SimulatedChange, TraceItem, Trait, TraitChange, Verdict,
};
pub use release::{
    AudioCodec, HdrFormat, MediaKind, ParseNote, ParsedRelease, Quality, Resolution, Source,
    VideoCodec,
};
