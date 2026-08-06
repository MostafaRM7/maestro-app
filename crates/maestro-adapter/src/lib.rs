//! Internal contract between Maestro's daemon and CLI-agent adapters.
//!
//! Adapters are daemon components. They may only reach an AI provider by
//! launching the vendor's installed CLI executable. The daemon remains the
//! owner of process admission, persistence, redaction, and UI fan-out.

mod contract;
mod fake;
mod jsonl;

pub use contract::{
    ADAPTER_CONTRACT_VERSION, AdapterCapability, AdapterConfiguration, AdapterError,
    AdapterErrorKind, AdapterEvent, AdapterEventPayload, AdapterHealth, AdapterIdentity,
    AdapterLifecycleSignal, AdapterModel, AdapterOperation, AdapterProbe, AdapterRetrySafety,
    AdapterSession, AdapterSessionSnapshot, AdapterWriterLease, AgentAdapter, ApprovalPolicy,
    AuthenticationState, CapabilityCatalog, CapabilityCatalogError, CapabilityScope,
    ConfigurationChange, ConsoleAnnotation, FeatureInvocation, FeatureResult, InputItem,
    InstallationState, PermissionDecision, PermissionResolution, ProbeRequest, ProcessLaunchSpec,
    ProcessTransport, ResumeSessionRequest, RunStopReason, SandboxPolicy, SensitiveJson,
    SensitiveText, SessionOptions, StartSessionRequest, SteeringMode, TuiLaunchPlan,
    TuiLaunchRequest, TurnInput, UserInputAnswer, UserInputResponse, VendorBinding,
    VendorRequestContext, VersionCompatibility,
};
pub use fake::FakeAdapter;
pub use jsonl::{
    BoundedJsonLineDecoder, DecodedJsonLines, JsonLineError, MAXIMUM_JSONL_FRAME_BYTES,
    SensitiveWireFrame, encode_json_line,
};
