//! Settings: everything that configures Singularr, described as data. The mechanism is the
//! platform's (`singularr_platform::settings`): the sources of a value, the saved values, the
//! views and the save. The fields are the app's ([`registry`]). The app's configuration hands
//! them to the mechanism.

pub mod registry;

use crate::core::config::Config;
use singularr_platform::settings::Described;
pub use singularr_platform::settings::{
    INSTANCE, Locale, LocaleValue, PatchError, model, person, store, user_scope,
};

/// The settings of this process over the app's configuration and its fields.
pub type Settings = singularr_platform::settings::Settings<Config>;

impl Described for Config {
    fn sections() -> &'static [registry::Section] {
        registry::SECTIONS
    }

    fn instance_path(&self) -> Option<&str> {
        self.bootstrap.instance_path.as_deref()
    }
}

#[cfg(test)]
mod tests;
