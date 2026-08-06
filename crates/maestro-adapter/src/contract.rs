use std::{collections::HashSet, error::Error, ffi::OsString, fmt, path::PathBuf};

use async_trait::async_trait;
use maestro_domain::{
    AgentKind, CapabilityDescriptor, IntegrationMode, NormalizedEvent, ProjectId, RequestId, RunId,
    SessionId, TurnId,
};
use maestro_redaction::{redact_json, redact_text};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

/// Incremented only for breaking changes to the daemon/adapter contract.
pub const ADAPTER_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterIdentity {
    pub adapter_id: String,
    pub display_name: String,
    pub agent_kind: AgentKind,
    pub executable_names: Vec<String>,
    pub contract_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationState {
    Installed,
    NotInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationState {
    Authenticated,
    Unauthenticated,
    NotRequired,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionCompatibility {
    Tested,
    Untested,
    Unsupported,
}

/// Read-only probe input. The daemon resolves the executable so an adapter
/// never searches or invokes through a shell.
#[derive(Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub executable: PathBuf,
    pub working_directory: PathBuf,
}

impl fmt::Debug for ProbeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeRequest")
            .field("executable", &self.executable.file_name())
            .field("working_directory", &"[LOCAL PATH]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterProbe {
    pub installation: InstallationState,
    pub cli_version: Option<String>,
    pub authentication: AuthenticationState,
    pub compatibility: VersionCompatibility,
    pub preferred_mode: IntegrationMode,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityCatalog {
    pub adapter_id: String,
    pub cli_version: String,
    pub capabilities: Vec<AdapterCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityScope {
    Application,
    Project,
    Session,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterCapability {
    pub descriptor: CapabilityDescriptor,
    pub scopes: Vec<CapabilityScope>,
    /// Exact typed adapter method or generic feature endpoint used to invoke
    /// this capability.
    pub operation: String,
    /// JSON Schema for generic feature arguments. Typed operations may omit it.
    pub input_schema: Option<Value>,
    pub required_executables: Vec<String>,
}

impl fmt::Debug for AdapterCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdapterCapability")
            .field("descriptor", &self.descriptor)
            .field("scopes", &self.scopes)
            .field("operation", &self.operation)
            .field("has_input_schema", &self.input_schema.is_some())
            .field("required_executables", &self.required_executables)
            .finish()
    }
}

impl CapabilityCatalog {
    /// Checks the invariants required before a catalog is exposed to the GUI.
    ///
    /// # Errors
    ///
    /// Returns a catalog error for duplicate/empty feature identifiers or an
    /// incomplete support explanation.
    pub fn validate(&self) -> Result<(), CapabilityCatalogError> {
        if self.adapter_id.trim().is_empty() {
            return Err(CapabilityCatalogError::EmptyAdapterId);
        }
        if self.cli_version.trim().is_empty() {
            return Err(CapabilityCatalogError::EmptyCliVersion);
        }
        let mut identifiers = HashSet::with_capacity(self.capabilities.len());
        for capability in &self.capabilities {
            let descriptor = &capability.descriptor;
            if descriptor.id.trim().is_empty() {
                return Err(CapabilityCatalogError::EmptyCapabilityId);
            }
            if !identifiers.insert(descriptor.id.clone()) {
                return Err(CapabilityCatalogError::DuplicateCapability(
                    descriptor.id.clone(),
                ));
            }
            if descriptor.tested_version_range.is_none() {
                return Err(CapabilityCatalogError::MissingTestedVersionRange(
                    descriptor.id.clone(),
                ));
            }
            if !descriptor.is_available() && descriptor.unavailable_reason.is_none() {
                return Err(CapabilityCatalogError::MissingUnavailableReason(
                    descriptor.id.clone(),
                ));
            }
            if descriptor.requires_explicit_warning() && descriptor.fallback.is_none() {
                return Err(CapabilityCatalogError::MissingFallback(
                    descriptor.id.clone(),
                ));
            }
            if capability.scopes.is_empty() {
                return Err(CapabilityCatalogError::MissingScope(descriptor.id.clone()));
            }
            if capability.operation.trim().is_empty() {
                return Err(CapabilityCatalogError::MissingOperation(
                    descriptor.id.clone(),
                ));
            }
            if capability.operation == "invoke_feature"
                && !capability.scopes.contains(&CapabilityScope::Session)
                || capability.operation == "invoke_global_feature"
                    && capability.scopes.contains(&CapabilityScope::Session)
            {
                return Err(CapabilityCatalogError::InvalidOperationScope(
                    descriptor.id.clone(),
                ));
            }
            if matches!(
                capability.operation.as_str(),
                "invoke_feature" | "invoke_global_feature"
            ) && capability.input_schema.is_none()
            {
                return Err(CapabilityCatalogError::MissingInputSchema(
                    descriptor.id.clone(),
                ));
            }
            if capability
                .required_executables
                .iter()
                .any(|name| name.trim().is_empty())
            {
                return Err(CapabilityCatalogError::InvalidExecutable(
                    descriptor.id.clone(),
                ));
            }
        }
        Ok(())
    }

    pub fn capability(&self, identifier: &str) -> Option<&CapabilityDescriptor> {
        self.capabilities
            .iter()
            .find(|capability| capability.descriptor.id == identifier)
            .map(|capability| &capability.descriptor)
    }

    pub fn adapter_capability(&self, identifier: &str) -> Option<&AdapterCapability> {
        self.capabilities
            .iter()
            .find(|capability| capability.descriptor.id == identifier)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityCatalogError {
    #[error("adapter id must not be empty")]
    EmptyAdapterId,
    #[error("CLI version must not be empty")]
    EmptyCliVersion,
    #[error("capability id must not be empty")]
    EmptyCapabilityId,
    #[error("duplicate capability id: {0}")]
    DuplicateCapability(String),
    #[error("available capability {0} has no tested version range")]
    MissingTestedVersionRange(String),
    #[error("unavailable capability {0} has no explanation")]
    MissingUnavailableReason(String),
    #[error("warning-gated capability {0} has no fallback")]
    MissingFallback(String),
    #[error("capability {0} has no operation scope")]
    MissingScope(String),
    #[error("capability {0} has no adapter operation")]
    MissingOperation(String),
    #[error("generic capability {0} has a scope incompatible with its adapter operation")]
    InvalidOperationScope(String),
    #[error("generic capability {0} has no input schema")]
    MissingInputSchema(String),
    #[error("capability {0} has an empty executable prerequisite")]
    InvalidExecutable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessTransport {
    StructuredJsonLines,
    OneShot,
    Pty,
}

/// An executable launch plan. Arguments are deliberately omitted from Debug
/// because they can contain local paths or user-authored content.
#[derive(PartialEq, Eq)]
pub struct ProcessLaunchSpec {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub working_directory: PathBuf,
    pub transport: ProcessTransport,
    /// Names only. The daemon decides which values are available under its
    /// controlled environment policy.
    pub requested_environment_variables: Vec<String>,
}

impl fmt::Debug for ProcessLaunchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessLaunchSpec")
            .field("executable", &self.executable.file_name())
            .field("argument_count", &self.arguments.len())
            .field("working_directory", &"[LOCAL PATH]")
            .field("transport", &self.transport)
            .field(
                "requested_environment_variables",
                &self.requested_environment_variables,
            )
            .finish()
    }
}

/// Opaque single-writer claim held by the daemon for the complete lifetime of
/// an exact-TUI process run. Adapter-specific implementations release their
/// vendor binding in `Drop`.
pub trait AdapterWriterLease: fmt::Debug + Send + Sync {}

/// Exact-TUI launch specification plus the optional vendor-binding writer
/// lease. The daemon must retain the lease until the PTY process has
/// conclusively exited and been reaped.
#[must_use = "the daemon must retain the TUI writer lease through process reaping"]
pub struct TuiLaunchPlan {
    process: ProcessLaunchSpec,
    writer_lease: Option<Box<dyn AdapterWriterLease>>,
}

impl TuiLaunchPlan {
    pub fn new(
        process: ProcessLaunchSpec,
        writer_lease: Option<Box<dyn AdapterWriterLease>>,
    ) -> Self {
        Self {
            process,
            writer_lease,
        }
    }

    pub fn process(&self) -> &ProcessLaunchSpec {
        &self.process
    }

    /// Transfers both values to the daemon process supervisor. Keeping only
    /// the process specification would release the writer claim too early.
    pub fn into_parts(self) -> (ProcessLaunchSpec, Option<Box<dyn AdapterWriterLease>>) {
        (self.process, self.writer_lease)
    }
}

impl fmt::Debug for TuiLaunchPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TuiLaunchPlan")
            .field("process", &self.process)
            .field("has_writer_lease", &self.writer_lease.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    VendorDefault,
    Ask,
    AutomaticWithinExplicitRules,
    DangerousBypass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxPolicy {
    VendorDefault,
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

#[derive(PartialEq, Eq)]
pub struct SessionOptions {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub approval_policy: ApprovalPolicy,
    pub sandbox_policy: SandboxPolicy,
    pub collaboration_mode: Option<String>,
    pub personality: Option<String>,
    /// Adapter-specific values validated against the selected capability's
    /// versioned input schema.
    pub vendor_options: Option<SensitiveJson>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            model: None,
            reasoning_effort: None,
            approval_policy: ApprovalPolicy::Ask,
            sandbox_policy: SandboxPolicy::VendorDefault,
            collaboration_mode: None,
            personality: None,
            vendor_options: None,
        }
    }
}

impl fmt::Debug for SessionOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionOptions")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("approval_policy", &self.approval_policy)
            .field("sandbox_policy", &self.sandbox_policy)
            .field("collaboration_mode", &self.collaboration_mode)
            .field("personality", &self.personality)
            .field(
                "vendor_options",
                &self.vendor_options.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(PartialEq, Eq)]
pub struct StartSessionRequest {
    pub session_id: SessionId,
    /// Assigned by the daemon before adapter launch. The adapter never creates
    /// or reuses process-run identity.
    pub run_id: RunId,
    pub project_id: ProjectId,
    pub executable: PathBuf,
    pub working_directory: PathBuf,
    pub integration_mode: IntegrationMode,
    pub options: SessionOptions,
    pub capture_raw_protocol: bool,
}

impl fmt::Debug for StartSessionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartSessionRequest")
            .field("session_id", &self.session_id)
            .field("run_id", &self.run_id)
            .field("project_id", &self.project_id)
            .field("executable", &self.executable.file_name())
            .field("working_directory", &"[LOCAL PATH]")
            .field("integration_mode", &self.integration_mode)
            .field("options", &self.options)
            .field("capture_raw_protocol", &self.capture_raw_protocol)
            .finish()
    }
}

#[derive(PartialEq, Eq)]
pub struct ResumeSessionRequest {
    pub start: StartSessionRequest,
    pub binding: VendorBinding,
}

#[derive(PartialEq, Eq)]
pub struct TuiLaunchRequest {
    pub start: StartSessionRequest,
    /// Present when exact TUI mode must resume the same vendor conversation.
    /// The daemon must release the prior structured writer before launch.
    pub binding: Option<VendorBinding>,
}

impl fmt::Debug for TuiLaunchRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TuiLaunchRequest")
            .field("start", &self.start)
            .field("has_binding", &self.binding.is_some())
            .finish()
    }
}

impl fmt::Debug for ResumeSessionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResumeSessionRequest")
            .field("start", &self.start)
            .field("binding", &"[VENDOR BINDING]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VendorBinding {
    pub agent_kind: AgentKind,
    pub vendor_session_id: String,
}

impl fmt::Debug for VendorBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VendorBinding")
            .field("agent_kind", &self.agent_kind)
            .field("vendor_session_id", &"[VENDOR SESSION ID]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SensitiveText(String);

impl SensitiveText {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn into_inner(mut self) -> Zeroizing<String> {
        Zeroizing::new(std::mem::take(&mut self.0))
    }
}

impl fmt::Debug for SensitiveText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveText([REDACTED])")
    }
}

impl Drop for SensitiveText {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(PartialEq, Eq)]
pub struct SensitiveJson(Zeroizing<Vec<u8>>);

impl SensitiveJson {
    /// Serializes a JSON value into zeroizing storage.
    ///
    /// # Errors
    ///
    /// Returns the serializer error if the value cannot be encoded.
    pub fn from_value(value: &Value) -> Result<Self, serde_json::Error> {
        serde_json::to_vec(value).map(Zeroizing::new).map(Self)
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    pub fn into_bytes(mut self) -> Zeroizing<Vec<u8>> {
        std::mem::take(&mut self.0)
    }
}

impl fmt::Debug for SensitiveJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SensitiveJson")
            .field("bytes", &"[REDACTED]")
            .field("length", &self.0.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Text { text: SensitiveText },
    FileReference { path: PathBuf },
    ImageReference { path: PathBuf },
}

impl fmt::Debug for InputItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { .. } => formatter.write_str("Text([REDACTED])"),
            Self::FileReference { .. } => formatter.write_str("FileReference([LOCAL PATH])"),
            Self::ImageReference { .. } => formatter.write_str("ImageReference([LOCAL PATH])"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInput {
    pub items: Vec<InputItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteeringMode {
    SteerActiveTurn,
    QueueFollowUp,
}

#[derive(PartialEq, Eq)]
pub enum PermissionDecision {
    AllowOnce,
    AllowForSession,
    Deny,
    Cancel,
    VendorSpecific {
        decision_id: String,
        payload: Option<SensitiveJson>,
    },
}

impl fmt::Debug for PermissionDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllowOnce => formatter.write_str("AllowOnce"),
            Self::AllowForSession => formatter.write_str("AllowForSession"),
            Self::Deny => formatter.write_str("Deny"),
            Self::Cancel => formatter.write_str("Cancel"),
            Self::VendorSpecific { decision_id, .. } => formatter
                .debug_struct("VendorSpecific")
                .field("decision_id", decision_id)
                .field("payload", &"[REDACTED]")
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct VendorRequestContext {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: Option<TurnId>,
    pub item_id: Option<String>,
    pub vendor_request_id: String,
    pub expires_at_milliseconds: Option<i64>,
}

impl fmt::Debug for VendorRequestContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VendorRequestContext")
            .field("session_id", &self.session_id)
            .field("run_id", &self.run_id)
            .field("turn_id", &self.turn_id)
            .field(
                "item_id",
                &self.item_id.as_ref().map(|_| "[VENDOR ITEM ID]"),
            )
            .field("vendor_request_id", &"[VENDOR REQUEST ID]")
            .field("expires_at_milliseconds", &self.expires_at_milliseconds)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PermissionResolution {
    pub context: VendorRequestContext,
    pub decision: PermissionDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInputAnswer {
    pub question_id: String,
    pub values: Vec<SensitiveText>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInputResponse {
    pub context: VendorRequestContext,
    pub answers: Vec<UserInputAnswer>,
}

#[derive(PartialEq, Eq)]
pub struct FeatureInvocation {
    pub operation_id: RequestId,
    pub feature_id: String,
    arguments: SensitiveJson,
}

impl FeatureInvocation {
    /// Creates a daemon-correlated invocation with arguments held in
    /// zeroizing storage.
    ///
    /// # Errors
    ///
    /// Returns the JSON serializer error when arguments cannot be encoded.
    pub fn new(
        operation_id: RequestId,
        feature_id: impl Into<String>,
        arguments: &Value,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            operation_id,
            feature_id: feature_id.into(),
            arguments: SensitiveJson::from_value(arguments)?,
        })
    }

    pub fn argument_bytes(&self) -> &[u8] {
        self.arguments.as_bytes()
    }
}

impl fmt::Debug for FeatureInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeatureInvocation")
            .field("operation_id", &self.operation_id)
            .field("feature_id", &self.feature_id)
            .field("arguments", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureResult {
    pub operation_id: RequestId,
    pub feature_id: String,
    value: Value,
}

impl FeatureResult {
    pub fn redacted(operation_id: RequestId, feature_id: impl Into<String>, value: &Value) -> Self {
        Self {
            operation_id,
            feature_id: feature_id.into(),
            value: redact_json(value),
        }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl fmt::Debug for FeatureResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeatureResult")
            .field("operation_id", &self.operation_id)
            .field("feature_id", &self.feature_id)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterOperation {
    pub operation_id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleAnnotation {
    pub operation_id: RequestId,
    pub action: String,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterLifecycleSignal {
    Ready,
    Running,
    Interrupted,
    Completed,
    Stopped,
    Failed,
}

#[derive(Clone, PartialEq)]
pub enum AdapterEventPayload {
    Normalized(NormalizedEvent),
    Console(ConsoleAnnotation),
    Lifecycle {
        signal: AdapterLifecycleSignal,
        detail: String,
    },
}

impl fmt::Debug for AdapterEventPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normalized(event) => formatter
                .debug_struct("Normalized")
                .field("kind", &event.kind)
                .field("visibility", &event.visibility)
                .finish_non_exhaustive(),
            Self::Console(annotation) => formatter
                .debug_struct("Console")
                .field("action", &annotation.action)
                .finish_non_exhaustive(),
            Self::Lifecycle { signal, .. } => formatter
                .debug_struct("Lifecycle")
                .field("signal", signal)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdapterEvent {
    pub operation_id: Option<RequestId>,
    pub payload: AdapterEventPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSessionSnapshot {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub binding: VendorBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterHealth {
    pub healthy: bool,
    pub summary: String,
    pub fallback_mode: Option<IntegrationMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterModel {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub supported_reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
}

/// Redacted wrapper around vendor-authoritative configuration returned by the
/// CLI. Maestro may cache a normalized view but does not become its authority.
#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterConfiguration {
    value: Value,
    read_only: bool,
}

impl AdapterConfiguration {
    /// Builds the vendor-authoritative view after applying Maestro's standard
    /// structured redaction policy. Raw CLI configuration must never cross the
    /// adapter boundary through this type.
    pub fn redacted(value: &Value, read_only: bool) -> Self {
        Self {
            value: redact_json(value),
            read_only,
        }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

impl fmt::Debug for AdapterConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdapterConfiguration")
            .field("value", &"[REDACTED]")
            .field("read_only", &self.read_only)
            .finish()
    }
}

#[derive(PartialEq, Eq)]
pub struct ConfigurationChange {
    pub path: String,
    value: SensitiveJson,
}

impl ConfigurationChange {
    /// Creates a configuration mutation whose value is held in zeroizing
    /// storage until the adapter passes it to the CLI.
    ///
    /// # Errors
    ///
    /// Returns the JSON serializer error when the value cannot be encoded.
    pub fn new(path: impl Into<String>, value: &Value) -> Result<Self, serde_json::Error> {
        Ok(Self {
            path: path.into(),
            value: SensitiveJson::from_value(value)?,
        })
    }

    pub fn value_bytes(&self) -> &[u8] {
        self.value.as_bytes()
    }
}

impl fmt::Debug for ConfigurationChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigurationChange")
            .field("path", &self.path)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterErrorKind {
    InvalidRequest,
    BindingInUse,
    RequestNotActive,
    CliNotInstalled,
    AuthenticationRequired,
    UnsupportedVersion,
    UnsupportedCapability,
    Protocol,
    Process,
    Closed,
    Internal,
}

/// Whether retrying a failed adapter operation can duplicate a child-process
/// delivery. This is deliberately separate from ordinary recoverability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterRetrySafety {
    /// The operation did not attempt a single-use child delivery.
    NotApplicable,
    /// The adapter knows the request was not delivered. Retrying cannot cause
    /// a duplicate delivery.
    Safe,
    /// A partial or unacknowledged write may have reached the child. The
    /// request remains claimed until an authoritative result or expiry.
    UnsafeDeliveryUncertain,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AdapterError {
    kind: AdapterErrorKind,
    message: String,
    retry_safety: AdapterRetrySafety,
    fallback_mode: Option<IntegrationMode>,
}

impl AdapterError {
    pub fn new(kind: AdapterErrorKind, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            kind,
            message: redact_text(&message).into_owned(),
            retry_safety: AdapterRetrySafety::NotApplicable,
            fallback_mode: None,
        }
    }

    pub const fn kind(&self) -> AdapterErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn retry_safety(&self) -> AdapterRetrySafety {
        self.retry_safety
    }

    pub const fn fallback_mode(&self) -> Option<IntegrationMode> {
        self.fallback_mode
    }

    #[must_use]
    pub fn with_retry_safety(mut self, safety: AdapterRetrySafety) -> Self {
        self.retry_safety = safety;
        self
    }

    #[must_use]
    pub fn with_fallback(mut self, mode: IntegrationMode) -> Self {
        self.fallback_mode = Some(mode);
        self
    }
}

impl fmt::Debug for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdapterError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("retry_safety", &self.retry_safety)
            .field("fallback_mode", &self.fallback_mode)
            .finish()
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AdapterError {}

/// One logical vendor session. Implementations live in the daemon and must
/// preserve a single writer for the underlying CLI binding.
#[async_trait]
pub trait AdapterSession: fmt::Debug + Send + Sync {
    fn snapshot(&self) -> AdapterSessionSnapshot;

    async fn send_turn(&self, input: TurnInput) -> Result<AdapterOperation, AdapterError>;

    async fn steer_or_follow_up(
        &self,
        mode: SteeringMode,
        input: TurnInput,
    ) -> Result<AdapterOperation, AdapterError>;

    async fn interrupt(&self) -> Result<AdapterOperation, AdapterError>;

    /// Permission and input responses are single-use deliveries. An
    /// implementation must atomically claim an active request before writing,
    /// restore it only after a definite pre-delivery failure, and retain the
    /// in-flight claim when delivery is uncertain. Session, run, request, and
    /// expiry context must match. The returned error's retry safety
    /// communicates the delivery distinction to the daemon.
    async fn resolve_permission(
        &self,
        resolution: PermissionResolution,
    ) -> Result<AdapterOperation, AdapterError>;

    /// Uses the same transactional, single-use delivery rules as permission
    /// resolution.
    async fn respond_user_input(
        &self,
        response: UserInputResponse,
    ) -> Result<AdapterOperation, AdapterError>;

    async fn invoke_feature(
        &self,
        invocation: FeatureInvocation,
    ) -> Result<FeatureResult, AdapterError>;

    /// Returns the next already-normalized, redacted event. Raw vendor frames
    /// use the daemon's separately opt-in, bounded inspector path.
    async fn next_event(&self) -> Result<Option<AdapterEvent>, AdapterError>;

    /// Stops this daemon-assigned process run without closing the durable
    /// logical session. Implementations release their vendor-binding writer
    /// claim only for the matching run.
    async fn stop_run(&self, reason: RunStopReason) -> Result<(), AdapterError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStopReason {
    UserRequested,
    ApplicationQuit,
    SwitchIntegrationMode,
    DaemonShutdown,
}

/// Stable internal interface implemented by every bundled agent adapter.
#[async_trait]
pub trait AgentAdapter: fmt::Debug + Send + Sync {
    fn identity(&self) -> AdapterIdentity;

    async fn probe(&self, request: ProbeRequest) -> Result<AdapterProbe, AdapterError>;

    /// Builds the explicit feature catalog for a completed probe.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when the probed version is unsupported or the
    /// adapter's catalog violates the contract invariants.
    fn discover_capabilities(
        &self,
        probe: &AdapterProbe,
    ) -> Result<CapabilityCatalog, AdapterError>;

    async fn start_session(
        &self,
        request: StartSessionRequest,
    ) -> Result<Box<dyn AdapterSession>, AdapterError>;

    async fn resume_session(
        &self,
        request: ResumeSessionRequest,
    ) -> Result<Box<dyn AdapterSession>, AdapterError>;

    async fn launch_tui(&self, request: TuiLaunchRequest) -> Result<TuiLaunchPlan, AdapterError>;

    async fn list_models(&self, request: ProbeRequest) -> Result<Vec<AdapterModel>, AdapterError>;

    async fn read_configuration(
        &self,
        request: ProbeRequest,
    ) -> Result<AdapterConfiguration, AdapterError>;

    async fn update_configuration(
        &self,
        request: ProbeRequest,
        change: ConfigurationChange,
    ) -> Result<AdapterConfiguration, AdapterError>;

    /// Invokes a CLI-managed capability that is not scoped to a conversation,
    /// such as authentication, MCP, plugin, update, or cloud management.
    async fn invoke_global_feature(
        &self,
        request: ProbeRequest,
        invocation: FeatureInvocation,
    ) -> Result<FeatureResult, AdapterError>;

    async fn health_check(&self, request: ProbeRequest) -> Result<AdapterHealth, AdapterError>;
}

#[cfg(test)]
mod tests {
    use maestro_domain::{CapabilityMaturity, CapabilitySupport, SecurityClass};

    use super::*;

    fn capability(id: &str) -> AdapterCapability {
        AdapterCapability {
            descriptor: CapabilityDescriptor {
                id: id.to_owned(),
                label: id.to_owned(),
                description: "fixture".to_owned(),
                support: CapabilitySupport::Structured,
                maturity: CapabilityMaturity::Stable,
                security_class: SecurityClass::ReadOnly,
                tested_version_range: Some("=1.0.0".to_owned()),
                requires_authentication: false,
                fallback: None,
                unavailable_reason: None,
            },
            scopes: vec![CapabilityScope::Application],
            operation: "health_check".to_owned(),
            input_schema: None,
            required_executables: vec!["fixture".to_owned()],
        }
    }

    #[test]
    fn catalog_rejects_duplicate_feature_identifiers() {
        let catalog = CapabilityCatalog {
            adapter_id: "fixture".to_owned(),
            cli_version: "1.0.0".to_owned(),
            capabilities: vec![capability("session.start"), capability("session.start")],
        };
        assert_eq!(
            catalog.validate(),
            Err(CapabilityCatalogError::DuplicateCapability(
                "session.start".to_owned()
            ))
        );
    }

    #[test]
    fn sensitive_values_are_not_exposed_by_debug() {
        let prompt = SensitiveText::new("super-secret-prompt");
        let operation_id = RequestId::new();
        let invocation = FeatureInvocation::new(
            operation_id,
            "fixture.echo",
            &serde_json::json!({ "token": "secret-value" }),
        )
        .expect("sensitive invocation");
        let configuration = AdapterConfiguration::redacted(
            &serde_json::json!({ "token": "configuration-secret" }),
            false,
        );
        let result = FeatureResult::redacted(
            operation_id,
            "fixture.echo",
            &serde_json::json!({ "password": "result-secret" }),
        );
        let change = ConfigurationChange::new(
            "fixture.token",
            &serde_json::json!({ "token": "change-secret" }),
        )
        .expect("sensitive configuration change");
        let error = AdapterError::new(
            AdapterErrorKind::Protocol,
            "Authorization: Bearer adapter-error-secret",
        )
        .with_retry_safety(AdapterRetrySafety::UnsafeDeliveryUncertain);
        assert!(!format!("{prompt:?}").contains("super-secret-prompt"));
        assert!(!format!("{invocation:?}").contains("secret-value"));
        assert!(!format!("{configuration:?}").contains("configuration-secret"));
        assert!(
            !configuration
                .value()
                .to_string()
                .contains("configuration-secret")
        );
        assert!(!result.value().to_string().contains("result-secret"));
        assert!(!format!("{change:?}").contains("change-secret"));
        assert!(!format!("{error:?}").contains("adapter-error-secret"));
        assert!(!error.to_string().contains("adapter-error-secret"));
        assert_eq!(
            error.retry_safety(),
            AdapterRetrySafety::UnsafeDeliveryUncertain
        );
    }

    #[test]
    fn generic_capabilities_require_scope_schema_and_executable_metadata() {
        let mut generic = capability("feature.invoke");
        generic.operation = "invoke_feature".to_owned();
        generic.scopes = vec![CapabilityScope::Session];
        generic.input_schema = None;
        let mut catalog = CapabilityCatalog {
            adapter_id: "fixture".to_owned(),
            cli_version: "1.0.0".to_owned(),
            capabilities: vec![generic],
        };
        assert_eq!(
            catalog.validate(),
            Err(CapabilityCatalogError::MissingInputSchema(
                "feature.invoke".to_owned()
            ))
        );

        catalog.capabilities[0].input_schema = Some(serde_json::json!({ "type": "object" }));
        catalog.capabilities[0].scopes.clear();
        assert_eq!(
            catalog.validate(),
            Err(CapabilityCatalogError::MissingScope(
                "feature.invoke".to_owned()
            ))
        );

        catalog.capabilities[0].scopes = vec![CapabilityScope::Session];
        catalog.capabilities[0].required_executables = vec![String::new()];
        assert_eq!(
            catalog.validate(),
            Err(CapabilityCatalogError::InvalidExecutable(
                "feature.invoke".to_owned()
            ))
        );
    }
}
