use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
    Claude,
    Agy,
    Fake,
}

impl AgentKind {
    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Agy => "agy",
            Self::Fake => "maestro-fake-agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Structured,
    CliManaged,
    PtyOnly,
    MaestroEmulated,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityMaturity {
    Stable,
    Experimental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityClass {
    ReadOnly,
    Mutating,
    Sensitive,
    Dangerous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub label: String,
    pub description: String,
    pub support: CapabilitySupport,
    pub maturity: CapabilityMaturity,
    pub security_class: SecurityClass,
    pub tested_version_range: Option<String>,
    pub requires_authentication: bool,
    pub fallback: Option<String>,
    pub unavailable_reason: Option<String>,
}

impl CapabilityDescriptor {
    pub fn is_available(&self) -> bool {
        self.support != CapabilitySupport::Unavailable
    }

    pub fn requires_explicit_warning(&self) -> bool {
        self.maturity == CapabilityMaturity::Experimental
            || self.security_class == SecurityClass::Dangerous
    }
}
