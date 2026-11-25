//! What the settings routes send and receive: the schema with its values, and a save.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The answer of `GET /api/v1/settings`: every section with its groups and fields, each field
/// with its value in force and where that value comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SettingsView {
    pub sections: Vec<SectionView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SectionView {
    /// `general`, `integrations`, `libraries`, `people`, `portal`, `notifications`,
    /// `appearance`, `jobs`, `api`, `system`: the Settings page's sub-pages (`#tab=<id>`).
    pub id: String,
    pub label: String,
    pub summary: String,
    pub groups: Vec<GroupView>,
    /// Collections with routes of their own that the page draws in this section: `server` and
    /// `integrations` (`/integrations`), `libraries`, `people` and `roles` (`/people`), `keys`,
    /// `jobs` and `cache` (`/jobs`), `services` and `about` (`/system`).
    pub panels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GroupView {
    pub id: String,
    pub label: String,
    pub help: String,
    pub fields: Vec<FieldView>,
}

/// One choice of a `select` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OptionView {
    pub value: String,
    pub label: String,
}

/// The installation's own value of a field each person sets for themselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct InstanceValue {
    #[schema(value_type = Object)]
    pub value: Value,
    pub source: String,
    pub editable: bool,
}

/// One setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldView {
    pub key: String,
    pub label: String,
    /// `text`, `secret`, `number`, `bool`, `select`, `duration` (seconds), `url`, `path`,
    /// `list`, `timezone` or `language`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The value in force. Left out for a secret, which never leaves the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub value: Option<Value>,
    /// The value when nothing fixes or saves it. Left out for a secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub default: Option<Value>,
    /// A secret only: whether it is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<bool>,
    /// `default`, `environment`, `file` or `saved`.
    pub source: String,
    /// With `environment`: the variable that fixes the value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// With `file`: the instance file that fixes the value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The value can be saved here: nothing fixes it and a save path exists.
    pub editable: bool,
    /// A change takes effect at the next start.
    pub restart: bool,
    /// `instance`, or `user` for a value each person sets for themselves.
    pub scope: String,
    pub help: String,
    /// The value is not shown to this caller (it needs the `settings` scope).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub withheld: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<OptionView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// A `user` field: the installation's value, the default for everyone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<InstanceValue>,
    /// Where a panel shows the field: `connection` or `schedule` (an integration's fields).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// An example of a value, for an empty field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

/// `PATCH /api/v1/settings`: the values to save, by key. `null` takes a saved value away, so
/// the default (or the installation's value, for a person's own) is in force again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[schema(value_type = Object)]
    pub values: BTreeMap<String, Value>,
    /// Left out: each value where it belongs (a person's own field is the caller's own when
    /// the caller is a person). `user`: the caller's own values only. `instance`: the
    /// installation's values (a person's field then sets the default for everyone).
    #[serde(default)]
    pub scope: Option<String>,
}

/// A save that went through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SettingsSaved {
    /// The keys whose value changed, each with a `setting_changed` event in the journal.
    pub saved: Vec<String>,
    /// The keys sent with the value they already had.
    pub unchanged: Vec<String>,
    /// A saved change takes effect only at the next start.
    pub restart: bool,
}

/// A save that was refused, nothing of it stored: why, by key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SettingsRefused {
    pub errors: BTreeMap<String, String>,
}
