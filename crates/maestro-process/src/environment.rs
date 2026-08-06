use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
};

const DEFAULT_ALLOWED: &[&str] = &[
    "COLORTERM",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOGNAME",
    "PATH",
    "SHELL",
    "SSH_AUTH_SOCK",
    "TERM",
    "TMPDIR",
    "USER",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
];

const SECRET_MARKERS: &[&str] = &["AUTH", "CREDENTIAL", "KEY", "PASSWORD", "SECRET", "TOKEN"];

/// A controlled environment policy. Deny rules always override allow rules and
/// overrides. Project `.env` loading is intentionally not part of this type.
#[derive(Debug, Clone)]
pub struct EnvironmentPolicy {
    allowed: BTreeSet<OsString>,
    denied: BTreeSet<OsString>,
    overrides: BTreeMap<OsString, OsString>,
    inherit_full: bool,
}

impl Default for EnvironmentPolicy {
    fn default() -> Self {
        Self {
            allowed: DEFAULT_ALLOWED.iter().map(OsString::from).collect(),
            denied: BTreeSet::new(),
            overrides: BTreeMap::new(),
            inherit_full: false,
        }
    }
}

impl EnvironmentPolicy {
    #[must_use]
    pub fn allow(mut self, name: impl Into<OsString>) -> Self {
        self.allowed.insert(name.into());
        self
    }

    #[must_use]
    pub fn deny(mut self, name: impl Into<OsString>) -> Self {
        self.denied.insert(name.into());
        self
    }

    #[must_use]
    pub fn override_value(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.overrides.insert(name.into(), value.into());
        self
    }

    /// Full inheritance is deliberately explicit and remains subject to deny
    /// rules. This should only be enabled by a user-facing confirmation.
    #[must_use]
    pub fn inherit_full(mut self, enabled: bool) -> Self {
        self.inherit_full = enabled;
        self
    }

    pub fn evaluate<I, K, V>(&self, source: I) -> ControlledEnvironment
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let mut values = BTreeMap::new();
        for (name, value) in source {
            let name = name.into();
            if !self.denied.contains(&name) && (self.inherit_full || self.allowed.contains(&name)) {
                values.insert(name, value.into());
            }
        }

        for (name, value) in &self.overrides {
            if !self.denied.contains(name) {
                values.insert(name.clone(), value.clone());
            }
        }

        ControlledEnvironment { values }
    }

    pub fn evaluate_current(&self) -> ControlledEnvironment {
        self.evaluate(std::env::vars_os())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlledEnvironment {
    values: BTreeMap<OsString, OsString>,
}

impl ControlledEnvironment {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn values(&self) -> &BTreeMap<OsString, OsString> {
        &self.values
    }

    pub fn insert(&mut self, name: impl Into<OsString>, value: impl Into<OsString>) {
        self.values.insert(name.into(), value.into());
    }

    pub fn preview(&self) -> Vec<EnvironmentPreview> {
        self.values
            .iter()
            .map(|(name, value)| EnvironmentPreview {
                name: name.to_string_lossy().into_owned(),
                value: masked_value(name, value),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentPreview {
    pub name: String,
    pub value: String,
}

fn masked_value(name: &OsStr, value: &OsStr) -> String {
    let upper_name = name.to_string_lossy().to_ascii_uppercase();
    if SECRET_MARKERS
        .iter()
        .any(|marker| upper_name.contains(marker))
    {
        "[REDACTED]".to_owned()
    } else {
        value.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::EnvironmentPolicy;

    #[test]
    fn default_policy_excludes_unrelated_secrets() {
        let environment = EnvironmentPolicy::default().evaluate([
            ("HOME", "/tmp/home"),
            ("PATH", "/bin"),
            ("FIXTURE_TOKEN", "do-not-inherit"),
        ]);

        assert_eq!(
            environment.values().get(OsStr::new("HOME")).unwrap(),
            "/tmp/home"
        );
        assert!(
            !environment
                .values()
                .contains_key(OsStr::new("FIXTURE_TOKEN"))
        );
    }

    #[test]
    fn deny_wins_over_full_inheritance_and_override() {
        let environment = EnvironmentPolicy::default()
            .inherit_full(true)
            .override_value("BLOCKED", "override")
            .deny("BLOCKED")
            .evaluate([("BLOCKED", "source"), ("VISIBLE", "yes")]);

        assert!(!environment.values().contains_key(OsStr::new("BLOCKED")));
        assert_eq!(
            environment.values().get(OsStr::new("VISIBLE")).unwrap(),
            "yes"
        );
    }

    #[test]
    fn preview_masks_secret_named_values() {
        let environment = EnvironmentPolicy::default()
            .allow("SERVICE_API_KEY")
            .evaluate([("SERVICE_API_KEY", "fixture-secret")]);

        assert_eq!(environment.preview()[0].value, "[REDACTED]");
    }
}
