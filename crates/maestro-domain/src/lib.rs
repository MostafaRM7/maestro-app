//! Shared, serializable domain types used by the daemon, desktop host, and
//! adapter implementations.

mod capability;
mod error;
mod event;
mod ids;
mod permission;
mod session;

pub use capability::{
    AgentKind, CapabilityDescriptor, CapabilityMaturity, CapabilitySupport, SecurityClass,
};
pub use error::{ErrorCode, MaestroError};
pub use event::{EventEnvelope, EventSource, EventVisibility, NormalizedEvent};
pub use ids::{EventId, ProjectId, RequestId, RunId, SessionId, TerminalId, TurnId};
pub use permission::{
    PermissionEffect, PermissionEvaluation, PermissionEvaluationSource, PermissionRequestContext,
    PermissionRule, PermissionRuleCreationError, PermissionRuleDraft, PermissionRuleMatcher,
    PermissionRuleScope, RuleCreationIntent, VendorDefaultDecision, VendorPermissionPolicy,
    create_persistent_permission_rule, evaluate_permission,
};
pub use session::{IntegrationMode, SessionState, SessionTransitionError};
