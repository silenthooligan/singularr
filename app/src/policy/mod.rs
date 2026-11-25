//! The rules: the policy file `SINGULARR_POLICY` names (the product's default is
//! `rules/policy.toml`) loaded into a typed policy, or the saved model (quality profiles, lists,
//! rules) compiled into one, and simulation of a proposed one.
//!
//! The model, scoring and decisions are `singularr_core::policy`; the model a person edits and
//! its compiler are `singularr_core::profile`. This module loads the text, names it by hash,
//! remembers the versions it has seen, keeps the saved model's versions (`saved`), holds the
//! policy in force so a save puts a new one in force at once, and compares two policies over
//! the stored copies.

pub mod model;

#[cfg(feature = "ssr")]
pub mod saved;
#[cfg(feature = "ssr")]
pub mod simulate;
#[cfg(feature = "ssr")]
pub mod store;

#[cfg(feature = "ssr")]
pub use loaded::*;

#[cfg(feature = "ssr")]
mod loaded {
    use super::model::PolicyView;
    use chrono::{DateTime, Utc};
    use sha2::{Digest, Sha256};
    use singularr_core::profile::{self, Model, ModelError};
    use singularr_core::{EditionList, EvalContext, MediaInfo, ParsedRelease, Policy, PolicyError};
    use std::sync::{Arc, PoisonError, RwLock};

    /// The fallback policy compiled into the binary.
    pub const BUILTIN: &str = include_str!("builtin.toml");

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolicySource {
        File(String),
        Builtin,
        /// The saved model, compiled: its version.
        Saved(i64),
    }

    impl PolicySource {
        pub fn label(&self) -> String {
            match self {
                PolicySource::File(path) => format!("file:{path}"),
                PolicySource::Builtin => "builtin".to_string(),
                PolicySource::Saved(version) => format!("saved:{version}"),
            }
        }
    }

    /// The policy in force for this run of the program.
    #[derive(Debug, Clone)]
    pub struct ActivePolicy {
        pub policy: Policy,
        pub text: String,
        pub hash: String,
        pub source: PolicySource,
        pub loaded_at: DateTime<Utc>,
    }

    /// The policy in force, swapped whole when the saved model changes or the source does.
    /// Readers take the policy of the moment with [`PolicyCell::current`] and keep it for the
    /// length of one answer.
    #[derive(Debug)]
    pub struct PolicyCell(RwLock<Arc<ActivePolicy>>);

    pub type PolicyHandle = Arc<PolicyCell>;

    impl PolicyCell {
        pub fn handle(policy: ActivePolicy) -> PolicyHandle {
            PolicyCell::from_arc(Arc::new(policy))
        }

        pub fn from_arc(policy: Arc<ActivePolicy>) -> PolicyHandle {
            Arc::new(PolicyCell(RwLock::new(policy)))
        }

        /// The policy in force now.
        pub fn current(&self) -> Arc<ActivePolicy> {
            self.0
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// Put another policy in force.
        pub fn replace(&self, policy: ActivePolicy) {
            *self.0.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(policy);
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum LoadError {
        #[error("the policy file {path} could not be read: {message}")]
        Unreadable { path: String, message: String },
        #[error("the policy file {path} is not a valid policy: {source}")]
        Invalid {
            path: String,
            #[source]
            source: PolicyError,
        },
    }

    pub fn hash_text(text: &str) -> String {
        hex::encode(Sha256::digest(text.as_bytes()))
    }

    impl ActivePolicy {
        pub fn from_text(text: &str, source: PolicySource) -> Result<ActivePolicy, PolicyError> {
            Ok(ActivePolicy {
                policy: Policy::from_toml(text)?,
                text: text.to_string(),
                hash: hash_text(text),
                source,
                loaded_at: Utc::now(),
            })
        }

        pub fn builtin() -> ActivePolicy {
            ActivePolicy::from_text(BUILTIN, PolicySource::Builtin)
                .expect("the built-in policy is checked by a test")
        }

        /// The saved model of this version, compiled. Its text is the policy file form.
        pub fn from_model(model: &Model, version: i64) -> Result<ActivePolicy, ModelError> {
            let text = profile::export(model, &format!("v{version}"))?;
            let policy = Policy::from_toml(&text).map_err(|e| ModelError::Policy(e.to_string()))?;
            Ok(ActivePolicy {
                policy,
                hash: hash_text(&text),
                text,
                source: PolicySource::Saved(version),
                loaded_at: Utc::now(),
            })
        }

        pub fn view(&self) -> PolicyView {
            PolicyView {
                hash: self.hash.clone(),
                name: self.policy.name.clone(),
                version: self.policy.version.clone(),
                source: self.source.label(),
                loaded_at: self.loaded_at,
                policy: self.policy.clone(),
                text: self.text.clone(),
            }
        }
    }

    /// What loading found. A missing file is not an error: the built-in policy is used and
    /// `warning` says so. A file that is there but wrong is an error, because scoring the
    /// library with the fallback would rewrite every score.
    pub struct Loaded {
        pub policy: ActivePolicy,
        pub warning: Option<String>,
    }

    pub fn load(path: &str) -> Result<Loaded, LoadError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let policy = ActivePolicy::from_text(&text, PolicySource::File(path.to_string()))
                    .map_err(|source| LoadError::Invalid {
                    path: path.to_string(),
                    source,
                })?;
                Ok(Loaded {
                    policy,
                    warning: None,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Loaded {
                policy: ActivePolicy::builtin(),
                warning: Some(format!(
                    "the policy file {path} is missing; the built-in fallback policy is in force. \
                     Set SINGULARR_POLICY or add rules/policy.toml to the image."
                )),
            }),
            Err(e) => Err(LoadError::Unreadable {
                path: path.to_string(),
                message: e.to_string(),
            }),
        }
    }

    /// The edition list in force: `rules/editions.toml`, or the copy compiled into the core.
    #[derive(Debug, Clone)]
    pub struct ActiveEditions {
        pub list: EditionList,
        pub hash: String,
        pub source: PolicySource,
    }

    pub type EditionsHandle = Arc<ActiveEditions>;

    impl ActiveEditions {
        /// The list as shipped, compiled into `singularr_core`.
        pub fn shipped() -> ActiveEditions {
            ActiveEditions {
                list: EditionList::shipped().clone(),
                hash: hash_text(singularr_core::edition::SHIPPED),
                source: PolicySource::Builtin,
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum EditionsError {
        #[error("the edition list {path} could not be read: {message}")]
        Unreadable { path: String, message: String },
        #[error("the edition list {path} is not valid: {source}")]
        Invalid {
            path: String,
            #[source]
            source: singularr_core::edition::EditionError,
        },
    }

    /// Load the edition list. A missing file falls back to the shipped copy with a warning; a
    /// file that is there but wrong is an error, as for the policy.
    pub fn load_editions(path: &str) -> Result<(ActiveEditions, Option<String>), EditionsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let list =
                    EditionList::from_toml(&text).map_err(|source| EditionsError::Invalid {
                        path: path.to_string(),
                        source,
                    })?;
                Ok((
                    ActiveEditions {
                        list,
                        hash: hash_text(&text),
                        source: PolicySource::File(path.to_string()),
                    },
                    None,
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((
                ActiveEditions::shipped(),
                Some(format!(
                    "the edition list {path} is missing; the copy built into the program is in force. Set SINGULARR_EDITIONS or add rules/editions.toml to the image."
                )),
            )),
            Err(e) => Err(EditionsError::Unreadable {
                path: path.to_string(),
                message: e.to_string(),
            }),
        }
    }

    /// What is known about a candidate release beyond its name: its size and its title.
    pub fn eval_context(
        size_bytes: Option<u64>,
        title_tags: Vec<String>,
        original_language: Option<String>,
    ) -> EvalContext {
        EvalContext {
            size_bytes,
            title_tags,
            original_language,
            ..EvalContext::default()
        }
    }

    /// What the arr's probe of a file found, from the `mediaInfo` object the arr reports.
    pub fn media_from_arr(media_info: &serde_json::Value) -> Option<MediaInfo> {
        let m = media_info.as_object()?;
        let text = |key: &str| m.get(key).and_then(|v| v.as_str()).unwrap_or("");
        Some(MediaInfo::from_arr(
            text("videoCodec"),
            text("videoDynamicRangeType"),
            text("audioCodec"),
            m.get("audioChannels").and_then(|v| v.as_f64()),
            text("resolution"),
        ))
    }

    /// What is known about a held copy beyond its name: its size, its title's tags and
    /// original language, the languages the arr recorded, and the probe of the file.
    pub fn held_context(
        size_bytes: i64,
        title_tags: &[String],
        original_language: Option<&str>,
        languages: &[String],
        media_info: Option<&serde_json::Value>,
    ) -> EvalContext {
        EvalContext {
            size_bytes: u64::try_from(size_bytes).ok().filter(|n| *n > 0),
            title_tags: title_tags.to_vec(),
            original_language: original_language.map(str::to_string),
            languages: languages.to_vec(),
            media: media_info.and_then(media_from_arr),
        }
    }

    /// The lane an arr's quality profile places a title in.
    pub fn lane_for(policy: &Policy, arr: &str, profile: Option<&str>) -> Option<String> {
        policy.lane_for_arr(arr, profile?).map(|l| l.id.clone())
    }

    /// The first lane that stands for a profile of this arr, for a name that belongs to no
    /// title yet.
    pub fn first_lane_of_arr(policy: &Policy, arr: &str) -> Option<String> {
        let prefix = format!("{}:", arr.to_ascii_lowercase());
        policy
            .lanes
            .iter()
            .find(|l| {
                l.arr_profiles
                    .iter()
                    .any(|p| p.to_ascii_lowercase().starts_with(&prefix))
            })
            .map(|l| l.id.clone())
    }

    /// The arr whose profile a lane stands for.
    pub fn arr_of_lane(policy: &Policy, lane: &str) -> Option<String> {
        policy
            .lane(lane)?
            .arr_profiles
            .first()
            .and_then(|p| p.split_once(':'))
            .map(|(arr, _)| arr.to_string())
    }

    /// Ids of the protections that guard a held copy: by a tag on its title, or because the
    /// copy itself matches the protection's `held` condition. This mirrors the test
    /// `singularr_core::policy::decide` makes before it lets a candidate replace the copy.
    pub fn protections_on(policy: &Policy, held: &ParsedRelease, ctx: &EvalContext) -> Vec<String> {
        policy
            .protections
            .iter()
            .filter(|p| {
                let by_tag = p
                    .tags
                    .iter()
                    .any(|t| ctx.title_tags.iter().any(|x| x.eq_ignore_ascii_case(t)));
                let by_held = p
                    .held
                    .as_ref()
                    .is_some_and(|c| c.matches(held, ctx).is_some());
                by_tag || by_held
            })
            .map(|p| p.id.clone())
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use singularr_core::release::parse;

        #[test]
        fn the_builtin_policy_is_valid_and_has_four_lanes() {
            let p = ActivePolicy::builtin();
            assert_eq!(p.policy.lanes.len(), 4);
            for lane in ["movies-hd", "movies-uhd", "shows-hd", "shows-uhd"] {
                assert!(p.policy.lane(lane).is_some(), "lane {lane} is missing");
            }
            assert_eq!(p.hash.len(), 64);
            assert_eq!(p.view().source, "builtin");
        }

        #[test]
        fn a_missing_file_falls_back_with_a_warning() {
            let loaded = load("/nonexistent/singularr/policy.toml").unwrap();
            assert_eq!(loaded.policy.source, PolicySource::Builtin);
            assert!(loaded.warning.unwrap().contains("missing"));
        }

        #[test]
        fn a_broken_file_is_an_error_not_a_fallback() {
            let dir = std::env::temp_dir().join(format!("singularr-policy-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("policy.toml");
            std::fs::write(&path, "name = \"x\"\nversion = \"1\"\nnonsense = true\n").unwrap();
            let err = load(path.to_str().unwrap()).err().unwrap();
            assert!(matches!(err, LoadError::Invalid { .. }));
            std::fs::write(&path, BUILTIN).unwrap();
            let ok = load(path.to_str().unwrap()).unwrap();
            assert!(ok.warning.is_none());
            assert!(matches!(ok.policy.source, PolicySource::File(_)));
            assert_eq!(ok.policy.hash, hash_text(BUILTIN));
            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[test]
        fn the_edition_list_loads_from_its_file_or_falls_back() {
            let (shipped, warning) = load_editions("/nonexistent/singularr/editions.toml").unwrap();
            assert_eq!(shipped.source, PolicySource::Builtin);
            assert!(warning.unwrap().contains("missing"));
            let path = format!("{}/../rules/editions.toml", env!("CARGO_MANIFEST_DIR"));
            let (file, warning) = load_editions(&path).unwrap();
            assert!(warning.is_none());
            assert_eq!(
                file.hash, shipped.hash,
                "the file and the built-in copy are one text"
            );
            assert_eq!(file.list, shipped.list);
        }

        #[test]
        fn protections_apply_by_held_copy_and_by_tag() {
            let p = ActivePolicy::builtin();
            let ctx = EvalContext::default();
            let open = parse("Movie.2004.Open.Matte.1080p.WEB-DL.HEVC-PeruGuy");
            let plain = parse("Movie.2004.1080p.BluRay.x265-QxR");
            assert_eq!(protections_on(&p.policy, &open, &ctx), vec!["open-matte"]);
            assert!(protections_on(&p.policy, &plain, &ctx).is_empty());

            let mut tagged = p.policy.clone();
            tagged.protections[0].held = None;
            tagged.protections[0].tags = vec!["Open-Matte".to_string()];
            let ctx = EvalContext {
                title_tags: vec!["open-matte".to_string()],
                ..EvalContext::default()
            };
            assert_eq!(protections_on(&tagged, &plain, &ctx), vec!["open-matte"]);
        }
    }
}
